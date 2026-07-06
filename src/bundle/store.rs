use super::compression::{decompress_bundle, decompress_bundle_slices};
use super::ggpk::{
    ggpk_file_record, ggpk_find_file, ggpk_find_pdir, ggpk_name_hash, ggpk_pdir_record,
    GgpkArchive, GgpkFileSpan, GgpkPatchPlan, GgpkPdirEntry,
};
use super::hashing::HashMode;
use super::index::{BundleFile, BundleIndex, BundleInfo, DirectoryRecord};
use crate::backup::GgpkBackupMeta;
use crate::install::{detect_install_layout, InstallLayout};
use anyhow::{anyhow, bail, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use rayon::prelude::*;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::fs;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Clone)]
pub struct BundleStore {
    pub game_dir: PathBuf,
    pub bundles_dir: PathBuf,
    pub index_path: PathBuf,
    pub layout: InstallLayout,
    pub(super) content_path: PathBuf,
    ggpk: Option<Arc<GgpkArchive>>,
}

#[derive(Debug, Clone)]
struct CacheKey {
    source: String,
    size: u64,
    mtime: u128,
}

const CACHE_MAGIC: &[u8; 4] = b"2SI2";

impl BundleStore {
    pub fn new(game_dir: impl Into<PathBuf>) -> Self {
        let game_dir = game_dir.into();
        let bundles_dir = game_dir.join("Bundles2");
        let index_path = bundles_dir.join("_.index.bin");
        let content_path = game_dir.join("Content.ggpk");
        let layout = detect_install_layout(&game_dir).unwrap_or(InstallLayout::LooseBundles);
        let ggpk = if matches!(layout, InstallLayout::ContentGgpk) {
            GgpkArchive::from_file(&content_path).ok().map(Arc::new)
        } else {
            None
        };
        Self {
            game_dir,
            bundles_dir,
            index_path,
            layout,
            content_path,
            ggpk,
        }
    }

    fn cache_key(&self) -> Result<Option<CacheKey>> {
        let (source_path, source_label) = if matches!(self.layout, InstallLayout::ContentGgpk) {
            (
                self.content_path.clone(),
                format!("{}::Bundles2/_.index.bin", self.content_path.display()),
            )
        } else if self.index_path.exists() {
            (
                self.index_path.clone(),
                self.index_path.to_string_lossy().into_owned(),
            )
        } else {
            return Ok(None);
        };
        let meta = fs::metadata(&source_path)?;
        let size = meta.len();
        Ok(meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| CacheKey {
                source: source_label,
                size,
                mtime: d.as_nanos(),
            }))
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
        let source_len = c.read_u64::<LittleEndian>().ok()? as usize;
        let mut source_bytes = vec![0u8; source_len];
        c.read_exact(&mut source_bytes).ok()?;
        let cached_source = String::from_utf8(source_bytes).ok()?;
        let cached_size: u64 = c.read_u64::<LittleEndian>().ok()?;
        let cached_mtime: u128 = c.read_u128::<LittleEndian>().ok()?;
        if cached_source != key.source || cached_size != key.size || cached_mtime != key.mtime {
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
        data.write_u64::<LittleEndian>(key.source.len() as u64).ok();
        data.extend_from_slice(key.source.as_bytes());
        data.write_u64::<LittleEndian>(key.size).ok();
        data.write_u128::<LittleEndian>(key.mtime).ok();
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
        let bytes = self.read_index_bytes()?;
        crate::timing!("index_decompress");
        let decompressed =
            decompress_bundle(&bytes).context("failed to decompress bundle index")?;
        crate::timing!("index_parse");
        let index = BundleIndex::parse(decompressed)?;
        self.write_cache(&index);
        Ok(index)
    }

    pub fn read_index_bytes(&self) -> Result<Vec<u8>> {
        self.read_storage_file("Bundles2/_.index.bin")
            .with_context(|| format!("failed to read {}", self.index_display_path()))
    }

    pub fn index_exists(&self) -> Result<bool> {
        self.storage_file_exists("Bundles2/_.index.bin")
    }

