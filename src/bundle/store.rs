use super::compression::{
    OOZ_SAFE_SPACE, bundle_uncompressed_size, decompress_bundle, pack_bundle_serial,
    visit_bundle_slices, visit_bundle_slices_from,
};
use super::ggpk::{
    GgpkArchive, GgpkFileSpan, GgpkPatchPlan, GgpkPdirEntry, ggpk_file_at, ggpk_file_record,
    ggpk_find_file, ggpk_find_pdir, ggpk_name_hash, ggpk_pdir_at, ggpk_pdir_record,
    ggpk_tail_in_use,
};
use super::hashing::hash_fnv1a;
use super::index::{BundleFile, BundleIndex, PathList};
use crate::backup::GgpkBackupMeta;
use crate::install::{InstallLayout, detect_install_layout, normalized_game_dir};
use anyhow::{Context, Result, anyhow, bail};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use rayon::prelude::*;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::fs;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

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

const CACHE_MAGIC: &[u8; 4] = b"2SI6";
const CACHE_MIGRATION_MARKER: &str = "index-cache-v3.migrated";

static SHARED_INDEX: Mutex<Option<(CacheKey, Weak<BundleIndex>)>> = Mutex::new(None);

fn shared_index() -> MutexGuard<'static, Option<(CacheKey, Weak<BundleIndex>)>> {
    SHARED_INDEX.lock().unwrap_or_else(PoisonError::into_inner)
}

fn default_cache_dir() -> PathBuf {
    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("tiny-poe2smoother")
}

fn cache_source(game_dir: &std::path::Path, layout: InstallLayout) -> String {
    let game_dir = normalized_game_dir(game_dir);
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
    // One-time sweep of legacy cache files. v3 and temp files are kept.
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

fn read_cache_section(reader: &mut impl Read, remaining: &mut u64) -> Option<Vec<u8>> {
    let len = reader.read_u64::<LittleEndian>().ok()?;
    *remaining = remaining.checked_sub(8)?.checked_sub(len)?;
    let mut bytes = vec![0; usize::try_from(len).ok()?];
    reader.read_exact(&mut bytes).ok()?;
    Some(bytes)
}

fn write_cache_section(writer: &mut impl Write, bytes: &[u8]) -> std::io::Result<()> {
    writer.write_u64::<LittleEndian>(bytes.len() as u64)?;
    writer.write_all(bytes)
}

/// `len` bounds every section, so a corrupt length can't force a huge allocation.
fn parse_index_cache(mut reader: impl Read, len: u64, key: &CacheKey) -> Option<BundleIndex> {
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic).ok()?;
    if &magic != CACHE_MAGIC {
        return None;
    }
    let mut remaining = len.checked_sub(4)?;
    let cached_source = read_cache_section(&mut reader, &mut remaining)?;
    remaining = remaining.checked_sub(24)?;
    let cached_size = reader.read_u64::<LittleEndian>().ok()?;
    let cached_mtime = reader.read_u128::<LittleEndian>().ok()?;
    if cached_source != key.source.as_bytes()
        || cached_size != key.size
        || cached_mtime != key.mtime
    {
        return None;
    }
    let raw_decompressed = read_cache_section(&mut reader, &mut remaining)?;
    // Real path text is about 1.8x the raw index. This only stops absurd headers.
    let text_budget = (raw_decompressed.len() as u64).saturating_mul(4);
    let paths = read_path_blocks(&mut reader, &mut remaining, text_budget)?;
    if remaining != 0 {
        return None;
    }
    BundleIndex::with_paths(raw_decompressed, paths).ok()
}

/// Decodes each block on the pool while the next is read, so few compressed blocks are
/// held at once.
fn read_path_blocks(
    reader: &mut impl Read,
    remaining: &mut u64,
    mut text_budget: u64,
) -> Option<PathList> {
    *remaining = remaining.checked_sub(4)?;
    let count = reader.read_u32::<LittleEndian>().ok()? as usize;
    if count as u64 > *remaining / 16 {
        return None;
    }
    let slots: Vec<OnceLock<Option<(String, usize)>>> =
        std::iter::repeat_with(OnceLock::new).take(count).collect();
    let all_read = rayon::in_place_scope(|scope| {
        for slot in &slots {
            let Some(packed) = read_cache_section(reader, remaining) else {
                return false;
            };
            // Oodle trusts its input, so damaged blocks must never reach it.
            let Some(rest) = remaining.checked_sub(8) else {
                return false;
            };
            *remaining = rest;
            if reader.read_u64::<LittleEndian>().ok() != Some(checksum(&packed)) {
                return false;
            }
            let Some(size) = bundle_uncompressed_size(&packed)
                .ok()
                .and_then(|size| u64::try_from(size).ok())
                .filter(|&size| size <= text_budget)
            else {
                return false;
            };
            text_budget -= size;
            scope.spawn(move |_| {
                let block = decompress_bundle(&packed)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .and_then(PathList::checked_block);
                let _ = slot.set(block);
            });
        }
        true
    });
    if !all_read {
        return None;
    }
    let mut blocks = Vec::with_capacity(count);
    let mut len = 0;
    for slot in slots {
        let (block, block_len) = slot.into_inner()??;
        blocks.push(block);
        len += block_len;
    }
    Some(PathList::from_blocks(blocks, len))
}

