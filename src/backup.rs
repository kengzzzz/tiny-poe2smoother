use anyhow::{anyhow, bail, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Read, Seek, Write};
use std::path::{Path, PathBuf};

const MAGIC: u32 = 0x4B425332; // "2SBK"
const VERSION: u32 = 2;
const APP_NAME: &str = env!("CARGO_PKG_NAME");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const GGPK_META_PATH: &str = "__tiny_poe2smoother/ggpk-meta-v1";
const GGPK_META_MAGIC: u32 = 0x4D475032; // "2PGM"

#[derive(Debug, Clone)]
pub struct BackupEntry {
    pub rel_path: PathBuf,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct BackupStore {
    path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GgpkBackupMeta {
    pub bundles2_offset_pos: u64,
    pub original_bundles2_offset: u64,
    pub appended_start: u64,
    pub appended_end: u64,
}

impl BackupStore {
    pub fn from_default_location() -> Result<Self> {
        let base = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .ok_or_else(|| anyhow!("could not locate user data directory"))?;
        let dir = base.join("tiny-poe2smoother");
        Ok(Self {
            path: dir.join("poe2.bak"),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn has_backup(&self) -> bool {
        self.path.exists()
    }

    pub fn remove(&self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => {
                Err(err).with_context(|| format!("failed to remove {}", self.path.display()))
            }
        }
    }

    pub fn entries(&self) -> Result<Vec<BackupEntry>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        read_entries(&self.path)
    }

    pub fn ensure_ggpk_backup_meta(&self, game_dir: &Path, meta: GgpkBackupMeta) -> Result<()> {
        self.ensure_original_bytes(
            game_dir,
            &[(PathBuf::from(GGPK_META_PATH), encode_ggpk_meta(meta)?)],
        )
    }

    pub fn ensure_originals(&self, game_dir: &Path, rel_paths: &[PathBuf]) -> Result<()> {
        let entries = rel_paths
            .iter()
            .map(|rel_path| {
                let abs = game_dir.join(rel_path);
                let bytes = fs::read(&abs)
                    .with_context(|| format!("failed to backup {}", abs.display()))?;
                Ok((rel_path.clone(), bytes))
            })
            .collect::<Result<Vec<_>>>()?;
        self.ensure_original_bytes(game_dir, &entries)
    }

    pub fn ensure_original_bytes(
        &self,
        game_dir: &Path,
        entries: &[(PathBuf, Vec<u8>)],
    ) -> Result<()> {
        let mut known = HashSet::new();
        if self.path.exists() {
            for entry in self.entries()? {
                known.insert(entry.rel_path);
            }
        } else {
            if let Some(parent) = self.path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = Vec::new();
            out.write_u32::<LittleEndian>(MAGIC)?;
            out.write_u32::<LittleEndian>(VERSION)?;
            // Write manifest: app name, app version, game dir
            let manifest = format!("{APP_NAME} v{APP_VERSION} game={}", game_dir.display());
            out.write_u32::<LittleEndian>(manifest.len() as u32)?;
            out.extend_from_slice(manifest.as_bytes());
            fs::write(&self.path, out)?;
        }

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open backup {}", self.path.display()))?;

        for (rel_path, bytes) in entries {
            if !known.insert(rel_path.clone()) {
                continue;
            }
            let path_bytes = rel_path.to_string_lossy().as_bytes().to_vec();
            file.write_u32::<LittleEndian>(path_bytes.len() as u32)?;
            file.write_all(&path_bytes)?;
            file.write_u64::<LittleEndian>(bytes.len() as u64)?;
            file.write_all(bytes)?;
        }
        file.flush()?;
        file.sync_all()?;
        Ok(())
    }

    /// Restore `entries` (as returned by [`Self::entries`]) into the game dir,
    /// then remove the backup file. Taking the entries avoids re-parsing the
    /// backup when the caller already read them.
    pub fn restore(
        &self,
        entries: &[BackupEntry],
        game_dir: &Path,
        remove_overlay_index: bool,
    ) -> Result<usize> {
        let mut restored = 0;
        for entry in entries {
            if is_ggpk_meta_path(&entry.rel_path) {
                continue;
            }
            restored += 1;
            let path = game_dir.join(&entry.rel_path);
            if remove_overlay_index && entry.rel_path == *"Bundles2/_.index.bin" {
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        return Err(err)
                            .with_context(|| format!("failed to remove {}", path.display()));
                    }
                }
                continue;
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, &entry.bytes)
                .with_context(|| format!("failed to restore {}", path.display()))?;
        }
        let custom_bundle_dir = game_dir.join("Bundles2/TinyPoe2Smoother");
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
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        Ok(restored)
    }
}

fn is_ggpk_meta_path(path: &Path) -> bool {
    path == Path::new(GGPK_META_PATH)
}

/// Find and decode the GGPK metadata entry among already-read backup entries.
pub fn ggpk_backup_meta(entries: &[BackupEntry]) -> Result<Option<GgpkBackupMeta>> {
    entries
        .iter()
        .find(|entry| is_ggpk_meta_path(&entry.rel_path))
        .map(|entry| decode_ggpk_meta(&entry.bytes))
        .transpose()
}

fn encode_ggpk_meta(meta: GgpkBackupMeta) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.write_u32::<LittleEndian>(GGPK_META_MAGIC)?;
    out.write_u64::<LittleEndian>(meta.bundles2_offset_pos)?;
    out.write_u64::<LittleEndian>(meta.original_bundles2_offset)?;
    out.write_u64::<LittleEndian>(meta.appended_start)?;
    out.write_u64::<LittleEndian>(meta.appended_end)?;
    Ok(out)
}

