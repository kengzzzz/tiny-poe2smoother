use super::compression::{decompress_bundle, pack_uncompressed_bundle};
use super::hashing::{hash_path, HashMode};
use anyhow::{anyhow, bail, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::io::{Cursor, Read};

#[derive(Debug, Clone)]
pub struct BundleFile {
    pub hash: u64,
    pub bundle_index: u32,
    pub bundle_name: String,
    pub offset: u32,
    pub size: u32,
    pub(super) record_pos: usize,
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
    pub(super) raw_decompressed: Vec<u8>,
    pub(super) bundles: Vec<BundleInfo>,
    pub(super) hash_mode: HashMode,
    pub(super) files: HashMap<u64, BundleFile>,
    pub(super) file_order: Vec<u64>,
    pub(super) file_count_pos: usize,
    pub(super) directory_bytes_compressed: Vec<u8>,
    pub(super) directories: Vec<DirectoryRecord>,
    pub(super) paths: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub(super) struct BundleInfo {
    pub(super) name: String,
    pub(super) uncompressed_size: u32,
    pub(super) size_pos: usize,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DirectoryRecord {
    pub(super) path_hash: u64,
    pub(super) offset: u32,
    pub(super) size: u32,
    pub(super) _recursive_size: u32,
}

#[derive(Debug, Clone)]
pub struct IndexedPath {
    pub path: String,
    pub file: BundleFile,
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
            let hash = super::hashing::fnv1a_bundle_hash(path);
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

    pub fn create_custom_bundle(&mut self, name_prefix: &str) -> Result<u32> {
        let mut ordinal = 0usize;
        loop {
            let name = format!("{name_prefix}{ordinal}");
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