    pub fn index_display_path(&self) -> String {
        if matches!(self.layout, InstallLayout::ContentGgpk) {
            format!("{}::Bundles2/_.index.bin", self.content_path.display())
        } else if self.index_path.exists() {
            self.index_path.display().to_string()
        } else {
            self.index_path.display().to_string()
        }
    }

    pub fn read_file(&self, index: &BundleIndex, path: &str) -> Result<Vec<u8>> {
        let file = index
            .file_by_path(path)
            .ok_or_else(|| anyhow!("path not found in bundle index: {path}"))?;
        let compressed = self
            .read_storage_file_cow(&storage_bundle_path(&file.bundle_name))
            .with_context(|| {
                format!(
                    "failed to read {}",
                    self.bundle_display_path(&file.bundle_name)
                )
            })?;
        let mut slices = decompress_bundle_slices(&compressed, &[(file.offset, file.size)])
            .with_context(|| {
                format!(
                    "failed to decompress {}",
                    self.bundle_display_path(&file.bundle_name)
                )
            })?;
        Ok(slices.pop().expect("one span requested"))
    }

    pub fn read_bundle(&self, bundle_name: &str) -> Result<Vec<u8>> {
        let storage_path = storage_bundle_path(bundle_name);
        let bytes = self
            .read_storage_file_cow(&storage_path)
            .with_context(|| format!("failed to read {}", self.bundle_display_path(bundle_name)))?;
        decompress_bundle(&bytes).with_context(|| {
            format!(
                "failed to decompress {}",
                self.bundle_display_path(bundle_name)
            )
        })
    }

    pub fn bundle_path(&self, bundle_name: &str) -> PathBuf {
        self.bundles_dir.join(format!("{bundle_name}.bundle.bin"))
    }

    pub fn bundle_exists(&self, bundle_name: &str) -> Result<bool> {
        self.storage_file_exists(&storage_bundle_path(bundle_name))
    }

    pub fn bundle_display_path(&self, bundle_name: &str) -> String {
        let loose = self.bundle_path(bundle_name);
        if matches!(self.layout, InstallLayout::ContentGgpk) {
            format!(
                "{}::{}",
                self.content_path.display(),
                storage_bundle_path(bundle_name)
            )
        } else if loose.exists() {
            loose.display().to_string()
        } else {
            loose.display().to_string()
        }
    }

    /// Read the bytes of each target file, decompressing only the bundle
    /// chunks that cover them. Results are keyed by the file's index hash.
    pub fn read_files_batch(&self, files: &[BundleFile]) -> Result<HashMap<u64, Vec<u8>>> {
        let mut by_bundle: HashMap<&str, Vec<&BundleFile>> = HashMap::new();
        for file in files {
            by_bundle
                .entry(file.bundle_name.as_str())
                .or_default()
                .push(file);
        }
        let groups: Vec<_> = by_bundle.into_iter().collect();
        let per_bundle: Vec<Vec<(u64, Vec<u8>)>> = groups
            .par_iter()
            .map(|(name, files)| {
                let compressed = self
                    .read_storage_file_cow(&storage_bundle_path(name))
                    .with_context(|| {
                        format!("failed to read {}", self.bundle_display_path(name))
                    })?;
                let spans: Vec<(u32, u32)> =
                    files.iter().map(|file| (file.offset, file.size)).collect();
                let slices = decompress_bundle_slices(&compressed, &spans).with_context(|| {
                    format!("failed to decompress {}", self.bundle_display_path(name))
                })?;
                Ok(files
                    .iter()
                    .zip(slices)
                    .map(|(file, bytes)| (file.hash, bytes))
                    .collect())
            })
            .collect::<Result<_>>()?;
        Ok(per_bundle.into_iter().flatten().collect())
    }

    fn read_storage_file(&self, rel_path: &str) -> Result<Vec<u8>> {
        self.read_storage_file_cow(rel_path)
            .map(std::borrow::Cow::into_owned)
    }

