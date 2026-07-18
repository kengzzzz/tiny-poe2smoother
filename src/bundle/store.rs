use super::compression::{decompress_bundle, decompress_bundle_slices};
use super::ggpk::{
    ggpk_file_record, ggpk_find_file, ggpk_find_pdir, ggpk_name_hash, ggpk_pdir_record,
    GgpkArchive, GgpkFileSpan, GgpkPatchPlan, GgpkPdirEntry,
};
use super::hashing::{hash_fnv1a, HashMode};
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
    cache_source: String,
    cache_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheKey {
    source: String,
    size: u64,
    mtime: u128,
}

const CACHE_MAGIC: &[u8; 4] = b"2SI4";
const CACHE_MIGRATION_MARKER: &str = "index-cache-v3.migrated";

fn default_cache_dir() -> PathBuf {
    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("tiny-poe2smoother")
}

fn normalized_cache_game_dir(game_dir: &std::path::Path) -> PathBuf {
    // Keep this cache-only: Windows canonical paths may use a `\\?\` prefix,
    // which should not leak into the user-visible/persisted game directory.
    fs::canonicalize(game_dir)
        .or_else(|_| std::path::absolute(game_dir))
        .unwrap_or_else(|_| game_dir.to_path_buf())
}

fn cache_source(game_dir: &std::path::Path, layout: InstallLayout) -> String {
    let game_dir = normalized_cache_game_dir(game_dir);
    match layout {
        InstallLayout::LooseBundles => game_dir
            .join("Bundles2")
            .join("_.index.bin")
            .to_string_lossy()
            .into_owned(),
        InstallLayout::ContentGgpk => format!(
            "{}::Bundles2/_.index.bin",
            game_dir.join("Content.ggpk").display()
        ),
    }
}

fn is_legacy_cache_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if name == "index-cache.bin" {
        return true;
    }
    ["index-cache-", "index-cache-v2-"]
        .into_iter()
        .filter_map(|prefix| name.strip_prefix(prefix))
        .filter_map(|name| name.strip_suffix(".bin"))
        .any(|tag| {
            tag.len() == 16
                && tag
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn retire_legacy_caches(cache_dir: &std::path::Path) {
    retire_legacy_caches_with(cache_dir, |path| fs::remove_file(path));
}

fn retire_legacy_caches_with(
    cache_dir: &std::path::Path,
    remove: impl Fn(&std::path::Path) -> std::io::Result<()>,
) {
    // The marker makes this a one-time, app-wide sweep. Exact legacy matching
    // deliberately excludes v3, temporary, and unrelated files.
    let marker = cache_dir.join(CACHE_MIGRATION_MARKER);
    if marker.exists() || fs::create_dir_all(cache_dir).is_err() {
        return;
    }
    let Ok(entries) = fs::read_dir(cache_dir) else {
        return;
    };
    let mut complete = true;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                complete = false;
                continue;
            }
        };
        if !is_legacy_cache_name(&entry.file_name()) {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                complete = false;
                continue;
            }
        };
        if !file_type.is_file() {
            continue;
        }
        match remove(&entry.path()) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => complete = false,
        }
    }
    if complete {
        let _ = fs::write(marker, b"");
    }
}

/// Validate a length/count read from the cache against the bytes actually
/// remaining, so a truncated or corrupt file is rejected instead of aborting
/// on a huge allocation. `unit` is the minimum encoded size of one element.
fn checked_count(c: &Cursor<&[u8]>, count: u64, unit: u64) -> Option<usize> {
    let remaining = (c.get_ref().len() as u64).saturating_sub(c.position());
    (count.checked_mul(unit)? <= remaining).then_some(count as usize)
}

