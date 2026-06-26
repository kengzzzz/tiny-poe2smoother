use anyhow::{anyhow, bail, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use rayon::prelude::*;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::os::raw::{c_int, c_void};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[link(name = "libooz", kind = "static")]
extern "C" {
    fn Ooz_Decompress(src_buf: *const u8, src_len: u32, dst: *mut u8, dst_size: usize) -> i32;
    #[link_name = "_Z13CompressBlockiPhS_iiPK15CompressOptionsS_P10LRMCascade"]
    fn Ooz_CompressBlock(
        codec_id: c_int,
        src_in: *mut u8,
        dst_in: *mut u8,
        src_size: c_int,
        level: c_int,
        compressopts: *const c_void,
        src_window_base: *mut u8,
        lrm: *mut c_void,
    ) -> c_int;
    #[link_name = "_Z29GetCompressedBufferSizeNeededi"]
    fn Ooz_GetCompressedBufferSizeNeeded(size: c_int) -> c_int;
}

const BUNDLE_CHUNK_SIZE: usize = 0x40000;
const BUNDLE_FIXED_HEAD_SIZE_AFTER_PREFIX: usize = 48;
const OODLE_MERMAID_COMPRESSOR: u32 = 9;
const OODLE_COMPRESS_LEVEL: c_int = 1;

#[derive(Debug, Clone)]
pub struct BundleFile {
    pub hash: u64,
    pub bundle_index: u32,
    pub bundle_name: String,
    pub offset: u32,
    pub size: u32,
    record_pos: usize,
}

#[cfg(test)]
impl BundleFile {
    pub(crate) fn for_test(bundle_name: &str, size: u32) -> Self {
        Self {
            hash: 0,
            bundle_index: 0,
            bundle_name: bundle_name.to_string(),
            offset: 0,
            size,
            record_pos: 0,
        }
    }

    pub(crate) fn for_test_with_hash(hash: u64, bundle_name: &str, size: u32) -> Self {
        Self {
            hash,
            bundle_index: 0,
            bundle_name: bundle_name.to_string(),
            offset: 0,
            size,
            record_pos: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BundleIndex {
    raw_decompressed: Vec<u8>,
    bundles: Vec<BundleInfo>,
    hash_mode: HashMode,
    files: HashMap<u64, BundleFile>,
    file_order: Vec<u64>,
    file_count_pos: usize,
    directory_bytes_compressed: Vec<u8>,
    directories: Vec<DirectoryRecord>,
    paths: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct BundleInfo {
    name: String,
    uncompressed_size: u32,
    size_pos: usize,
}

#[derive(Debug, Clone, Copy)]
enum HashMode {
    Murmur64A,
    Fnv1A,
}

#[derive(Debug, Clone, Copy)]
struct DirectoryRecord {
    path_hash: u64,
    offset: u32,
    size: u32,
    _recursive_size: u32,
}

#[derive(Debug, Clone)]
pub struct IndexedPath {
    pub path: String,
    pub file: BundleFile,
}

#[derive(Debug, Clone)]
pub struct BundleStore {
    pub game_dir: PathBuf,
    pub bundles_dir: PathBuf,
    pub index_path: PathBuf,
}

const CACHE_MAGIC: &[u8; 4] = b"2SIC";

impl BundleStore {
    pub fn new(game_dir: impl Into<PathBuf>) -> Self {
        let game_dir = game_dir.into();
        let bundles_dir = game_dir.join("Bundles2");
        let index_path = bundles_dir.join("_.index.bin");
        Self {
            game_dir,
            bundles_dir,
            index_path,
        }
    }

    fn cache_key(&self) -> Result<Option<(u64, u128)>> {
        let meta = fs::metadata(&self.index_path)?;
        let size = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok());
        Ok(mtime.map(|d| (size, d.as_nanos())))
    }

    fn cache_path(&self) -> PathBuf {
        let base = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        base.join("tiny-poe2smoother").join("index-cache.bin")
    }

    fn read_cache(&self) -> Option<BundleIndex> {
        crate::timing!("cache_read");
        let key = self.cache_key().ok()??;
        let data = fs::read(self.cache_path()).ok()?;
        let mut c = Cursor::new(&data);
        let mut magic = [0u8; 4];
        c.read_exact(&mut magic).ok()?;
        if &magic != CACHE_MAGIC {
            return None;
        }
        let cached_size: u64 = c.read_u64::<LittleEndian>().ok()?;
        let cached_mtime: u128 = c.read_u128::<LittleEndian>().ok()?;
        if (cached_size, cached_mtime) != key {
            return None;
        }
        let rd_len = c.read_u64::<LittleEndian>().ok()? as usize;
        let mut raw_decompressed = vec![0u8; rd_len];
        c.read_exact(&mut raw_decompressed).ok()?;
        let bcount = c.read_u64::<LittleEndian>().ok()? as usize;
        let mut bundles = Vec::with_capacity(bcount);
        for _ in 0..bcount {
            let nlen = c.read_u32::<LittleEndian>().ok()? as usize;
            let mut nb = vec![0u8; nlen];
            c.read_exact(&mut nb).ok()?;
            let size_pos = c.position() as usize;
            let uncompressed_size = c.read_u32::<LittleEndian>().ok()?;
            bundles.push(BundleInfo {
                name: String::from_utf8(nb).ok()?,
                uncompressed_size,
                size_pos,
            });
        }
        let fcount = c.read_u64::<LittleEndian>().ok()? as usize;
        let mut files = HashMap::with_capacity(fcount);
        let mut file_order = Vec::with_capacity(fcount);
        for _ in 0..fcount {
            let hash = c.read_u64::<LittleEndian>().ok()?;
            let record_pos = c.read_u64::<LittleEndian>().ok()? as usize;
            let bundle_index = c.read_u32::<LittleEndian>().ok()?;
            let offset = c.read_u32::<LittleEndian>().ok()?;
            let size = c.read_u32::<LittleEndian>().ok()?;
            let bname = bundles.get(bundle_index as usize)?.name.clone();
            file_order.push(hash);
            files.insert(
                hash,
                BundleFile {
                    hash,
                    bundle_index,
                    bundle_name: bname,
                    offset,
                    size,
                    record_pos,
                },
            );
        }
        let hm_byte = c.read_u8().ok()?;
        let hash_mode = match hm_byte {
            0 => HashMode::Murmur64A,
            1 => HashMode::Fnv1A,
            _ => return None,
        };
        let file_count_pos = c.read_u64::<LittleEndian>().ok()? as usize;
        let dirlen = c.read_u64::<LittleEndian>().ok()? as usize;
        let mut directory_bytes_compressed = vec![0u8; dirlen];
        c.read_exact(&mut directory_bytes_compressed).ok()?;
        let dcount = c.read_u64::<LittleEndian>().ok()? as usize;
        let mut directories = Vec::with_capacity(dcount);
        for _ in 0..dcount {
            directories.push(DirectoryRecord {
                path_hash: c.read_u64::<LittleEndian>().ok()?,
                offset: c.read_u32::<LittleEndian>().ok()?,
                size: c.read_u32::<LittleEndian>().ok()?,
                _recursive_size: c.read_u32::<LittleEndian>().ok()?,
            });
        }
        let has_paths = c.read_u8().ok()? != 0;
        let paths = if has_paths {
            let pcount = c.read_u64::<LittleEndian>().ok()? as usize;
            let mut p = Vec::with_capacity(pcount);
            for _ in 0..pcount {
                let plen = c.read_u64::<LittleEndian>().ok()? as usize;
                let mut pb = vec![0u8; plen];
                c.read_exact(&mut pb).ok()?;
                p.push(String::from_utf8(pb).ok()?);
            }
            Some(p)
        } else {
            None
        };
        Some(BundleIndex {
            raw_decompressed,
            bundles,
            hash_mode,
            files,
            file_order,
            file_count_pos,
            directory_bytes_compressed,
            directories,
            paths,
        })
    }

    fn write_cache(&self, index: &BundleIndex) {
        let key = match self.cache_key() {
            Ok(Some(k)) => k,
            _ => return,
        };
        let mut data = Vec::new();
        data.extend_from_slice(CACHE_MAGIC);
        data.write_u64::<LittleEndian>(key.0).ok();
        data.write_u128::<LittleEndian>(key.1).ok();
        data.write_u64::<LittleEndian>(index.raw_decompressed.len() as u64)
            .ok();
        data.extend_from_slice(&index.raw_decompressed);
        data.write_u64::<LittleEndian>(index.bundles.len() as u64)
            .ok();
        for b in &index.bundles {
            data.write_u32::<LittleEndian>(b.name.len() as u32).ok();
            data.extend_from_slice(b.name.as_bytes());
            data.write_u32::<LittleEndian>(b.uncompressed_size).ok();
        }
        data.write_u64::<LittleEndian>(index.files.len() as u64)
            .ok();
        for h in &index.file_order {
            let f = &index.files[h];
            data.write_u64::<LittleEndian>(f.hash).ok();
            data.write_u64::<LittleEndian>(f.record_pos as u64).ok();
            data.write_u32::<LittleEndian>(f.bundle_index).ok();
            data.write_u32::<LittleEndian>(f.offset).ok();
            data.write_u32::<LittleEndian>(f.size).ok();
        }
        data.write_u8(match index.hash_mode {
            HashMode::Murmur64A => 0,
            HashMode::Fnv1A => 1,
        })
        .ok();
        data.write_u64::<LittleEndian>(index.file_count_pos as u64)
            .ok();
        data.write_u64::<LittleEndian>(index.directory_bytes_compressed.len() as u64)
            .ok();
        data.extend_from_slice(&index.directory_bytes_compressed);
        data.write_u64::<LittleEndian>(index.directories.len() as u64)
            .ok();
        for d in &index.directories {
            data.write_u64::<LittleEndian>(d.path_hash).ok();
            data.write_u32::<LittleEndian>(d.offset).ok();
            data.write_u32::<LittleEndian>(d.size).ok();
            data.write_u32::<LittleEndian>(d._recursive_size).ok();
        }
        if let Some(ref p) = index.paths {
            data.write_u8(1).ok();
            data.write_u64::<LittleEndian>(p.len() as u64).ok();
            for s in p {
                data.write_u64::<LittleEndian>(s.len() as u64).ok();
                data.extend_from_slice(s.as_bytes());
            }
        } else {
            data.write_u8(0).ok();
        }
        if let Some(parent) = self.cache_path().parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(self.cache_path(), &data);
    }

    pub fn clear_cache(&self) {
        let p = self.cache_path();
        if p.exists() {
            let _ = fs::remove_file(&p);
        }
    }

    pub fn open_index(&self) -> Result<BundleIndex> {
        crate::timing!("index_read");
        if let Some(cached) = self.read_cache() {
            tracing::debug!("using cached index metadata");
            return Ok(cached);
        }
        let bytes = fs::read(&self.index_path)
            .with_context(|| format!("failed to read {}", self.index_path.display()))?;
        crate::timing!("index_decompress");
        let decompressed =
            decompress_bundle(&bytes).context("failed to decompress bundle index")?;
        crate::timing!("index_parse");
        let index = BundleIndex::parse(decompressed)?;
        self.write_cache(&index);
        Ok(index)
    }

    pub fn read_file(&self, index: &BundleIndex, path: &str) -> Result<Vec<u8>> {
        let file = index
            .file_by_path(path)
            .ok_or_else(|| anyhow!("path not found in bundle index: {path}"))?;
        let bundle = self.read_bundle(&file.bundle_name)?;
        slice_file(&bundle, &file)
    }

    pub fn read_bundle(&self, bundle_name: &str) -> Result<Vec<u8>> {
        let path = self.bundle_path(bundle_name);
        let bytes =
            fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        decompress_bundle(&bytes)
            .with_context(|| format!("failed to decompress {}", path.display()))
    }

    pub fn bundle_path(&self, bundle_name: &str) -> PathBuf {
        self.bundles_dir.join(format!("{bundle_name}.bundle.bin"))
    }

    pub fn read_bundles_batch(&self, names: &[String]) -> Result<HashMap<String, Vec<u8>>> {
        names
            .par_iter()
            .map(|name| {
                self.read_bundle(name)
                    .map(|data| (name.clone(), data))
                    .with_context(|| format!("failed to read bundle batch entry: {name}"))
            })
            .collect()
    }
}

impl BundleIndex {
    #[cfg(test)]
    pub(crate) fn for_test_paths(paths: &[(&str, &str, u32)]) -> Self {
        let mut bundle_names = Vec::<String>::new();
        let mut bundles = Vec::new();
        let mut files = HashMap::with_capacity(paths.len());
        let mut file_order = Vec::with_capacity(paths.len());
        for (path, bundle_name, size) in paths {
            let bundle_index =
                if let Some(index) = bundle_names.iter().position(|name| name == bundle_name) {
                    index
                } else {
                    bundle_names.push((*bundle_name).to_string());
                    bundles.push(BundleInfo {
                        name: (*bundle_name).to_string(),
                        uncompressed_size: 0,
                        size_pos: 0,
                    });
                    bundles.len() - 1
                };
            let hash = fnv1a_bundle_hash(path);
            file_order.push(hash);
            files.insert(
                hash,
                BundleFile {
                    hash,
                    bundle_index: bundle_index as u32,
                    bundle_name: (*bundle_name).to_string(),
                    offset: 0,
                    size: *size,
                    record_pos: 0,
                },
            );
        }

        Self {
            raw_decompressed: vec![0; 4],
            bundles,
            hash_mode: HashMode::Fnv1A,
            files,
            file_order,
            file_count_pos: 4,
            directory_bytes_compressed: Vec::new(),
            directories: Vec::new(),
            paths: Some(
                paths
                    .iter()
                    .map(|(path, _, _)| (*path).to_string())
                    .collect(),
            ),
        }
    }

    pub fn parse(raw_decompressed: Vec<u8>) -> Result<Self> {
        let mut cursor = Cursor::new(raw_decompressed.as_slice());
        let bundle_count = cursor.read_u32::<LittleEndian>()? as usize;
        let mut bundles = Vec::with_capacity(bundle_count);

        for _ in 0..bundle_count {
            let len = cursor.read_u32::<LittleEndian>()? as usize;
            let mut name = vec![0; len];
            cursor.read_exact(&mut name)?;
            let size_pos = usize::try_from(cursor.position())?;
            let uncompressed_size = cursor.read_u32::<LittleEndian>()?;
            bundles.push(BundleInfo {
                name: String::from_utf8(name).context("bundle name is not UTF-8")?,
                uncompressed_size,
                size_pos,
            });
        }

        let file_count_pos = usize::try_from(cursor.position())?;
        let file_count = cursor.read_u32::<LittleEndian>()? as usize;
        let mut files = HashMap::with_capacity(file_count);
        let mut file_order = Vec::with_capacity(file_count);
        for _ in 0..file_count {
            let record_pos = usize::try_from(cursor.position())?;
            let hash = cursor.read_u64::<LittleEndian>()?;
            let bundle_index = cursor.read_u32::<LittleEndian>()?;
            let offset = cursor.read_u32::<LittleEndian>()?;
            let size = cursor.read_u32::<LittleEndian>()?;
            let bundle_name = bundles
                .get(bundle_index as usize)
                .ok_or_else(|| anyhow!("file references invalid bundle index {bundle_index}"))?
                .name
                .clone();
            file_order.push(hash);
            files.insert(
                hash,
                BundleFile {
                    hash,
                    bundle_index,
                    bundle_name,
                    offset,
                    size,
                    record_pos,
                },
            );
        }

        let directory_count = cursor.read_u32::<LittleEndian>()? as usize;
        let mut directories = Vec::with_capacity(directory_count);
        for _ in 0..directory_count {
            directories.push(DirectoryRecord {
                path_hash: cursor.read_u64::<LittleEndian>()?,
                offset: cursor.read_u32::<LittleEndian>()?,
                size: cursor.read_u32::<LittleEndian>()?,
                _recursive_size: cursor.read_u32::<LittleEndian>()?,
            });
        }
        let directory_bytes_compressed = raw_decompressed[cursor.position() as usize..].to_vec();
        let hash_mode = match directories.first().map(|dir| dir.path_hash) {
            Some(0xF42A94E69CFF42FE) => HashMode::Murmur64A,
            Some(0x07E47507B4A92E53) => HashMode::Fnv1A,
            Some(value) => bail!("unsupported index name-hash sentinel {value:#x}"),
            None => bail!("index contains no directory records"),
        };

        Ok(Self {
            raw_decompressed,
            bundles,
            hash_mode,
            files,
            file_order,
            file_count_pos,
            directory_bytes_compressed,
            directories,
            paths: None,
        })
    }

    pub fn file_by_path(&self, path: &str) -> Option<&BundleFile> {
        self.files.get(&self.name_hash(path))
    }

    pub fn ensure_paths_built(&mut self) -> Result<&[String]> {
        if self.paths.is_some() {
            return Ok(self.paths.as_ref().unwrap());
        }
        crate::timing!("dir_decompress");
        let directory_bytes = decompress_bundle(&self.directory_bytes_compressed)
            .context("failed to decompress index directory data")?;
        crate::timing!("dir_build_paths");
        let paths = build_paths_from_directories(&directory_bytes, &self.directories)?;
        self.paths = Some(paths);
        Ok(self.paths.as_ref().unwrap())
    }

    pub fn paths(&self) -> &[String] {
        self.paths.as_deref().unwrap_or(&[])
    }

    pub fn matching_paths(
        &mut self,
        prefix: &str,
        extensions: &[&str],
    ) -> Result<Vec<IndexedPath>> {
        self.matching_paths_by(|path| {
            let normalized = path.replace('\\', "/").to_ascii_lowercase();
            normalized.starts_with(prefix)
                && extensions
                    .iter()
                    .any(|extension| normalized.ends_with(extension))
        })
    }

    pub fn matching_paths_by<F>(&mut self, mut predicate: F) -> Result<Vec<IndexedPath>>
    where
        F: FnMut(&str) -> bool,
    {
        self.ensure_paths_built()?;
        let paths = self.paths.as_ref().expect("paths were just built");
        let files = &self.files;
        let hash_mode = self.hash_mode;
        let mut result = Vec::new();
        for path in paths {
            if !predicate(path) {
                continue;
            }
            if let Some(file) = files.get(&hash_path(hash_mode, path)).cloned() {
                result.push(IndexedPath {
                    path: path.clone(),
                    file,
                });
            }
        }
        Ok(result)
    }

    pub fn update_file_record(
        &mut self,
        hash: u64,
        bundle_index: u32,
        offset: u32,
        size: u32,
    ) -> Result<()> {
        let file = self
            .files
            .get_mut(&hash)
            .ok_or_else(|| anyhow!("file hash not found in index: {hash:#x}"))?;
        file.bundle_index = bundle_index;
        file.offset = offset;
        file.size = size;
        file.bundle_name = self
            .bundles
            .get(bundle_index as usize)
            .ok_or_else(|| anyhow!("invalid bundle index {bundle_index}"))?
            .name
            .clone();

        let pos = file.record_pos + 8;
        let mut writer = Cursor::new(&mut self.raw_decompressed[pos..pos + 12]);
        writer.write_u32::<LittleEndian>(bundle_index)?;
        writer.write_u32::<LittleEndian>(offset)?;
        writer.write_u32::<LittleEndian>(size)?;
        Ok(())
    }

    pub fn update_bundle_size(&mut self, bundle_index: u32, uncompressed_size: u32) -> Result<()> {
        let bundle = self
            .bundles
            .get_mut(bundle_index as usize)
            .ok_or_else(|| anyhow!("invalid bundle index {bundle_index}"))?;
        bundle.uncompressed_size = uncompressed_size;
        let mut writer =
            Cursor::new(&mut self.raw_decompressed[bundle.size_pos..bundle.size_pos + 4]);
        writer.write_u32::<LittleEndian>(uncompressed_size)?;
        Ok(())
    }

    fn name_hash(&self, path: &str) -> u64 {
        hash_path(self.hash_mode, path)
    }

    pub fn indexed_paths(&mut self) -> Result<Vec<IndexedPath>> {
        self.ensure_paths_built()?;
        let paths = self.paths.as_ref().expect("paths were just built");
        let files = &self.files;
        let hash_mode = self.hash_mode;
        let mut result = Vec::with_capacity(paths.len());
        for path in paths {
            if let Some(file) = files.get(&hash_path(hash_mode, path)).cloned() {
                result.push(IndexedPath {
                    path: path.clone(),
                    file,
                });
            }
        }
        Ok(result)
    }

    pub fn file_order_map(&self) -> HashMap<u64, usize> {
        self.file_order
            .iter()
            .enumerate()
            .map(|(index, hash)| (*hash, index))
            .collect()
    }

    pub fn packed_bytes(&self) -> Result<Vec<u8>> {
        pack_uncompressed_bundle(&self.raw_decompressed)
    }

    pub fn has_bundle_prefix(&self, prefix: &str) -> bool {
        self.bundles
            .iter()
            .any(|bundle| bundle.name.starts_with(prefix))
    }

    pub fn create_custom_bundle(&mut self) -> Result<u32> {
        let mut ordinal = 0usize;
        loop {
            let name = format!("TinyPoe2Smoother/{ordinal}");
            if !self.bundles.iter().any(|bundle| bundle.name == name) {
                let bundle_index = u32::try_from(self.bundles.len())?;
                let mut record = Vec::new();
                record.write_u32::<LittleEndian>(u32::try_from(name.len())?)?;
                record.extend_from_slice(name.as_bytes());
                let size_pos = self.file_count_pos + record.len();
                record.write_u32::<LittleEndian>(0)?;

                self.raw_decompressed.splice(
                    self.file_count_pos..self.file_count_pos,
                    record.iter().copied(),
                );
                let delta = record.len();
                self.file_count_pos += delta;
                for file in self.files.values_mut() {
                    file.record_pos += delta;
                }
                self.bundles.push(BundleInfo {
                    name,
                    uncompressed_size: 0,
                    size_pos,
                });
                let mut writer = Cursor::new(&mut self.raw_decompressed[0..4]);
                writer.write_u32::<LittleEndian>(u32::try_from(self.bundles.len())?)?;
                return Ok(bundle_index);
            }
            ordinal += 1;
        }
    }
}

pub fn apply_bundle_replacements(
    store: &BundleStore,
    index: &mut BundleIndex,
    replacements: &HashMap<String, Vec<(BundleFile, Vec<u8>)>>,
) -> Result<Vec<PathBuf>> {
    crate::timing!("apply_replacements_total");
    let mut touched = Vec::new();
    let mut generated_bundle_paths: Vec<PathBuf> = Vec::new();
    let custom_bundle_index = index.create_custom_bundle()?;
    let custom_bundle_name = index.bundles[custom_bundle_index as usize].name.clone();
    let mut custom_data = Vec::new();

    let mut edits = replacements
        .values()
        .flat_map(|items| items.iter().cloned())
        .collect::<Vec<_>>();
    let file_order = index.file_order_map();
    sort_edits_by_index_order(&mut edits, &file_order);

    for (file, replacement) in edits {
        let new_offset = custom_data.len();
        custom_data.extend_from_slice(&replacement);
        index.update_file_record(
            file.hash,
            custom_bundle_index,
            u32::try_from(new_offset)?,
            u32::try_from(replacement.len())?,
        )?;
    }
    index.update_bundle_size(custom_bundle_index, u32::try_from(custom_data.len())?)?;

    let out = pack_uncompressed_bundle(&custom_data)?;
    let custom_path = store.bundle_path(&custom_bundle_name);
    if let Some(parent) = custom_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Err(e) = atomic_write(&custom_path, &out) {
        // Clean up any previously written generated bundles
        for p in &generated_bundle_paths {
            let _ = fs::remove_file(p);
        }
        return Err(e.context(format!(
            "failed to write generated bundle at {}",
            custom_path.display()
        )));
    }
    generated_bundle_paths.push(custom_path.clone());
    touched.push(custom_path);

    let index_bytes = index.packed_bytes()?;
    if let Err(e) = atomic_write(&store.index_path, &index_bytes) {
        // Index replacement failed; remove the orphaned generated bundle
        for p in &generated_bundle_paths {
            let _ = fs::remove_file(p);
        }
        return Err(e.context("failed to replace index; generated bundle has been cleaned up"));
    }
    touched.push(store.index_path.clone());
    Ok(touched)
}

fn sort_edits_by_index_order(
    edits: &mut [(BundleFile, Vec<u8>)],
    file_order: &HashMap<u64, usize>,
) {
    edits.sort_by_key(|(file, _)| file_order.get(&file.hash).copied().unwrap_or(usize::MAX));
}

pub fn decompress_bundle(src: &[u8]) -> Result<Vec<u8>> {
    let mut cursor = Cursor::new(src);
    let _total_uncompressed_32 = cursor.read_u32::<LittleEndian>()?;
    let _total_compressed_32 = cursor.read_u32::<LittleEndian>()?;
    let head_size = cursor.read_u32::<LittleEndian>()? as usize;
    let _encoding = cursor.read_u32::<LittleEndian>()?;
    let _unknown = cursor.read_u32::<LittleEndian>()?;
    let uncompressed_size = cursor.read_u64::<LittleEndian>()? as usize;
    let _compressed_size = cursor.read_u64::<LittleEndian>()?;
    let chunk_count = cursor.read_u32::<LittleEndian>()? as usize;
    let chunk_unpacked_size = cursor.read_u32::<LittleEndian>()? as usize;
    for _ in 0..4 {
        let _ = cursor.read_u32::<LittleEndian>()?;
    }
    let mut chunk_sizes = Vec::with_capacity(chunk_count);
    for _ in 0..chunk_count {
        chunk_sizes.push(cursor.read_u32::<LittleEndian>()? as usize);
    }

    let mut offset = 12 + head_size;
    if offset < cursor.position() as usize {
        bail!("bundle head_size is smaller than chunk table");
    }
    let mut out = Vec::with_capacity(uncompressed_size);
    let mut remaining = uncompressed_size;
    for size in chunk_sizes {
        if offset + size > src.len() {
            bail!("bundle chunk exceeds source length");
        }
        let dst_size = remaining.min(chunk_unpacked_size);
        let chunk = &src[offset..offset + size];
        let mut chunk_out = vec![0; dst_size + 64];
        let wrote = unsafe {
            Ooz_Decompress(
                chunk.as_ptr(),
                u32::try_from(size)?,
                chunk_out.as_mut_ptr(),
                dst_size,
            )
        };
        if wrote < 0 {
            bail!("Oodle decompression failed with code {wrote}");
        }
        out.extend_from_slice(&chunk_out[..dst_size]);
        remaining = remaining.saturating_sub(dst_size);
        offset += size;
    }
    if out.len() != uncompressed_size {
        bail!(
            "bundle decompressed to {} bytes, expected {}",
            out.len(),
            uncompressed_size
        );
    }
    Ok(out)
}

pub fn pack_uncompressed_bundle(data: &[u8]) -> Result<Vec<u8>> {
    crate::timing!("bundle_compress");
    let chunks = data
        .par_chunks(BUNDLE_CHUNK_SIZE)
        .map(compress_chunk)
        .collect::<Result<Vec<_>>>()?;

    let compressed_len: usize = chunks.iter().map(Vec::len).sum();
    let head_size = BUNDLE_FIXED_HEAD_SIZE_AFTER_PREFIX + 4 * chunks.len();
    let total_file_size = 12 + head_size + compressed_len;
    let mut out = Vec::with_capacity(total_file_size);
    out.write_u32::<LittleEndian>(u32::try_from(data.len())?)?;
    out.write_u32::<LittleEndian>(u32::try_from(compressed_len)?)?;
    out.write_u32::<LittleEndian>(u32::try_from(head_size)?)?;
    out.write_u32::<LittleEndian>(OODLE_MERMAID_COMPRESSOR)?;
    out.write_u32::<LittleEndian>(1)?;
    out.write_u64::<LittleEndian>(data.len() as u64)?;
    out.write_u64::<LittleEndian>(compressed_len as u64)?;
    out.write_u32::<LittleEndian>(u32::try_from(chunks.len())?)?;
    out.write_u32::<LittleEndian>(BUNDLE_CHUNK_SIZE as u32)?;
    for _ in 0..4 {
        out.write_u32::<LittleEndian>(0)?;
    }
    for chunk in &chunks {
        out.write_u32::<LittleEndian>(u32::try_from(chunk.len())?)?;
    }
    for chunk in &chunks {
        out.extend_from_slice(chunk);
    }
    Ok(out)
}

fn compress_chunk(chunk: &[u8]) -> Result<Vec<u8>> {
    let capacity = unsafe { Ooz_GetCompressedBufferSizeNeeded(c_int::try_from(chunk.len())?) };
    if capacity <= 0 {
        bail!("Oodle compression reported invalid capacity {capacity}");
    }
    let mut src = chunk.to_vec();
    let mut out = vec![0; usize::try_from(capacity)?];
    let compressed_len = unsafe {
        Ooz_CompressBlock(
            c_int::try_from(OODLE_MERMAID_COMPRESSOR)?,
            src.as_mut_ptr(),
            out.as_mut_ptr(),
            c_int::try_from(src.len())?,
            OODLE_COMPRESS_LEVEL,
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if compressed_len <= 0 {
        bail!("Oodle compression failed with code {compressed_len}");
    }
    out.truncate(usize::try_from(compressed_len)?);
    Ok(out)
}

pub fn slice_file(bundle: &[u8], file: &BundleFile) -> Result<Vec<u8>> {
    let start = file.offset as usize;
    let end = start + file.size as usize;
    if end > bundle.len() {
        bail!("file slice exceeds bundle length for {}", file.bundle_name);
    }
    Ok(bundle[start..end].to_vec())
}

fn build_paths_from_directories(
    bytes: &[u8],
    directories: &[DirectoryRecord],
) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for directory in directories {
        let start = directory.offset as usize;
        let end = start + directory.size as usize;
        if end > bytes.len() {
            bail!("directory path data exceeds decompressed directory bundle");
        }
        build_paths(&bytes[start..end], &mut paths)?;
    }
    Ok(paths)
}

fn build_paths(bytes: &[u8], files: &mut Vec<String>) -> Result<()> {
    let mut cursor = Cursor::new(bytes);
    let mut generation_phase = false;
    let mut table: Vec<String> = Vec::new();

    while cursor.position() + 4 <= bytes.len() as u64 {
        let index = cursor.read_u32::<LittleEndian>()? as usize;
        if index == 0 {
            generation_phase = !generation_phase;
            if generation_phase {
                table.clear();
            }
            continue;
        }

        let suffix = read_nul_utf8(&mut cursor)?;
        let text = if index <= table.len() {
            format!("{}{}", table[index - 1], suffix)
        } else {
            suffix
        };
        if generation_phase {
            table.push(text);
        } else {
            files.push(text);
        }
    }
    Ok(())
}

fn read_nul_utf8(cursor: &mut Cursor<&[u8]>) -> Result<String> {
    let mut bytes = Vec::new();
    loop {
        let byte = cursor.read_u8()?;
        if byte == 0 {
            break;
        }
        bytes.push(byte);
    }
    String::from_utf8(bytes).context("path data is not UTF-8")
}

pub fn filepath_hash(path: &str) -> u64 {
    fnv1a_bundle_hash(path)
}

fn fnv1a_bundle_hash(path: &str) -> u64 {
    hash_fnv1a(format!("{}++", path.trim_end_matches('/').to_ascii_lowercase()).as_bytes())
}

fn hash_path(hash_mode: HashMode, path: &str) -> u64 {
    match hash_mode {
        HashMode::Murmur64A => {
            murmur_hash64a(path.trim_end_matches('/').to_ascii_lowercase().as_bytes())
        }
        HashMode::Fnv1A => fnv1a_bundle_hash(path),
    }
}

fn hash_fnv1a(data: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn murmur_hash64a(data: &[u8]) -> u64 {
    if data.is_empty() {
        return 0xF42A94E69CFF42FE;
    }
    const M: u64 = 0xC6A4A7935BD1E995;
    const R: u32 = 47;
    let mut hash = 0x1337B33Fu64 ^ ((data.len() as u64).wrapping_mul(M));
    let mut chunks = data.chunks_exact(8);
    for chunk in &mut chunks {
        let mut k = u64::from_le_bytes(chunk.try_into().expect("chunk size"));
        k = k.wrapping_mul(M);
        k ^= k >> R;
        k = k.wrapping_mul(M);
        hash ^= k;
        hash = hash.wrapping_mul(M);
    }
    let rem = chunks.remainder();
    if !rem.is_empty() {
        let mut tail = 0u64;
        for (i, byte) in rem.iter().enumerate() {
            tail |= u64::from(*byte) << (i * 8);
        }
        hash ^= tail;
        hash = hash.wrapping_mul(M);
    }
    hash ^= hash >> R;
    hash = hash.wrapping_mul(M);
    hash ^ (hash >> R)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|err| anyhow!("failed to replace {}: {}", path.display(), err.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncompressed_bundle_round_trip() {
        let data = b"hello poe2".repeat(100_000);
        let packed = pack_uncompressed_bundle(&data).unwrap();
        let unpacked = decompress_bundle(&packed).unwrap();
        assert_eq!(unpacked, data);
    }

    #[test]
    fn packed_bundle_header_matches_libbundle_layout() {
        let data = b"header check".repeat(100_000);
        let packed = pack_uncompressed_bundle(&data).unwrap();
        let mut cursor = Cursor::new(packed.as_slice());
        let uncompressed_size = cursor.read_u32::<LittleEndian>().unwrap() as usize;
        let compressed_size = cursor.read_u32::<LittleEndian>().unwrap() as usize;
        let head_size = cursor.read_u32::<LittleEndian>().unwrap() as usize;
        let compressor = cursor.read_u32::<LittleEndian>().unwrap();
        let unknown = cursor.read_u32::<LittleEndian>().unwrap();
        let uncompressed_size_long = cursor.read_u64::<LittleEndian>().unwrap() as usize;
        let compressed_size_long = cursor.read_u64::<LittleEndian>().unwrap() as usize;
        let chunk_count = cursor.read_u32::<LittleEndian>().unwrap() as usize;

        assert_eq!(uncompressed_size, data.len());
        assert_eq!(uncompressed_size_long, data.len());
        assert_eq!(compressed_size, compressed_size_long);
        assert_eq!(head_size, 48 + 4 * chunk_count);
        assert_eq!(12 + head_size + compressed_size, packed.len());
        assert_eq!(compressor, OODLE_MERMAID_COMPRESSOR);
        assert_eq!(unknown, 1);
    }

    #[test]
    fn filepath_hash_is_case_insensitive() {
        assert_eq!(
            filepath_hash("Metadata/Foo.ot"),
            filepath_hash("metadata/foo.ot")
        );
    }

    #[test]
    fn replacement_edits_sort_by_index_order_map() {
        let mut edits = vec![
            (
                BundleFile::for_test_with_hash(30, "a", 1),
                b"third".to_vec(),
            ),
            (
                BundleFile::for_test_with_hash(10, "a", 1),
                b"first".to_vec(),
            ),
            (
                BundleFile::for_test_with_hash(20, "a", 1),
                b"second".to_vec(),
            ),
        ];
        let order = HashMap::from([(10, 0), (20, 1), (30, 2)]);

        sort_edits_by_index_order(&mut edits, &order);

        let hashes = edits
            .into_iter()
            .map(|(file, _)| file.hash)
            .collect::<Vec<_>>();
        assert_eq!(hashes, vec![10, 20, 30]);
    }
}