    /// Read a storage file without copying when possible: the GGPK layout
    /// returns a borrowed slice of the archive mmap.
    fn read_storage_file_cow(&self, rel_path: &str) -> Result<std::borrow::Cow<'_, [u8]>> {
        if matches!(self.layout, InstallLayout::ContentGgpk) {
            let span = self.ggpk_file_span(rel_path)?.ok_or_else(|| {
                anyhow!("{} not found in {}", rel_path, self.content_path.display())
            })?;
            let ggpk = self.ggpk.as_ref().ok_or_else(|| {
                anyhow!("failed to open {}", self.content_path.display())
            })?;
            let end = span.begin + span.len;
            return ggpk
                .mmap
                .get(span.begin..end)
                .map(std::borrow::Cow::Borrowed)
                .ok_or_else(|| {
                    anyhow!(
                        "{} points outside {}",
                        rel_path,
                        self.content_path.display()
                    )
                });
        }
        let loose = self.game_dir.join(rel_path);
        fs::read(&loose)
            .map(std::borrow::Cow::Owned)
            .with_context(|| format!("failed to read {}", loose.display()))
    }

    fn storage_file_exists(&self, rel_path: &str) -> Result<bool> {
        if matches!(self.layout, InstallLayout::ContentGgpk) {
            return self.ggpk_file_span(rel_path).map(|span| span.is_some());
        }
        let loose = self.game_dir.join(rel_path);
        if loose.exists() {
            return Ok(true);
        }
        Ok(false)
    }

    fn ggpk_file_span(&self, rel_path: &str) -> Result<Option<GgpkFileSpan>> {
        ggpk_find_file(self.open_ggpk()?.as_ref(), rel_path)
    }

    fn open_ggpk(&self) -> Result<Arc<GgpkArchive>> {
        self.ggpk
            .clone()
            .ok_or_else(|| anyhow!("failed to open {}", self.content_path.display()))
    }

    pub(super) fn prepare_ggpk_patch(
        &self,
        custom_bundle_name: &str,
        custom_bundle_bytes: &[u8],
        index_bytes: &[u8],
    ) -> Result<GgpkPatchPlan> {
        let ggpk = self.open_ggpk()?;
        let bundles2 = ggpk_find_pdir(ggpk.as_ref(), "Bundles2")?.ok_or_else(|| {
            anyhow!(
                "Bundles2 directory not found in {}",
                self.content_path.display()
            )
        })?;
        let bundles2_offset_pos = bundles2.parent_offset_pos.ok_or_else(|| {
            anyhow!(
                "Bundles2 has no parent pointer in {}",
                self.content_path.display()
            )
        })?;
        if custom_bundle_name.contains('/') || custom_bundle_name.contains('\\') {
            bail!("standalone custom bundle name must be flat: {custom_bundle_name}");
        }

        let appended_start = fs::metadata(&self.content_path)?.len();
        let index_record = ggpk_file_record("_.index.bin", index_bytes, ggpk.use_utf32())?;
        let custom_file_name = format!("{custom_bundle_name}.bundle.bin");
        let custom_record = ggpk_file_record(
            &custom_file_name,
            custom_bundle_bytes,
            ggpk.use_utf32(),
        )?;
        let index_offset = appended_start;
        let custom_offset = index_offset + u64::try_from(index_record.len())?;
        let new_bundles2_offset = custom_offset + u64::try_from(custom_record.len())?;

        let mut entries = bundles2.entries.clone();
        let mut replaced_index = false;
        for entry in &mut entries {
            if entry.name.eq_ignore_ascii_case("_.index.bin") {
                entry.offset = index_offset;
                replaced_index = true;
            }
        }
        if !replaced_index {
            bail!(
                "Bundles2/_.index.bin is missing from {}",
                self.content_path.display()
            );
        }
        entries.push(GgpkPdirEntry {
            name: custom_file_name.clone(),
            name_hash: ggpk_name_hash(&custom_file_name),
            offset: custom_offset,
        });
        entries.sort_by_key(|entry| entry.name_hash);

        let new_bundles2 = ggpk_pdir_record(
            &bundles2.name,
            &bundles2.digest,
            &entries,
            ggpk.use_utf32(),
        )?;

        let mut append_bytes = Vec::new();
        append_bytes.extend_from_slice(&index_record);
        append_bytes.extend_from_slice(&custom_record);
        append_bytes.extend_from_slice(&new_bundles2);
        let appended_end = appended_start + u64::try_from(append_bytes.len())?;

        Ok(GgpkPatchPlan {
            backup: GgpkBackupMeta {
                bundles2_offset_pos,
                original_bundles2_offset: bundles2.offset,
                appended_start,
                appended_end,
            },
            append_bytes,
            new_bundles2_offset,
        })
    }

    pub(super) fn apply_ggpk_patch(&self, plan: &GgpkPatchPlan) -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.content_path)
            .with_context(|| format!("failed to open {}", self.content_path.display()))?;
        let current_len = file.metadata()?.len();
        if current_len != plan.backup.appended_start {
            bail!(
                "{} changed while preparing patch; verify game files and retry",
                self.content_path.display()
            );
        }

        file.seek(SeekFrom::Start(plan.backup.appended_start))?;
        file.write_all(&plan.append_bytes)?;
        file.flush()?;
        file.sync_all()?;

        file.seek(SeekFrom::Start(plan.backup.bundles2_offset_pos))?;
        file.write_u64::<LittleEndian>(plan.new_bundles2_offset)?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    }

    pub fn restore_ggpk_backup(&mut self, meta: GgpkBackupMeta) -> Result<()> {
        self.ggpk = None;
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.content_path)
            .with_context(|| format!("failed to open {}", self.content_path.display()))?;
        file.seek(SeekFrom::Start(meta.bundles2_offset_pos))?;
        file.write_u64::<LittleEndian>(meta.original_bundles2_offset)?;
        file.flush()?;
        file.sync_all()?;

        if file.metadata()?.len() == meta.appended_end {
            file.set_len(meta.appended_start)?;
            file.sync_all()?;
        }
        Ok(())
    }

    pub fn remove_legacy_overlay_files(&self) -> Result<()> {
        match fs::remove_file(&self.index_path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to remove {}", self.index_path.display()));
            }
        }

        let custom_bundle_dir = self.bundles_dir.join("TinyPoe2Smoother");
        match fs::remove_dir_all(&custom_bundle_dir) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to remove generated bundle directory {}",
                        custom_bundle_dir.display()
                    )
                });
            }
        }
        Ok(())
    }
}