fn parse_index_cache(data: &[u8], key: &CacheKey) -> Option<BundleIndex> {
    let mut c = Cursor::new(data);
    let mut magic = [0u8; 4];
    c.read_exact(&mut magic).ok()?;
    if &magic != CACHE_MAGIC {
        return None;
    }
    let source_len = c.read_u64::<LittleEndian>().ok()?;
    let source_len = checked_count(&c, source_len, 1)?;
    let mut source_bytes = vec![0u8; source_len];
    c.read_exact(&mut source_bytes).ok()?;
    let cached_source = String::from_utf8(source_bytes).ok()?;
    let cached_size: u64 = c.read_u64::<LittleEndian>().ok()?;
    let cached_mtime: u128 = c.read_u128::<LittleEndian>().ok()?;
    if cached_source != key.source || cached_size != key.size || cached_mtime != key.mtime {
        return None;
    }
    let rd_len = c.read_u64::<LittleEndian>().ok()?;
    let rd_len = checked_count(&c, rd_len, 1)?;
    let mut raw_decompressed = vec![0u8; rd_len];
    c.read_exact(&mut raw_decompressed).ok()?;
    let bcount = c.read_u64::<LittleEndian>().ok()?;
    // name length (u32) + uncompressed_size (u32)
    let bcount = checked_count(&c, bcount, 8)?;
    let mut bundles = Vec::with_capacity(bcount);
    for _ in 0..bcount {
        let nlen = c.read_u32::<LittleEndian>().ok()?;
        let nlen = checked_count(&c, u64::from(nlen), 1)?;
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
    let fcount = c.read_u64::<LittleEndian>().ok()?;
    // hash + record_pos (u64 each) + bundle_index + offset + size (u32 each)
    let fcount = checked_count(&c, fcount, 28)?;
    let mut files = HashMap::with_capacity(fcount);
    let mut file_order = Vec::with_capacity(fcount);
    for _ in 0..fcount {
        let hash = c.read_u64::<LittleEndian>().ok()?;
        let record_pos = c.read_u64::<LittleEndian>().ok()? as usize;
        let bundle_index = c.read_u32::<LittleEndian>().ok()?;
        let offset = c.read_u32::<LittleEndian>().ok()?;
        let size = c.read_u32::<LittleEndian>().ok()?;
        if bundle_index as usize >= bundles.len() {
            return None;
        }
        file_order.push(hash);
        files.insert(
            hash,
            BundleFile {
                hash,
                bundle_index,
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
    let dirlen = c.read_u64::<LittleEndian>().ok()?;
    let dirlen = checked_count(&c, dirlen, 1)?;
    let mut directory_bytes_compressed = vec![0u8; dirlen];
    c.read_exact(&mut directory_bytes_compressed).ok()?;
    let dcount = c.read_u64::<LittleEndian>().ok()?;
    // path_hash (u64) + offset + size + recursive_size (u32 each)
    let dcount = checked_count(&c, dcount, 20)?;
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
        let pcount = c.read_u64::<LittleEndian>().ok()?;
        // length prefix (u64) per path
        let pcount = checked_count(&c, pcount, 8)?;
        let mut p = Vec::with_capacity(pcount);
        for _ in 0..pcount {
            let plen = c.read_u64::<LittleEndian>().ok()?;
            let plen = checked_count(&c, plen, 1)?;
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

fn write_cache_file(
    path: &std::path::Path,
    tmp: &std::path::Path,
    data: &[u8],
    write: impl FnOnce(&std::path::Path, &[u8]) -> std::io::Result<()>,
) {
    let result = write(tmp, data).and_then(|_| fs::rename(tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
}

impl BundleStore {
    pub fn new(game_dir: impl Into<PathBuf>) -> Self {
        Self::new_with_cache_dir(game_dir.into(), default_cache_dir())
    }

    fn new_with_cache_dir(game_dir: PathBuf, cache_dir: PathBuf) -> Self {
        let bundles_dir = game_dir.join("Bundles2");
        let index_path = bundles_dir.join("_.index.bin");
        let content_path = game_dir.join("Content.ggpk");
        let layout = detect_install_layout(&game_dir).unwrap_or(InstallLayout::LooseBundles);
        let cache_source = cache_source(&game_dir, layout);
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
            cache_source,
            cache_dir,
        }
    }

    fn cache_key(&self) -> Result<Option<CacheKey>> {
        let source_path = if matches!(self.layout, InstallLayout::ContentGgpk) {
            &self.content_path
        } else if self.index_path.exists() {
            &self.index_path
        } else {
            return Ok(None);
        };
        let meta = fs::metadata(source_path)?;
        let size = meta.len();
        Ok(meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| CacheKey {
                source: self.cache_source.clone(),
                size,
                mtime: d.as_nanos(),
            }))
    }

    fn cache_path(&self) -> PathBuf {
        // Scoped per install: a shared file would let a second install (or a
        // test suite's temp store) clobber or clear the real install's cache.
        // Unlike bundle-path hashing, cache identity must preserve path case.
        let tag = hash_fnv1a(self.cache_source.as_bytes());
        self.cache_dir
            .join(format!("index-cache-v3-{tag:016x}.bin"))
    }

    /// Refresh storage that is held open across index and bundle reads, then
    /// return the metadata key for that exact snapshot. In particular, a GGPK
    /// pathname may be replaced while an older memory map remains readable.
    fn refresh_source_snapshot(&mut self) -> Result<Option<CacheKey>> {
        let key_before = self.cache_key()?;
        if matches!(self.layout, InstallLayout::ContentGgpk) {
            let ggpk = GgpkArchive::from_file(&self.content_path)?;
            if self.cache_key()? != key_before {
                bail!(
                    "game data changed while opening Content.ggpk; \
                     wait for the standalone launcher to finish updating, then retry"
                );
            }
            self.ggpk = Some(Arc::new(ggpk));
        }
        Ok(key_before)
    }

    fn ensure_source_unchanged(
        &self,
        source_key: &Option<CacheKey>,
        phase: &str,
    ) -> Result<()> {
        if &self.cache_key()? != source_key {
            bail!(
                "game data changed while {phase} the bundle index; \
                 wait for Steam or the standalone launcher to finish updating, then retry"
            );
        }
        Ok(())
    }

    fn read_cache(&self) -> Option<BundleIndex> {
        crate::timing!("cache_read");
        retire_legacy_caches(&self.cache_dir);
        let key_before = self.cache_key().ok()??;
        let data = fs::read(self.cache_path()).ok()?;
        let index = parse_index_cache(&data, &key_before)?;
        let key_after = self.cache_key().ok()??;
        (key_before == key_after).then_some(index)
    }

    fn write_cache(&self, index: &BundleIndex, source_key: &CacheKey) {
        retire_legacy_caches(&self.cache_dir);
        if self.cache_key().ok().flatten().as_ref() != Some(source_key) {
            return;
        }
        let mut data = Vec::new();
        data.extend_from_slice(CACHE_MAGIC);
        data.write_u64::<LittleEndian>(source_key.source.len() as u64)
            .ok();
        data.extend_from_slice(source_key.source.as_bytes());
        data.write_u64::<LittleEndian>(source_key.size).ok();
        data.write_u128::<LittleEndian>(source_key.mtime).ok();
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
        // Serializing a large live index can take seconds. If the game updater
        // changed the source meanwhile, do not publish this stale snapshot.
        if self.cache_key().ok().flatten().as_ref() != Some(source_key) {
            return;
        }
        let path = self.cache_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        // Write-then-rename so concurrent readers never observe a torn
        // cache file (a failed parse silently forces a full index rebuild).
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        write_cache_file(&path, &tmp, &data, |tmp, data| fs::write(tmp, data));
    }

    pub fn clear_cache(&self) {
        retire_legacy_caches(&self.cache_dir);
        let p = self.cache_path();
        if p.exists() {
            let _ = fs::remove_file(&p);
        }
    }

    pub fn open_index(&mut self) -> Result<BundleIndex> {
        crate::timing!("index_read");
        let source_key_before = self.refresh_source_snapshot()?;
        if let Some(cached) = self.read_cache() {
            self.ensure_source_unchanged(&source_key_before, "loading")?;
            tracing::debug!("using cached index metadata");
            return Ok(cached);
        }
        // Cache miss. Rebuilds are expensive (full decompress + path build
        // across all cores), and several background threads may open the
        // index at once, so collapse concurrent misses into one rebuild;
        // late arrivals re-read the cache the winner just wrote.
        static REBUILD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = REBUILD_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source_key_before = self.refresh_source_snapshot()?;
        if let Some(cached) = self.read_cache() {
            self.ensure_source_unchanged(&source_key_before, "loading")?;
            tracing::debug!("using index metadata cached by concurrent rebuild");
            return Ok(cached);
        }
        let bytes = self.read_index_bytes()?;
        crate::timing!("index_decompress");
        let decompressed =
            decompress_bundle(&bytes).context("failed to decompress bundle index")?;
        crate::timing!("index_parse");
        let mut index = BundleIndex::parse(decompressed)?;
        // Build paths before caching so cache hits skip dir_decompress +
        // build_paths, the dominant cost of a warm launch.
        index.ensure_paths_built()?;
        self.ensure_source_unchanged(&source_key_before, "building")?;
        if let Some(source_key) = source_key_before.as_ref() {
            self.write_cache(&index, source_key);
        }
        self.ensure_source_unchanged(&source_key_before, "caching")?;
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
        } else {
            self.index_path.display().to_string()
        }
    }

    pub fn read_file(&self, index: &BundleIndex, path: &str) -> Result<Vec<u8>> {
        let file = index
            .file_by_path(path)
            .ok_or_else(|| anyhow!("path not found in bundle index: {path}"))?;
        let bundle_name = index.bundle_name(file.bundle_index)?;
        let compressed = self
            .read_storage_file_cow(&storage_bundle_path(bundle_name))
            .with_context(|| format!("failed to read {}", self.bundle_display_path(bundle_name)))?;
        let mut slices = decompress_bundle_slices(&compressed, &[(file.offset, file.size)])
            .with_context(|| {
                format!(
                    "failed to decompress {}",
                    self.bundle_display_path(bundle_name)
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
        } else {
            loose.display().to_string()
        }
    }

    /// Read the bytes of each target file, decompressing only the bundle
    /// chunks that cover them. Results are keyed by the file's index hash.
    pub fn read_files_batch(
        &self,
        index: &BundleIndex,
        files: &[BundleFile],
    ) -> Result<HashMap<u64, Vec<u8>>> {
        let mut by_bundle: HashMap<u32, Vec<&BundleFile>> = HashMap::new();
        for file in files {
            by_bundle.entry(file.bundle_index).or_default().push(file);
        }
        let groups: Vec<_> = by_bundle.into_iter().collect();
        let per_bundle: Vec<Vec<(u64, Vec<u8>)>> = groups
            .par_iter()
            .map(|(bundle_index, files)| {
                let name = index.bundle_name(*bundle_index)?;
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
            let ggpk = self
                .ggpk
                .as_ref()
                .ok_or_else(|| anyhow!("failed to open {}", self.content_path.display()))?;
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
        Ok(loose.exists())
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
        let custom_record =
            ggpk_file_record(&custom_file_name, custom_bundle_bytes, ggpk.use_utf32())?;
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

        let new_bundles2 =
            ggpk_pdir_record(&bundles2.name, &bundles2.digest, &entries, ggpk.use_utf32())?;

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

    fn write_test_install(game: &Path, layout: InstallLayout) {
        fs::create_dir_all(game).unwrap();
        match layout {
            InstallLayout::LooseBundles => {
                fs::create_dir_all(game.join("Bundles2")).unwrap();
                fs::write(game.join("Bundles2/_.index.bin"), b"index").unwrap();
            }
            InstallLayout::ContentGgpk => {
                fs::write(game.join("Content.ggpk"), b"ggpk").unwrap();
            }
        }
    }

    #[test]
    fn corrupt_index_cache_is_rejected_without_huge_allocations() {
        let key = CacheKey {
            source: "src".to_string(),
            size: 1,
            mtime: 2,
        };
        // Truncated header and wrong magic.
        assert!(parse_index_cache(b"2SI2", &key).is_none());
        assert!(parse_index_cache(b"XXXXXXXXXXXXXXXX", &key).is_none());

        // Valid magic + matching key, then corrupt lengths.
        let mut header = Vec::new();
        header.extend_from_slice(CACHE_MAGIC);
        header
            .write_u64::<LittleEndian>(key.source.len() as u64)
            .unwrap();
        header.extend_from_slice(key.source.as_bytes());
        header.write_u64::<LittleEndian>(key.size).unwrap();
        header.write_u128::<LittleEndian>(key.mtime).unwrap();

        // Absurd raw_decompressed length: must be rejected, not allocated.
        let mut absurd_len = header.clone();
        absurd_len.write_u64::<LittleEndian>(u64::MAX).unwrap();
        assert!(parse_index_cache(&absurd_len, &key).is_none());

        // Absurd file count after empty sections.
        let mut absurd_count = header;
        absurd_count.write_u64::<LittleEndian>(0).unwrap(); // raw_decompressed len
        absurd_count.write_u64::<LittleEndian>(0).unwrap(); // bundle count
        absurd_count
            .write_u64::<LittleEndian>(u64::MAX / 4)
            .unwrap(); // file count
        assert!(parse_index_cache(&absurd_count, &key).is_none());
    }

    #[test]
    fn changed_source_cannot_publish_a_stale_index_cache() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        write_test_install(&game, InstallLayout::ContentGgpk);
        let store = BundleStore::new_with_cache_dir(game.clone(), temp.path().join("cache"));
        let source_key_before = store.cache_key().unwrap().unwrap();
        let stale_index =
            BundleIndex::for_test_paths(&[("data/balance/activeskills.datc64", "old", 1)]);

        // Model a launcher update finishing after the index was read but before
        // its cache was serialized. A size change makes the key change exact.
        fs::write(game.join("Content.ggpk"), b"updated-ggpk").unwrap();
        assert_ne!(store.cache_key().unwrap().unwrap(), source_key_before);

        store.write_cache(&stale_index, &source_key_before);

        assert!(!store.cache_path().exists());
        assert!(store.read_cache().is_none());
    }

    #[test]
    fn open_index_remaps_replaced_content_before_caching() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        fs::create_dir_all(&game).unwrap();
        let content = game.join("Content.ggpk");
        write_test_ggpk(
            &content,
            "Bundles2/_.index.bin",
            &test_index_bytes(&["old"]),
        );
        let mut store = BundleStore::new_with_cache_dir(game.clone(), temp.path().join("cache"));

        let old = store.open_index().unwrap();
        assert!(old.has_bundle_prefix("old"));

        let replacement = game.join("Content.replacement.ggpk");
        write_test_ggpk(
            &replacement,
            "Bundles2/_.index.bin",
            &test_index_bytes(&["replacement"]),
        );
        fs::remove_file(&content).unwrap();
        fs::rename(replacement, &content).unwrap();

        let rebuilt = store.open_index().unwrap();
        assert!(rebuilt.has_bundle_prefix("replacement"));
        assert!(!rebuilt.has_bundle_prefix("old"));

        let cached = store.open_index().unwrap();
        assert!(cached.has_bundle_prefix("replacement"));
        assert!(!cached.has_bundle_prefix("old"));
    }

    #[cfg(unix)]
    #[test]
    fn aliases_share_cache_identity_for_both_install_layouts() {
        for layout in [InstallLayout::LooseBundles, InstallLayout::ContentGgpk] {
            let temp = tempfile::tempdir().unwrap();
            let game = temp.path().join("game");
            write_test_install(&game, layout);
            let alias = temp.path().join("game-alias");
            std::os::unix::fs::symlink(&game, &alias).unwrap();
            let cache_dir = temp.path().join("cache");

            let direct = BundleStore::new_with_cache_dir(game.clone(), cache_dir.clone());
            let aliased = BundleStore::new_with_cache_dir(alias.clone(), cache_dir);

            assert_eq!(direct.layout, layout);
            assert_eq!(aliased.layout, layout);
            assert_eq!(direct.game_dir, game);
            assert_eq!(aliased.game_dir, alias);
            if matches!(layout, InstallLayout::ContentGgpk) {
                assert!(!direct.index_path.exists());
            }
            assert_eq!(direct.cache_path(), aliased.cache_path());
            assert_eq!(
                direct.cache_key().unwrap().unwrap().source,
                aliased.cache_key().unwrap().unwrap().source
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn case_distinct_installs_have_distinct_cache_paths() {
        let temp = tempfile::tempdir().unwrap();
        let upper = temp.path().join("Game");
        let lower = temp.path().join("game");
        for game in [&upper, &lower] {
            write_test_install(game, InstallLayout::LooseBundles);
        }
        assert_ne!(
            fs::canonicalize(&upper).unwrap(),
            fs::canonicalize(&lower).unwrap()
        );
        let cache_dir = temp.path().join("cache");

        assert_ne!(
            BundleStore::new_with_cache_dir(upper, cache_dir.clone()).cache_path(),
            BundleStore::new_with_cache_dir(lower, cache_dir).cache_path()
        );
    }

    #[test]
    fn distinct_installs_have_distinct_cache_identities() {
        let temp = tempfile::tempdir().unwrap();
        let first_game = temp.path().join("install-a");
        let second_game = temp.path().join("install-b");
        write_test_install(&first_game, InstallLayout::LooseBundles);
        write_test_install(&second_game, InstallLayout::LooseBundles);
        let cache_dir = temp.path().join("cache");

        let first = BundleStore::new_with_cache_dir(first_game, cache_dir.clone());
        let second = BundleStore::new_with_cache_dir(second_game, cache_dir);

        assert_ne!(first.cache_source, second.cache_source);
        assert_ne!(first.cache_path(), second.cache_path());
    }

    #[test]
    fn missing_game_dir_uses_deterministic_cache_identity_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing-game");
        let cache_dir = temp.path().join("cache");
        assert!(!missing.exists());

        let first = BundleStore::new_with_cache_dir(missing.clone(), cache_dir.clone());
        let second = BundleStore::new_with_cache_dir(missing.clone(), cache_dir);
        let expected_source = std::path::absolute(&missing)
            .unwrap()
            .join("Bundles2")
            .join("_.index.bin")
            .to_string_lossy()
            .into_owned();

        assert_eq!(first.game_dir, missing);
        assert_eq!(first.cache_source, expected_source);
        assert_eq!(first.cache_source, second.cache_source);
        assert_eq!(first.cache_path(), second.cache_path());
    }

    #[cfg(unix)]
    #[test]
    fn cache_write_read_and_clear_agree_across_aliases() {
        for layout in [InstallLayout::LooseBundles, InstallLayout::ContentGgpk] {
            let temp = tempfile::tempdir().unwrap();
            let game = temp.path().join("game");
            write_test_install(&game, layout);
            let alias = temp.path().join("game-alias");
            std::os::unix::fs::symlink(&game, &alias).unwrap();
            let cache_dir = temp.path().join("cache");
            fs::create_dir_all(&cache_dir).unwrap();
            fs::write(cache_dir.join("index-cache.bin"), b"legacy").unwrap();
            let direct = BundleStore::new_with_cache_dir(game, cache_dir.clone());
            let aliased = BundleStore::new_with_cache_dir(alias, cache_dir.clone());
            let index = BundleIndex::for_test_paths(&[("Metadata/Test.dat", "test", 3)]);
            let source_key = direct.cache_key().unwrap().unwrap();

            direct.write_cache(&index, &source_key);
            assert!(!cache_dir.join("index-cache.bin").exists());
            assert!(cache_dir.join(CACHE_MIGRATION_MARKER).exists());
            assert!(direct.cache_path().exists());

            let cached = aliased
                .read_cache()
                .expect("alias should read direct cache");
            assert_eq!(cached.paths.unwrap(), vec!["Metadata/Test.dat".to_string()]);

            aliased.clear_cache();
            assert!(!direct.cache_path().exists());
            assert!(direct.read_cache().is_none());
        }
    }

    #[test]
    fn legacy_cache_migration_is_exact_and_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let cache_dir = temp.path().join("cache");
        fs::create_dir_all(&cache_dir).unwrap();
        let legacy = [
            "index-cache.bin",
            "index-cache-0123456789abcdef.bin",
            "index-cache-v2-0123456789abcdef.bin",
        ];
        let preserved = [
            "index-cache-v3-0123456789abcdef.bin",
            "index-cache-v2.migrated",
            "index-cache-v2-0123456789abcdef.tmp42",
            "index-cache-0123456789ABCDEf.bin",
            "index-cache-0123.bin",
            "unrelated.bin",
        ];
        for name in legacy.into_iter().chain(preserved) {
            fs::write(cache_dir.join(name), b"keep-or-remove").unwrap();
        }
        let cache_like_dir = cache_dir.join("index-cache-fedcba9876543210.bin");
        fs::create_dir(&cache_like_dir).unwrap();

        retire_legacy_caches(&cache_dir);

        for name in legacy {
            assert!(!cache_dir.join(name).exists());
        }
        for name in preserved {
            assert_eq!(fs::read(cache_dir.join(name)).unwrap(), b"keep-or-remove");
        }
        assert!(cache_dir.join(CACHE_MIGRATION_MARKER).exists());
        assert!(cache_like_dir.is_dir());

        let post_migration_legacy = cache_dir.join("index-cache-ffffffffffffffff.bin");
        fs::write(&post_migration_legacy, b"created by an older version").unwrap();
        retire_legacy_caches(&cache_dir);
        for name in preserved {
            assert_eq!(fs::read(cache_dir.join(name)).unwrap(), b"keep-or-remove");
        }
        assert_eq!(
            fs::read(post_migration_legacy).unwrap(),
            b"created by an older version"
        );
    }

    #[test]
    fn failed_legacy_cache_removal_retries_without_marker() {
        let temp = tempfile::tempdir().unwrap();
        let cache_dir = temp.path().join("cache");
        fs::create_dir_all(&cache_dir).unwrap();
        let legacy = cache_dir.join("index-cache-0123456789abcdef.bin");
        fs::write(&legacy, b"legacy").unwrap();

        retire_legacy_caches_with(&cache_dir, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected removal failure",
            ))
        });

        assert!(legacy.exists());
        assert!(!cache_dir.join(CACHE_MIGRATION_MARKER).exists());

        retire_legacy_caches(&cache_dir);
        assert!(!legacy.exists());
        assert!(cache_dir.join(CACHE_MIGRATION_MARKER).exists());
    }

    #[test]
    fn concurrent_legacy_cache_migration_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let cache_dir = temp.path().join("cache");
        fs::create_dir_all(&cache_dir).unwrap();
        let legacy = cache_dir.join("index-cache-0123456789abcdef.bin");
        let current = cache_dir.join("index-cache-v3-0123456789abcdef.bin");
        fs::write(&legacy, b"legacy").unwrap();
        fs::write(&current, b"current").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let cache_dir = cache_dir.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    retire_legacy_caches(&cache_dir);
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }
        assert!(!legacy.exists());
        assert_eq!(fs::read(current).unwrap(), b"current");
        assert!(cache_dir.join(CACHE_MIGRATION_MARKER).exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_variants_share_cache_identity_without_changing_public_path() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("Game");
        write_test_install(&game, InstallLayout::LooseBundles);
        let canonical = fs::canonicalize(&game).unwrap();
        let alternate = PathBuf::from(
            game.to_string_lossy()
                .replace('\\', "/")
                .to_ascii_uppercase(),
        );
        let cache_dir = temp.path().join("cache");

        let direct = BundleStore::new_with_cache_dir(game.clone(), cache_dir.clone());
        let verbatim = BundleStore::new_with_cache_dir(canonical.clone(), cache_dir.clone());
        let alternate_store = BundleStore::new_with_cache_dir(alternate.clone(), cache_dir.clone());

        assert_eq!(direct.game_dir, game);
        assert_eq!(verbatim.game_dir, canonical);
        assert_eq!(alternate_store.game_dir, alternate);
        assert_eq!(direct.cache_source, verbatim.cache_source);
        assert_eq!(direct.cache_source, alternate_store.cache_source);
        assert_eq!(direct.cache_path(), verbatim.cache_path());
        assert_eq!(direct.cache_path(), alternate_store.cache_path());
    }

    #[test]
    fn failed_cache_write_removes_partial_temp_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index-cache.bin");
        let tmp = temp.path().join("index-cache.tmp");

        write_cache_file(&path, &tmp, b"partial cache", |tmp, data| {
            fs::write(tmp, data)?;
            Err(std::io::Error::other("injected write failure"))
        });

        assert!(!tmp.exists());
        assert!(!path.exists());
    }

    #[test]
    fn failed_cache_rename_removes_temp_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index-cache.bin");
        let tmp = temp.path().join("index-cache.tmp");
        fs::create_dir(&path).unwrap();

        write_cache_file(&path, &tmp, b"complete cache", |tmp, data| {
            fs::write(tmp, data)
        });

        assert!(!tmp.exists());
        assert!(path.is_dir());
    }

    #[test]
    fn successful_cache_write_leaves_final_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index-cache.bin");
        let tmp = temp.path().join("index-cache.tmp");

        write_cache_file(&path, &tmp, b"complete cache", |tmp, data| {
            fs::write(tmp, data)
        });

        assert!(!tmp.exists());
        assert_eq!(fs::read(path).unwrap(), b"complete cache");
    }

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

    fn test_index_bytes(bundle_names: &[&str]) -> Vec<u8> {
        let mut raw = Vec::new();
        raw.write_u32::<LittleEndian>(bundle_names.len() as u32)
            .unwrap();
        for bundle_name in bundle_names {
            raw.write_u32::<LittleEndian>(bundle_name.len() as u32)
                .unwrap();
            raw.extend_from_slice(bundle_name.as_bytes());
            raw.write_u32::<LittleEndian>(0).unwrap();
        }
        raw.write_u32::<LittleEndian>(0).unwrap(); // file count
        raw.write_u32::<LittleEndian>(1).unwrap(); // directory count
        raw.write_u64::<LittleEndian>(0x07E47507B4A92E53)
            .unwrap();
        raw.extend_from_slice(&[0; 12]);
        raw.extend_from_slice(&crate::bundle::pack_uncompressed_bundle(&[]).unwrap());
        crate::bundle::pack_uncompressed_bundle(&raw).unwrap()
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