fn decode_ggpk_meta(bytes: &[u8]) -> Result<GgpkBackupMeta> {
    let mut cursor = Cursor::new(bytes);
    if cursor.read_u32::<LittleEndian>()? != GGPK_META_MAGIC {
        bail!("backup contains invalid GGPK metadata");
    }
    Ok(GgpkBackupMeta {
        bundles2_offset_pos: cursor.read_u64::<LittleEndian>()?,
        original_bundles2_offset: cursor.read_u64::<LittleEndian>()?,
        appended_start: cursor.read_u64::<LittleEndian>()?,
        appended_end: cursor.read_u64::<LittleEndian>()?,
    })
}

fn read_entries(path: &Path) -> Result<Vec<BackupEntry>> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut cursor = Cursor::new(bytes.as_slice());
    if cursor.read_u32::<LittleEndian>()? != MAGIC {
        bail!("{} is not a tiny-poe2smoother backup", path.display());
    }
    let version = cursor.read_u32::<LittleEndian>()?;
    if !(1..=VERSION).contains(&version) {
        bail!("unsupported backup version {version} in {}", path.display());
    }
    // Skip manifest string if version >= 2
    if version >= 2 {
        let manifest_len = cursor.read_u32::<LittleEndian>()? as usize;
        let _ = cursor.seek(std::io::SeekFrom::Current(manifest_len as i64));
    }
    let mut entries = Vec::new();
    // A crash while appending can leave a truncated trailing entry; treat
    // anything running past end-of-file as end-of-archive so the intact
    // entries before it stay restorable. Lengths are checked against the
    // remaining bytes before allocating so a corrupt file cannot trigger an
    // absurd allocation.
    while cursor.position() < bytes.len() as u64 {
        let Ok(path_len) = cursor.read_u32::<LittleEndian>() else {
            break;
        };
        let Some(path_bytes) = read_chunk(&mut cursor, u64::from(path_len)) else {
            break;
        };
        let Ok(data_len) = cursor.read_u64::<LittleEndian>() else {
            break;
        };
        let Some(data) = read_chunk(&mut cursor, data_len) else {
            break;
        };
        entries.push(BackupEntry {
            rel_path: PathBuf::from(String::from_utf8(path_bytes)?),
            bytes: data,
        });
    }
    Ok(entries)
}