pub(super) fn storage_bundle_path(bundle_name: &str) -> String {
    format!("Bundles2/{bundle_name}.bundle.bin")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;

    #[test]
    fn content_ggpk_reader_ignores_loose_overlay_index() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        fs::create_dir_all(&game).unwrap();
        write_test_ggpk(
            &game.join("Content.ggpk"),
            "Bundles2/_.index.bin",
            b"ggpk-index",
        );

        let ggpk = GgpkArchive::from_file(&game.join("Content.ggpk")).unwrap();
        let span = ggpk_find_file(&ggpk, "Bundles2/_.index.bin")
            .unwrap()
            .unwrap();
        assert_eq!(&ggpk.mmap[span.begin..span.begin + span.len], b"ggpk-index");

        let store = BundleStore::new(&game);
        assert_eq!(store.read_index_bytes().unwrap(), b"ggpk-index");

        fs::create_dir_all(game.join("Bundles2")).unwrap();
        fs::write(game.join("Bundles2/_.index.bin"), b"loose-index").unwrap();
        let store = BundleStore::new(&game);
        assert_eq!(store.read_index_bytes().unwrap(), b"ggpk-index");
    }

    #[test]
    fn ggpk_patch_appends_records_and_restores_pointer() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        fs::create_dir_all(&game).unwrap();
        let content = game.join("Content.ggpk");
        write_test_ggpk_files(
            &content,
            "Bundles2",
            &[
                ("_.index.bin", b"old-index".as_slice()),
                ("Base.bundle.bin", b"old-bundle".as_slice()),
            ],
        );
        let original_len = fs::metadata(&content).unwrap().len();

        let store = BundleStore::new(&game);
        let plan = store
            .prepare_ggpk_patch("TinyPoe2Smoother_0", b"new-bundle", b"new-index")
            .unwrap();
        assert_eq!(plan.backup.appended_start, original_len);
        store.apply_ggpk_patch(&plan).unwrap();

        let mut patched = BundleStore::new(&game);
        assert_eq!(patched.read_index_bytes().unwrap(), b"new-index");
        assert_eq!(
            patched
                .read_storage_file("Bundles2/TinyPoe2Smoother_0.bundle.bin")
                .unwrap(),
            b"new-bundle"
        );
        assert!(!game.join("Bundles2/_.index.bin").exists());

        patched.restore_ggpk_backup(plan.backup).unwrap();
        assert!(patched.ggpk.is_none());
        let restored = BundleStore::new(&game);
        assert_eq!(restored.read_index_bytes().unwrap(), b"old-index");
        assert_eq!(fs::metadata(&content).unwrap().len(), original_len);
    }

    fn write_test_ggpk(path: &Path, file_path: &str, data: &[u8]) {
        let (dir_name, file_name) = file_path.split_once('/').unwrap();
        write_test_ggpk_files(path, dir_name, &[(file_name, data)]);
    }

    fn write_test_ggpk_files(path: &Path, dir_name: &str, files: &[(&str, &[u8])]) {
        let root_pdir_size = pdir_record_size("", 1);
        let dir_pdir_size = pdir_record_size(dir_name, files.len());
        let root_offset = 20u64;
        let dir_offset = root_offset + root_pdir_size as u64;
        let mut file_offsets = Vec::with_capacity(files.len());
        let mut file_records = Vec::with_capacity(files.len());
        let mut next_offset = dir_offset + dir_pdir_size as u64;
        for (name, data) in files {
            let record = file_record(name, data);
            file_offsets.push(next_offset);
            next_offset += record.len() as u64;
            file_records.push(record);
        }

        let mut out = Vec::new();
        out.write_u32::<LittleEndian>(20).unwrap();
        out.extend_from_slice(b"GGPK");
        out.write_u32::<LittleEndian>(3).unwrap();
        out.write_u64::<LittleEndian>(root_offset).unwrap();
        write_pdir_record(&mut out, "", &[(dir_name, dir_offset)]);
        let children = files
            .iter()
            .zip(file_offsets)
            .map(|((name, _), offset)| (*name, offset))
            .collect::<Vec<_>>();
        write_pdir_record(&mut out, dir_name, &children);
        for record in file_records {
            out.extend_from_slice(&record);
        }

        fs::write(path, out).unwrap();
    }

    fn pdir_record_size(name: &str, entries: usize) -> usize {
        4 + 4 + 4 + 4 + 32 + utf16_nul_bytes(name).len() + entries * 12
    }

    fn write_pdir_record(out: &mut Vec<u8>, name: &str, children: &[(&str, u64)]) {
        out.write_u32::<LittleEndian>(pdir_record_size(name, children.len()) as u32)
            .unwrap();
        out.extend_from_slice(b"PDIR");
        out.write_u32::<LittleEndian>((name.len() + 1) as u32)
            .unwrap();
        out.write_u32::<LittleEndian>(children.len() as u32)
            .unwrap();
        out.extend_from_slice(&[0; 32]);
        out.extend_from_slice(&utf16_nul_bytes(name));
        for (child_name, offset) in children {
            out.write_u32::<LittleEndian>(ggpk_name_hash(child_name))
                .unwrap();
            out.write_u64::<LittleEndian>(*offset).unwrap();
        }
    }

    fn file_record(name: &str, data: &[u8]) -> Vec<u8> {
        let name_bytes = utf16_nul_bytes(name);
        let mut out = Vec::new();
        out.write_u32::<LittleEndian>((4 + 4 + 4 + 32 + name_bytes.len() + data.len()) as u32)
            .unwrap();
        out.extend_from_slice(b"FILE");
        out.write_u32::<LittleEndian>((name.len() + 1) as u32)
            .unwrap();
        out.extend_from_slice(&[0; 32]);
        out.extend_from_slice(&name_bytes);
        out.extend_from_slice(data);
        out
    }

    fn utf16_nul_bytes(text: &str) -> Vec<u8> {
        text.encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect()
    }
}