fn write_index_cache(
    writer: &mut impl Write,
    index: &BundleIndex,
    key: &CacheKey,
) -> std::io::Result<()> {
    writer.write_all(CACHE_MAGIC)?;
    write_cache_section(writer, key.source.as_bytes())?;
    writer.write_u64::<LittleEndian>(key.size)?;
    writer.write_u128::<LittleEndian>(key.mtime)?;
    write_cache_section(writer, &index.raw_decompressed)?;
    // Paths compress about 4x, which halves the read on every cached start.
    let blocks = index.paths().blocks();
    writer
        .write_u32::<LittleEndian>(u32::try_from(blocks.len()).map_err(std::io::Error::other)?)?;
    for block in blocks {
        let packed = pack_bundle_serial(block.as_bytes()).map_err(std::io::Error::other)?;
        write_cache_section(writer, &packed)?;
        writer.write_u64::<LittleEndian>(checksum(&packed))?;
    }
    Ok(())
}

const CACHE_TEMP_SUFFIX: &str = ".tps-tmp";
/// A live writer keeps touching its temp, so one this old was left by a killed app.
const STALE_CACHE_TEMP_AGE: Duration = Duration::from_secs(60);

static CACHE_WRITES: Mutex<Vec<(PathBuf, JoinHandle<()>)>> = Mutex::new(Vec::new());

fn cache_writes() -> MutexGuard<'static, Vec<(PathBuf, JoinHandle<()>)>> {
    CACHE_WRITES.lock().unwrap_or_else(PoisonError::into_inner)
}

fn source_key(source_path: &std::path::Path, source: &str) -> Result<CacheKey> {
    let meta = fs::metadata(source_path)?;
    let mtime = meta
        .modified()?
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    Ok(CacheKey {
        source: source.to_string(),
        size: meta.len(),
        mtime,
    })
}

/// Runs after the index is returned. It holds no game file handle or mapping, so Apply
/// and Restore can still rewrite or truncate game data meanwhile.
struct CacheWrite {
    source_path: PathBuf,
    key: CacheKey,
    path: PathBuf,
    index: Arc<BundleIndex>,
}

impl CacheWrite {
    fn spawn(self) {
        let mut writes = cache_writes();
        writes.retain(|(_, handle)| !handle.is_finished());
        if writes.iter().any(|(path, _)| *path == self.path) {
            return;
        }
        let path = self.path.clone();
        let spawned = std::thread::Builder::new()
            .name("index-cache-write".into())
            .spawn(move || {
                if let Err(err) = self.write() {
                    tracing::debug!("index cache not written: {err:#}");
                }
            });
        if let Ok(handle) = spawned {
            writes.push((path, handle));
        }
    }

    fn write(&self) -> Result<()> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("cache path has no parent"))?;
        fs::create_dir_all(dir)?;
        retire_legacy_caches(dir);
        remove_stale_cache_temps(dir, &self.path, STALE_CACHE_TEMP_AGE);
        publish_cache(
            &self.path,
            |file| {
                let mut writer = BufWriter::new(file);
                write_index_cache(&mut writer, &self.index, &self.key)?;
                writer.flush()
            },
            // The game may have updated while writing, so don't publish a stale snapshot.
            || {
                source_key(&self.source_path, &self.key.source)
                    .ok()
                    .as_ref()
                    == Some(&self.key)
            },
        )
    }
}

/// Renames a synced temp over `path` only if `still_valid`, so readers never see a
/// partial or stale file.
fn publish_cache(
    path: &std::path::Path,
    write: impl FnOnce(&mut fs::File) -> std::io::Result<()>,
    still_valid: impl FnOnce() -> bool,
) -> Result<()> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        bail!("not a file path: {}", path.display());
    };
    let mut temp = tempfile::Builder::new()
        .prefix(&format!(".{}.", name.to_string_lossy()))
        .suffix(CACHE_TEMP_SUFFIX)
        .tempfile_in(dir)?;
    write(temp.as_file_mut())?;
    temp.as_file().sync_all()?;
    if !still_valid() {
        bail!("game data changed while caching");
    }
    temp.persist(path)?;
    Ok(())
}