/// Read exactly `len` bytes, or `None` if fewer remain.
fn read_chunk(cursor: &mut Cursor<&[u8]>, len: u64) -> Option<Vec<u8>> {
    let remaining = (cursor.get_ref().len() as u64).saturating_sub(cursor.position());
    if len > remaining {
        return None;
    }
    let mut buf = vec![0; len as usize];
    cursor.read_exact(&mut buf).ok()?;
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let store = BackupStore {
            path: temp.path().join("poe2.bak"),
        };
        let game = temp.path().join("game");
        fs::create_dir_all(game.join("Bundles2")).unwrap();
        fs::write(game.join("Bundles2/_.index.bin"), b"index").unwrap();
        store
            .ensure_originals(&game, &[PathBuf::from("Bundles2/_.index.bin")])
            .unwrap();
        fs::create_dir_all(game.join("Bundles2/LibGGPK3")).unwrap();
        fs::write(game.join("Bundles2/LibGGPK3/0.bundle.bin"), b"custom").unwrap();
        fs::create_dir_all(game.join("Bundles2/TinyPoe2Smoother")).unwrap();
        fs::write(
            game.join("Bundles2/TinyPoe2Smoother/0.bundle.bin"),
            b"custom",
        )
        .unwrap();
        fs::write(game.join("Bundles2/_.index.bin"), b"changed").unwrap();
        let entries = store.entries().unwrap();
        assert_eq!(store.restore(&entries, &game, false).unwrap(), 1);
        assert_eq!(
            fs::read(game.join("Bundles2/_.index.bin")).unwrap(),
            b"index"
        );
        assert!(game.join("Bundles2/LibGGPK3").exists());
        assert!(!game.join("Bundles2/TinyPoe2Smoother").exists());
    }

    #[test]
    fn truncated_trailing_entry_keeps_intact_entries_restorable() {
        let temp = tempfile::tempdir().unwrap();
        let store = BackupStore {
            path: temp.path().join("poe2.bak"),
        };
        let game = temp.path().join("game");
        store
            .ensure_original_bytes(
                &game,
                &[(PathBuf::from("Bundles2/_.index.bin"), b"original".to_vec())],
            )
            .unwrap();

        // Simulate a crash mid-append: a partial second entry at the tail.
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&store.path)
            .unwrap();
        file.write_u32::<LittleEndian>(64).unwrap(); // path_len
        file.write_all(b"Bundles2/partial").unwrap(); // only 16 of 64 bytes
        drop(file);

        let entries = store.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].rel_path, PathBuf::from("Bundles2/_.index.bin"));

        fs::create_dir_all(game.join("Bundles2")).unwrap();
        fs::write(game.join("Bundles2/_.index.bin"), b"patched").unwrap();
        assert_eq!(store.restore(&entries, &game, false).unwrap(), 1);
        assert_eq!(
            fs::read(game.join("Bundles2/_.index.bin")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn absurd_data_length_is_rejected_without_allocating() {
        let temp = tempfile::tempdir().unwrap();
        let store = BackupStore {
            path: temp.path().join("poe2.bak"),
        };
        let game = temp.path().join("game");
        store
            .ensure_original_bytes(
                &game,
                &[(PathBuf::from("Bundles2/_.index.bin"), b"original".to_vec())],
            )
            .unwrap();

        // A corrupt entry claiming u64::MAX data bytes must not be trusted.
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&store.path)
            .unwrap();
        file.write_u32::<LittleEndian>(3).unwrap();
        file.write_all(b"bad").unwrap();
        file.write_u64::<LittleEndian>(u64::MAX).unwrap();
        file.write_all(b"short").unwrap();
        drop(file);

        let entries = store.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].rel_path, PathBuf::from("Bundles2/_.index.bin"));
    }

    #[test]
    fn remove_backup_ignores_missing_file() {
        let temp = tempfile::tempdir().unwrap();
        let store = BackupStore {
            path: temp.path().join("poe2.bak"),
        };

        store.remove().unwrap();
        fs::write(&store.path, b"backup").unwrap();
        store.remove().unwrap();

        assert!(!store.path.exists());
    }

    #[test]
    fn standalone_overlay_restore_removes_index_overlay() {
        let temp = tempfile::tempdir().unwrap();
        let store = BackupStore {
            path: temp.path().join("poe2.bak"),
        };
        let game = temp.path().join("game");
        fs::create_dir_all(game.join("Bundles2/TinyPoe2Smoother")).unwrap();
        store
            .ensure_original_bytes(
                &game,
                &[(PathBuf::from("Bundles2/_.index.bin"), b"original".to_vec())],
            )
            .unwrap();
        fs::write(game.join("Bundles2/_.index.bin"), b"patched").unwrap();
        fs::write(
            game.join("Bundles2/TinyPoe2Smoother/0.bundle.bin"),
            b"custom",
        )
        .unwrap();

        let entries = store.entries().unwrap();
        assert_eq!(store.restore(&entries, &game, true).unwrap(), 1);
        assert!(!game.join("Bundles2/_.index.bin").exists());
        assert!(!game.join("Bundles2/TinyPoe2Smoother").exists());
    }

    #[test]
    fn ggpk_backup_meta_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let store = BackupStore {
            path: temp.path().join("poe2.bak"),
        };
        let game = temp.path().join("game");
        let meta = GgpkBackupMeta {
            bundles2_offset_pos: 11,
            original_bundles2_offset: 22,
            appended_start: 33,
            appended_end: 44,
        };

        store.ensure_ggpk_backup_meta(&game, meta).unwrap();

        let entries = store.entries().unwrap();
        assert_eq!(ggpk_backup_meta(&entries).unwrap(), Some(meta));
        // The meta entry is bookkeeping, not a restored game file.
        assert_eq!(store.restore(&entries, &game, false).unwrap(), 0);
        assert!(!store.has_backup());
    }
}