/// Also removes the `.tmp<pid>` temps that older versions left.
fn remove_stale_cache_temps(dir: &std::path::Path, path: &std::path::Path, min_age: Duration) {
    let (Some(name), Some(stem)) = (path.file_name(), path.file_stem()) else {
        return;
    };
    let temp_prefix = format!(".{}.", name.to_string_lossy());
    let old_prefix = format!("{}.tmp", stem.to_string_lossy());
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        let ours = (file_name.starts_with(&temp_prefix) && file_name.ends_with(CACHE_TEMP_SUFFIX))
            || file_name
                .strip_prefix(&old_prefix)
                .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()));
        let old_enough = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= min_age);
        if ours && old_enough {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Detects damaged cache blocks. Not cryptographic.
fn checksum(bytes: &[u8]) -> u64 {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut lanes = [K, K.rotate_left(17), K.rotate_left(31), K.rotate_left(47)];
    let (words, tail) = bytes.as_chunks::<32>();
    for chunk in words {
        for (lane, word) in lanes.iter_mut().zip(chunk.as_chunks::<8>().0) {
            *lane = (*lane ^ u64::from_le_bytes(*word))
                .wrapping_mul(K)
                .rotate_left(29);
        }
    }
    let mut hash = lanes.iter().fold(bytes.len() as u64, |hash, &lane| {
        (hash ^ lane).wrapping_mul(K).rotate_left(27)
    });
    for &byte in tail {
        hash = (hash ^ u64::from(byte)).wrapping_mul(K).rotate_left(5);
    }
    hash ^ (hash >> 32)
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
        // Per install so other installs or tests can't clobber it. Path case is kept.
        let tag = hash_fnv1a(self.cache_source.as_bytes());
        self.cache_dir
            .join(format!("index-cache-v3-{tag:016x}.bin"))
    }

    /// Reopens held storage and returns the cache key for that snapshot. A GGPK
    /// can be replaced while an old mmap stays readable.
    fn refresh_source_snapshot(&mut self) -> Result<Option<CacheKey>> {
        let key_before = self.cache_key()?;
        if matches!(self.layout, InstallLayout::ContentGgpk) {
            let ggpk = GgpkArchive::from_file(&self.content_path)?;
            if self.cache_key()? != key_before {
                bail!(
                    "game data changed while opening Content.ggpk. \
                     Wait for the standalone launcher to finish updating, then retry"
                );
            }
            self.ggpk = Some(Arc::new(ggpk));
        }
        Ok(key_before)
    }

    fn ensure_source_unchanged(&self, source_key: Option<&CacheKey>, phase: &str) -> Result<()> {
        if self.cache_key()?.as_ref() != source_key {
            bail!(
                "game data changed while {phase} the bundle index. \
                 Wait for Steam or the standalone launcher to finish updating, then retry"
            );
        }
        Ok(())
    }

    fn read_cache(&self) -> Option<BundleIndex> {
        crate::timing!("cache_read");
        retire_legacy_caches(&self.cache_dir);
        let key_before = self.cache_key().ok()??;
        let file = fs::File::open(self.cache_path()).ok()?;
        let len = file.metadata().ok()?.len();
        let index = parse_index_cache(file, len, &key_before)?;
        let key_after = self.cache_key().ok()??;
        (key_before == key_after).then_some(index)
    }

    fn source_path(&self) -> Option<PathBuf> {
        if matches!(self.layout, InstallLayout::ContentGgpk) {
            Some(self.content_path.clone())
        } else {
            self.index_path.exists().then(|| self.index_path.clone())
        }
    }

    fn cache_write(&self, index: Arc<BundleIndex>, key: &CacheKey) -> Option<CacheWrite> {
        Some(CacheWrite {
            source_path: self.source_path()?,
            key: key.clone(),
            path: self.cache_path(),
            index,
        })
    }

    #[cfg(test)]
    fn write_cache(&self, index: BundleIndex, key: &CacheKey) {
        if let Some(job) = self.cache_write(Arc::new(index), key) {
            let _ = job.write();
        }
    }

    /// Waits for background cache writes. Short-lived tools call this before exiting.
    pub fn flush_cache_writes() {
        let writes = std::mem::take(&mut *cache_writes());
        for (_, handle) in writes {
            let _ = handle.join();
        }
    }

    pub fn clear_cache(&self) {
        retire_legacy_caches(&self.cache_dir);
        let mut shared = shared_index();
        if shared
            .as_ref()
            .is_some_and(|(key, _)| key.source == self.cache_source)
        {
            *shared = None;
        }
        drop(shared);
        let p = self.cache_path();
        if p.exists() {
            let _ = fs::remove_file(&p);
        }
    }

    /// For Apply and Restore. It never reads the cache, so cached bytes can't reach the
    /// game index.
    pub fn open_index(&mut self) -> Result<BundleIndex> {
        let source_key = self.refresh_source_snapshot()?;
        self.rebuild_index(source_key.as_ref())
    }

    fn rebuild_index(&self, source_key: Option<&CacheKey>) -> Result<BundleIndex> {
        crate::timing!("index_rebuild");
        let bytes = self.read_index_bytes()?;
        let decompressed =
            decompress_bundle(&bytes).context("failed to decompress bundle index")?;
        drop(bytes);
        let index = BundleIndex::parse(decompressed)?;
        self.ensure_source_unchanged(source_key, "building")?;
        Ok(index)
    }

    /// Overlapping readers share one index, freed when the last one drops it. A miss
    /// returns once the index is built and writes the cache in the background.
    pub fn open_shared_index(&mut self) -> Result<Arc<BundleIndex>> {
        let mut shared = shared_index();
        let Some(key) = self.refresh_source_snapshot()? else {
            return self.rebuild_index(None).map(Arc::new);
        };
        if let Some((held_key, held)) = shared.as_ref()
            && *held_key == key
            && let Some(index) = held.upgrade()
        {
            return Ok(index);
        }
        let index = match self.read_cache() {
            Some(cached) => {
                self.ensure_source_unchanged(Some(&key), "loading")?;
                Arc::new(cached)
            }
            None => {
                let index = Arc::new(self.rebuild_index(Some(&key))?);
                if let Some(job) = self.cache_write(index.clone(), &key) {
                    job.spawn();
                }
                index
            }
        };
        *shared = Some((key, Arc::downgrade(&index)));
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
        let mut bytes = self.visit_files_batch(index, &[file], |_, bytes| Ok(bytes))?;
        Ok(bytes.pop().expect("one file requested"))
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

    pub fn read_files_batch(
        &self,
        index: &BundleIndex,
        files: &[BundleFile],
    ) -> Result<HashMap<u64, Vec<u8>>> {
        let bytes = self.visit_files_batch(index, files, |_, bytes| Ok(bytes))?;
        Ok(files.iter().map(|file| file.hash).zip(bytes).collect())
    }

    /// Calls `visit` as each file is decoded. Results keep the order of `files`.
    pub fn visit_files_batch<T: Send>(
        &self,
        index: &BundleIndex,
        files: &[BundleFile],
        visit: impl Fn(usize, Vec<u8>) -> Result<T> + Sync,
    ) -> Result<Vec<T>> {
        let mut by_bundle: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, file) in files.iter().enumerate() {
            by_bundle.entry(file.bundle_index).or_default().push(i);
        }
        let groups: Vec<_> = by_bundle.into_iter().collect();
        let per_bundle: Vec<Vec<(usize, Result<T>)>> = groups
            .par_iter()
            .map(|(bundle_index, members)| {
                let name = index.bundle_name(*bundle_index)?;
                let spans: Vec<(u32, u32)> = members
                    .iter()
                    .map(|&i| (files[i].offset, files[i].size))
                    .collect();
                let mut out = Vec::with_capacity(members.len());
                let collect = |k: usize, bytes| {
                    let i = members[k];
                    out.push((i, visit(i, bytes)));
                };
                let decoded = if matches!(self.layout, InstallLayout::ContentGgpk) {
                    let compressed = self
                        .read_storage_file_cow(&storage_bundle_path(name))
                        .with_context(|| {
                            format!("failed to read {}", self.bundle_display_path(name))
                        })?;
                    visit_bundle_slices(&compressed, &spans, collect)
                } else {
                    // Read only the needed chunks. GGPK is mapped, so it skips this.
                    let path = self.bundle_path(name);
                    let mut file = fs::File::open(&path)
                        .with_context(|| format!("failed to read {}", path.display()))?;
                    let len = file.metadata()?.len();
                    visit_bundle_slices_from(
                        usize::try_from(len)?,
                        |offset, len| {
                            // Zeroed slack in case Oodle reads past the chunk end.
                            let mut buf = vec![0; len + OOZ_SAFE_SPACE];
                            file.seek(SeekFrom::Start(offset as u64))?;
                            file.read_exact(&mut buf[..len])?;
                            buf.truncate(len);
                            Ok(std::borrow::Cow::Owned(buf))
                        },
                        &spans,
                        collect,
                    )
                };
                decoded.with_context(|| {
                    format!("failed to decompress {}", self.bundle_display_path(name))
                })?;
                Ok(out)
            })
            .collect::<Result<_>>()?;
        let mut slots: Vec<Option<T>> = std::iter::repeat_with(|| None).take(files.len()).collect();
        for (i, result) in per_bundle.into_iter().flatten() {
            slots[i] = Some(result?);
        }
        slots
            .into_iter()
            .zip(files)
            .map(|(slot, file)| {
                slot.ok_or_else(|| anyhow!("no bytes decoded for file {:016x}", file.hash))
            })
            .collect()
    }

    fn read_storage_file(&self, rel_path: &str) -> Result<Vec<u8>> {
        self.read_storage_file_cow(rel_path)
            .map(std::borrow::Cow::into_owned)
    }

    fn read_storage_file_cow(&self, rel_path: &str) -> Result<std::borrow::Cow<'_, [u8]>> {
        if matches!(self.layout, InstallLayout::ContentGgpk) {
            let span = self.ggpk_file_span(rel_path)?.ok_or_else(|| {
                anyhow!("{} not found in {}", rel_path, self.content_path.display())
            })?;
            let ggpk = self.ggpk()?;
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
        ggpk_find_file(self.ggpk()?, rel_path)
    }

    fn ggpk(&self) -> Result<&GgpkArchive> {
        self.ggpk
            .as_deref()
            .ok_or_else(|| anyhow!("failed to open {}", self.content_path.display()))
    }

    pub(super) fn prepare_ggpk_patch(
        &self,
        custom_bundle_name: &str,
        custom_bundle_bytes: &[u8],
        index_bytes: &[u8],
    ) -> Result<GgpkPatchPlan> {
        let ggpk = self.ggpk()?;
        let bundles2 = ggpk_find_pdir(ggpk, "Bundles2")?.ok_or_else(|| {
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
                "{} changed while preparing patch. Verify game files and retry",
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
        drop(file);

        self.trim_restored_ggpk_tail(meta)?;
        Ok(())
    }

    /// Fails when Content.ggpk changed after Apply.
    pub fn ggpk_restore_target(&self, meta: GgpkBackupMeta) -> Result<BundleIndex> {
        let ggpk = self.ggpk()?;
        let live = ggpk_find_pdir(ggpk, "Bundles2")?.context("Bundles2 directory not found")?;
        if live.parent_offset_pos != Some(meta.bundles2_offset_pos)
            || !(meta.appended_start..meta.appended_end).contains(&live.offset)
            || (ggpk.mmap.len() as u64) < meta.appended_end
        {
            bail!("Bundles2 no longer points at the patch");
        }
        let original = ggpk_pdir_at(ggpk, meta.original_bundles2_offset)?
            .filter(|pdir| pdir.name == live.name && pdir.digest == live.digest)
            .context("the original Bundles2 directory is gone")?;
        let span = original
            .entries
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case("_.index.bin"))
            .and_then(|entry| ggpk_file_at(ggpk, entry.offset).transpose())
            .transpose()?
            .context("the original index is gone")?;
        let bytes = &ggpk.mmap[span.begin..span.begin + span.len];
        BundleIndex::parse(decompress_bundle(bytes)?).context("the original index is damaged")
    }

    /// Trims appended records once nothing in Content.ggpk uses them.
    pub fn trim_restored_ggpk_tail(&mut self, meta: GgpkBackupMeta) -> Result<bool> {
        // An interrupted Apply can leave a partial append.
        let len = fs::metadata(&self.content_path)?.len();
        if len <= meta.appended_start || len > meta.appended_end {
            return Ok(false);
        }
        if ggpk_tail_in_use(
            &GgpkArchive::from_file(&self.content_path)?,
            meta.appended_start,
        )? {
            return Ok(false);
        }
        // Fails on Windows while any process maps the file.
        self.ggpk = None;
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&self.content_path)
            .with_context(|| format!("failed to open {}", self.content_path.display()))?;
        file.set_len(meta.appended_start).with_context(|| {
            format!(
                "failed to shrink {}.\n  Close programs that use it and retry",
                self.content_path.display()
            )
        })?;
        file.sync_all()?;
        Ok(true)
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
pub(crate) mod tests {
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

    fn test_key() -> CacheKey {
        CacheKey {
            source: "src".to_string(),
            size: 1,
            mtime: 2,
        }
    }

    fn parse_cache_bytes(bytes: &[u8], key: &CacheKey) -> Option<BundleIndex> {
        parse_index_cache(bytes, bytes.len() as u64, key)
    }

    fn cached_paths(index: &BundleIndex) -> Vec<&str> {
        index.paths().iter().collect()
    }

    #[test]
    fn corrupt_index_cache_is_rejected_without_huge_allocations() {
        let key = test_key();
        // Truncated header and wrong magic.
        assert!(parse_cache_bytes(b"2SI2", &key).is_none());
        assert!(parse_cache_bytes(b"XXXXXXXXXXXXXXXX", &key).is_none());

        // Valid magic + matching key, then corrupt lengths.
        let mut header = Vec::new();
        header.extend_from_slice(CACHE_MAGIC);
        write_cache_section(&mut header, key.source.as_bytes()).unwrap();
        header.write_u64::<LittleEndian>(key.size).unwrap();
        header.write_u128::<LittleEndian>(key.mtime).unwrap();

        // Absurd raw_decompressed length: must be rejected, not allocated.
        let mut absurd_len = header.clone();
        absurd_len.write_u64::<LittleEndian>(u64::MAX).unwrap();
        assert!(parse_cache_bytes(&absurd_len, &key).is_none());

        let mut absurd_count = header;
        write_cache_section(&mut absurd_count, &u32::MAX.to_le_bytes()).unwrap();
        write_cache_section(&mut absurd_count, b"").unwrap();
        assert!(parse_cache_bytes(&absurd_count, &key).is_none());
    }

    #[test]
    fn index_cache_round_trip_keeps_raw_bytes_and_paths() {
        let key = test_key();
        let index = BundleIndex::for_test_paths(&[
            ("Metadata/First.dat", "first", 3),
            ("Metadata/Second.dat", "second", 4),
        ]);
        let mut bytes = Vec::new();
        write_index_cache(&mut bytes, &index, &key).unwrap();

        let cached = parse_cache_bytes(&bytes, &key).unwrap();

        assert_eq!(cached.raw_decompressed, index.raw_decompressed);
        assert_eq!(cached_paths(&cached), cached_paths(&index));
        let second = cached.file_by_path("Metadata/Second.dat").unwrap();
        assert_eq!(second.size, 4);
        assert_eq!(cached.bundle_name(second.bundle_index).unwrap(), "second");
    }

    #[test]
    fn index_cache_keeps_the_order_of_many_path_blocks() {
        let key = test_key();
        let mut index = BundleIndex::for_test_paths(&[
            ("Metadata/First.dat", "first", 3),
            ("Metadata/Second.dat", "second", 4),
            ("Metadata/Third.dat", "third", 5),
        ]);
        index.paths_mut_for_test().split_blocks_for_test();
        let mut bytes = Vec::new();
        write_index_cache(&mut bytes, &index, &key).unwrap();

        let cached = parse_cache_bytes(&bytes, &key).unwrap();

        assert_eq!(cached_paths(&cached), cached_paths(&index));
        assert_eq!(cached.paths().len(), 3);
    }

    #[test]
    fn path_block_with_an_absurd_size_is_rejected_before_decoding() {
        let key = test_key();
        let index = BundleIndex::for_test_paths(&[("Metadata/First.dat", "first", 3)]);
        let mut bytes = Vec::new();
        write_index_cache(&mut bytes, &index, &key).unwrap();
        let packed = pack_bundle_serial(b"Metadata/First.dat\0").unwrap();
        let block_end = bytes.len() - 8;
        let block_start = block_end - packed.len();
        assert_eq!(&bytes[block_start..block_end], packed.as_slice());
        // The 64-bit uncompressed size sits 20 bytes into the bundle header.
        bytes[block_start + 20..block_start + 28].copy_from_slice(&(1u64 << 40).to_le_bytes());
        // A matching checksum, so the size limit is what rejects it.
        let sum = checksum(&bytes[block_start..block_end]);
        bytes[block_end..].copy_from_slice(&sum.to_le_bytes());

        assert!(parse_cache_bytes(&bytes, &key).is_none());
    }

    #[test]
    fn damaged_path_block_is_rejected_before_decoding() {
        let key = test_key();
        let index = BundleIndex::for_test_paths(&[("Metadata/First.dat", "first", 3)]);
        let mut bytes = Vec::new();
        write_index_cache(&mut bytes, &index, &key).unwrap();
        let last_block_byte = bytes.len() - 9;
        bytes[last_block_byte] ^= 0x40;

        assert!(parse_cache_bytes(&bytes, &key).is_none());
    }

    #[test]
    fn damaged_index_cache_is_rejected() {
        let key = test_key();
        let index = BundleIndex::for_test_paths(&[("Metadata/First.dat", "first", 3)]);
        let mut bytes = Vec::new();
        write_index_cache(&mut bytes, &index, &key).unwrap();

        for len in 0..bytes.len() {
            assert!(parse_cache_bytes(&bytes[..len], &key).is_none(), "{len}");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(parse_cache_bytes(&longer, &key).is_none());
        let mut unterminated = bytes.clone();
        *unterminated.last_mut().unwrap() = b'x';
        assert!(parse_cache_bytes(&unterminated, &key).is_none());
        let stale = CacheKey {
            mtime: 3,
            ..test_key()
        };
        assert!(parse_cache_bytes(&bytes, &stale).is_none());
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

        // A launcher update lands after the index read but before caching.
        fs::write(game.join("Content.ggpk"), b"updated-ggpk").unwrap();
        assert_ne!(store.cache_key().unwrap().unwrap(), source_key_before);

        store.write_cache(stale_index, &source_key_before);

        assert!(!store.cache_path().exists());
        assert!(store.read_cache().is_none());
    }

    #[test]
    fn shared_open_remaps_replaced_content_before_caching() {
        let _slot = SHARED_SLOT_TESTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
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

        let old = store.open_shared_index().unwrap();
        assert!(old.has_bundle_prefix("old"));
        drop(old);
        BundleStore::flush_cache_writes();

        let replacement = game.join("Content.replacement.ggpk");
        write_test_ggpk(
            &replacement,
            "Bundles2/_.index.bin",
            &test_index_bytes(&["replacement"]),
        );
        fs::remove_file(&content).unwrap();
        fs::rename(replacement, &content).unwrap();

        let rebuilt = store.open_shared_index().unwrap();
        assert!(rebuilt.has_bundle_prefix("replacement"));
        assert!(!rebuilt.has_bundle_prefix("old"));
        drop(rebuilt);
        BundleStore::flush_cache_writes();

        let cached = store.read_cache().expect("background write should publish");
        assert!(cached.has_bundle_prefix("replacement"));
        assert!(!cached.has_bundle_prefix("old"));
    }

    #[test]
    fn owned_open_never_reads_the_cache() {
        let _slot = SHARED_SLOT_TESTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let temp = tempfile::tempdir().unwrap();
        let (mut store, _) = loose_store(temp.path(), "real");
        // Right key, wrong content, like a damaged or forged file.
        let key = store.cache_key().unwrap().unwrap();
        let forged =
            BundleIndex::parse(decompress_bundle(&test_index_bytes(&["forged"])).unwrap()).unwrap();
        store.write_cache(forged, &key);

        assert!(store.open_index().unwrap().has_bundle_prefix("real"));
        assert!(
            store
                .open_shared_index()
                .unwrap()
                .has_bundle_prefix("forged")
        );
    }

    /// Keeps other tests from replacing the single shared slot mid-test.
    static SHARED_SLOT_TESTS: Mutex<()> = Mutex::new(());

    fn loose_store(temp: &Path, bundle: &str) -> (BundleStore, PathBuf) {
        let game = temp.join("game");
        fs::create_dir_all(game.join("Bundles2")).unwrap();
        let index_path = game.join("Bundles2/_.index.bin");
        fs::write(&index_path, test_index_bytes(&[bundle])).unwrap();
        (
            BundleStore::new_with_cache_dir(game, temp.join("cache")),
            index_path,
        )
    }

    #[test]
    fn overlapping_readers_share_an_index_until_the_last_one_drops_it() {
        let _slot = SHARED_SLOT_TESTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let temp = tempfile::tempdir().unwrap();
        let (store, _) = loose_store(temp.path(), "first");

        let barrier = Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let mut store = store.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.open_shared_index().unwrap()
                })
            })
            .collect();
        let held: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert!(held.iter().all(|index| Arc::ptr_eq(index, &held[0])));

        let freed = Arc::downgrade(&held[0]);
        drop(held);
        // The background cache write holds the index until the file is written.
        BundleStore::flush_cache_writes();
        assert!(freed.upgrade().is_none());
    }

    #[test]
    fn changed_source_never_reuses_the_shared_index() {
        let _slot = SHARED_SLOT_TESTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let temp = tempfile::tempdir().unwrap();
        let (mut store, index_path) = loose_store(temp.path(), "first");
        let old = store.open_shared_index().unwrap();

        // A new size changes the key even with a coarse mtime.
        fs::write(&index_path, test_index_bytes(&["replacement"])).unwrap();
        let new = store.open_shared_index().unwrap();

        assert!(old.has_bundle_prefix("first"));
        assert!(new.has_bundle_prefix("replacement"));
    }

    #[test]
    fn clearing_the_cache_also_forgets_the_shared_index() {
        let _slot = SHARED_SLOT_TESTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let temp = tempfile::tempdir().unwrap();
        let (mut store, _) = loose_store(temp.path(), "first");
        let first = store.open_shared_index().unwrap();

        store.clear_cache();
        let second = store.open_shared_index().unwrap();

        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn apply_edits_never_reach_a_shared_index() {
        let _slot = SHARED_SLOT_TESTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let temp = tempfile::tempdir().unwrap();
        let (mut store, _) = loose_store(temp.path(), "first");
        let shared = store.open_shared_index().unwrap();

        let mut owned = store.open_index().unwrap();
        owned.create_custom_bundle("TinyPoe2Smoother_").unwrap();

        assert!(owned.has_bundle_prefix("TinyPoe2Smoother_"));
        assert!(!shared.has_bundle_prefix("TinyPoe2Smoother_"));
        assert!(
            !store
                .open_shared_index()
                .unwrap()
                .has_bundle_prefix("TinyPoe2Smoother_")
        );
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

            direct.write_cache(index, &source_key);
            assert!(!cache_dir.join("index-cache.bin").exists());
            assert!(cache_dir.join(CACHE_MIGRATION_MARKER).exists());
            assert!(direct.cache_path().exists());

            let cached = aliased
                .read_cache()
                .expect("alias should read direct cache");
            assert_eq!(cached_paths(&cached), ["Metadata/Test.dat"]);

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
        let mut preserved = vec![
            "index-cache-v3-0123456789abcdef.bin",
            "index-cache-v2.migrated",
            "index-cache-v2-0123456789abcdef.tmp42",
            "index-cache-0123.bin",
            "unrelated.bin",
        ];
        // Same file as a legacy name on case-insensitive filesystems.
        if !cfg!(windows) {
            preserved.push("index-cache-0123456789ABCDEf.bin");
        }
        for name in legacy.into_iter().chain(preserved.iter().copied()) {
            fs::write(cache_dir.join(name), b"keep-or-remove").unwrap();
        }
        let cache_like_dir = cache_dir.join("index-cache-fedcba9876543210.bin");
        fs::create_dir(&cache_like_dir).unwrap();

        retire_legacy_caches(&cache_dir);

        for name in legacy {
            assert!(!cache_dir.join(name).exists());
        }
        for name in &preserved {
            assert_eq!(fs::read(cache_dir.join(name)).unwrap(), b"keep-or-remove");
        }
        assert!(cache_dir.join(CACHE_MIGRATION_MARKER).exists());
        assert!(cache_like_dir.is_dir());

        let post_migration_legacy = cache_dir.join("index-cache-ffffffffffffffff.bin");
        fs::write(&post_migration_legacy, b"created by an older version").unwrap();
        retire_legacy_caches(&cache_dir);
        for name in &preserved {
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

    fn dir_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn failed_cache_write_removes_its_temp_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index-cache-v3-0000000000000000.bin");

        let result = publish_cache(
            &path,
            |file| {
                file.write_all(b"partial cache")?;
                Err(std::io::Error::other("injected write failure"))
            },
            || true,
        );

        assert!(result.is_err());
        assert!(dir_names(temp.path()).is_empty());
    }

    #[test]
    fn changed_source_stops_the_publish_and_removes_the_temp_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index-cache-v3-0000000000000000.bin");
        fs::write(&path, b"previous cache").unwrap();

        let result = publish_cache(&path, |file| file.write_all(b"stale cache"), || false);

        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"previous cache");
        assert_eq!(
            dir_names(temp.path()),
            ["index-cache-v3-0000000000000000.bin"]
        );
    }

    #[test]
    fn successful_cache_write_leaves_only_the_final_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index-cache-v3-0000000000000000.bin");

        publish_cache(&path, |file| file.write_all(b"complete cache"), || true).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"complete cache");
        assert_eq!(
            dir_names(temp.path()),
            ["index-cache-v3-0000000000000000.bin"]
        );
    }

    #[test]
    fn only_old_temps_of_this_cache_are_removed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index-cache-v3-0000000000000000.bin");
        let names = [
            ".index-cache-v3-0000000000000000.bin.a1b2c3.tps-tmp",
            "index-cache-v3-0000000000000000.tmp4242",
            ".index-cache-v3-0000000000000000.bin.young1.tps-tmp",
            ".index-cache-v3-1111111111111111.bin.other1.tps-tmp",
            "index-cache-v3-0000000000000000.bin",
        ];
        for name in names {
            fs::write(temp.path().join(name), b"x").unwrap();
        }
        let old = SystemTime::now() - Duration::from_secs(600);
        for name in [names[0], names[1], names[3], names[4]] {
            fs::File::options()
                .write(true)
                .open(temp.path().join(name))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }

        remove_stale_cache_temps(temp.path(), &path, STALE_CACHE_TEMP_AGE);

        assert_eq!(
            dir_names(temp.path()),
            [
                ".index-cache-v3-0000000000000000.bin.young1.tps-tmp",
                ".index-cache-v3-1111111111111111.bin.other1.tps-tmp",
                "index-cache-v3-0000000000000000.bin",
            ]
        );
    }

    #[test]
    fn damaged_cache_with_a_good_header_is_rewritten() {
        let _slot = SHARED_SLOT_TESTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let temp = tempfile::tempdir().unwrap();
        let (mut store, _) = loose_store(temp.path(), "first");
        drop(store.open_shared_index().unwrap());
        BundleStore::flush_cache_writes();
        let path = store.cache_path();
        let mut bytes = fs::read(&path).unwrap();
        let len = bytes.len();
        bytes[len - 9] ^= 0x40;
        fs::write(&path, &bytes).unwrap();
        assert!(store.read_cache().is_none());

        drop(store.open_shared_index().unwrap());
        BundleStore::flush_cache_writes();

        assert!(store.read_cache().is_some());
    }

    #[test]
    fn shared_miss_writes_the_cache_after_returning() {
        let _slot = SHARED_SLOT_TESTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let temp = tempfile::tempdir().unwrap();
        let (mut store, _) = loose_store(temp.path(), "first");

        let index = store.open_shared_index().unwrap();
        BundleStore::flush_cache_writes();
        drop(index);

        let cached = store.read_cache().expect("cache should be readable");
        assert!(cached.has_bundle_prefix("first"));
        assert_eq!(
            dir_names(&temp.path().join("cache")).len(),
            2,
            "cache and migration marker only"
        );
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
        // Windows can't truncate a mapped file.
        drop(store);

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

    #[test]
    fn trim_restored_ggpk_tail_cuts_only_a_tail_left_by_a_failed_restore() {
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
        store.apply_ggpk_patch(&plan).unwrap();
        drop(store);
        let meta = plan.backup;

        let mut patched = BundleStore::new(&game);
        assert!(!patched.trim_restored_ggpk_tail(meta).unwrap());
        assert_eq!(fs::metadata(&content).unwrap().len(), meta.appended_end);
        assert_eq!(patched.read_index_bytes().unwrap(), b"new-index");
        drop(patched);

        let mut file = fs::OpenOptions::new().write(true).open(&content).unwrap();
        file.seek(SeekFrom::Start(meta.bundles2_offset_pos))
            .unwrap();
        file.write_u64::<LittleEndian>(meta.original_bundles2_offset)
            .unwrap();
        drop(file);

        let mut half_restored = BundleStore::new(&game);
        assert!(half_restored.trim_restored_ggpk_tail(meta).unwrap());
        assert_eq!(fs::metadata(&content).unwrap().len(), original_len);
        assert!(!half_restored.trim_restored_ggpk_tail(meta).unwrap());

        let partial = (meta.appended_start + meta.appended_end) / 2;
        let file = fs::OpenOptions::new().write(true).open(&content).unwrap();
        file.set_len(partial).unwrap();
        drop(file);
        assert!(half_restored.trim_restored_ggpk_tail(meta).unwrap());
        assert_eq!(fs::metadata(&content).unwrap().len(), original_len);
        assert_eq!(
            BundleStore::new(&game).read_index_bytes().unwrap(),
            b"old-index"
        );
    }

    #[test]
    fn trim_keeps_a_tail_that_another_tool_wrote_into() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        fs::create_dir_all(&game).unwrap();
        let content = game.join("Content.ggpk");
        write_test_ggpk_files(
            &content,
            "Bundles2",
            &[("_.index.bin", b"old-index".as_slice())],
        );
        let store = BundleStore::new(&game);
        let meta = store
            .prepare_ggpk_patch("TinyPoe2Smoother_0", b"new-bundle", b"new-index")
            .unwrap()
            .backup;
        let bundles2 = ggpk_find_pdir(store.ggpk().unwrap(), "Bundles2")
            .unwrap()
            .unwrap();
        drop(store);

        let mut file = fs::OpenOptions::new().write(true).open(&content).unwrap();
        let tool_index = file.seek(SeekFrom::End(0)).unwrap();
        file.write_all(&file_record("_.index.bin", b"tool-index"))
            .unwrap();
        let entry_offset = bundles2.offset + 48 + utf16_nul_bytes("Bundles2").len() as u64 + 4;
        file.seek(SeekFrom::Start(entry_offset)).unwrap();
        file.write_u64::<LittleEndian>(tool_index).unwrap();
        drop(file);
        let len = fs::metadata(&content).unwrap().len();
        assert!(len > meta.appended_start && len <= meta.appended_end);

        assert!(
            !BundleStore::new(&game)
                .trim_restored_ggpk_tail(meta)
                .unwrap()
        );
        assert_eq!(fs::metadata(&content).unwrap().len(), len);
        assert_eq!(
            BundleStore::new(&game).read_index_bytes().unwrap(),
            b"tool-index"
        );
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
        raw.write_u64::<LittleEndian>(0x07E47507B4A92E53).unwrap();
        raw.extend_from_slice(&[0; 12]);
        raw.extend_from_slice(&crate::bundle::pack_uncompressed_bundle(&[]).unwrap());
        crate::bundle::pack_uncompressed_bundle(&raw).unwrap()
    }

    pub(crate) fn patch_test_ggpk(game: &Path, bundle_name: &str, index: &[u8]) -> GgpkBackupMeta {
        let store = BundleStore::new(game);
        let plan = store
            .prepare_ggpk_patch(bundle_name, b"new-bundle", index)
            .unwrap();
        store.apply_ggpk_patch(&plan).unwrap();
        plan.backup
    }

    pub(crate) fn write_test_ggpk_files(path: &Path, dir_name: &str, files: &[(&str, &[u8])]) {
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
