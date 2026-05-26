use crate::backup::{BackupEntry, BackupStore};
use crate::bundle::{apply_bundle_replacements, BundleStore};
use crate::install::{ensure_game_not_running, resolve_game_dir};
use crate::patches::{all_patches, compute_patch_set, parse_patch, PatchChange, PatchId};
use anyhow::{anyhow, bail, Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AppStatus {
    pub game_dir: PathBuf,
    pub index_path: PathBuf,
    pub indexed_paths: usize,
    pub backup_path: PathBuf,
    pub has_backup: bool,
}

#[derive(Debug, Clone)]
pub struct PatchRequest {
    pub game_dir: Option<PathBuf>,
    pub patches: Vec<PatchId>,
    pub zoom: f64,
}

#[derive(Debug, Clone)]
pub struct PatchPreview {
    pub game_dir: PathBuf,
    pub patches: Vec<PatchId>,
    pub changes: Vec<PatchChange>,
}

#[derive(Debug, Clone)]
pub struct ApplyReport {
    pub game_dir: PathBuf,
    pub changed_files: usize,
    pub touched_paths: Vec<PathBuf>,
    pub backup_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct RestoreReport {
    pub game_dir: PathBuf,
    pub restored_files: usize,
}

#[derive(Debug, Clone)]
pub struct PathSearchResult {
    pub path: String,
    pub has_record: bool,
}

#[derive(Debug, Clone)]
pub struct InspectedPath {
    pub path: String,
    pub size: usize,
    pub bytes: Vec<u8>,
    pub text_preview: Option<String>,
}

pub fn load_status(game_dir: Option<PathBuf>) -> Result<AppStatus> {
    crate::timing!("load_status_total");
    let game_dir = resolve_game_dir(game_dir)?;
    let store = BundleStore::new(&game_dir);
    let mut index = store.open_index()?;
    let indexed_paths = index.ensure_paths_built()?.len();
    let backup = BackupStore::default()?;

    Ok(AppStatus {
        game_dir,
        index_path: store.index_path,
        indexed_paths,
        backup_path: backup.path().to_path_buf(),
        has_backup: backup.has_backup(),
    })
}

pub fn list_patch_ids() -> Vec<PatchId> {
    all_patches().iter().map(|patch| patch.id).collect()
}

pub fn parse_patch_names(names: &[String], all: bool) -> Result<Vec<PatchId>> {
    if all {
        return Ok(list_patch_ids());
    }
    if names.is_empty() {
        bail!("select at least one --patch or use --all");
    }
    names
        .iter()
        .map(|name| parse_patch(name).ok_or_else(|| anyhow!("unknown patch: {name}")))
        .collect()
}

pub fn patch_names(ids: &[PatchId]) -> Vec<&'static str> {
    ids.iter()
        .filter_map(|id| all_patches().iter().find(|patch| patch.id == *id))
        .map(|patch| patch.name)
        .collect()
}

pub fn preview_patches(request: PatchRequest) -> Result<PatchPreview> {
    crate::timing!("preview_patches_total");
    let game_dir = resolve_game_dir(request.game_dir)?;
    let store = BundleStore::new(&game_dir);
    let mut index = store.open_index()?;
    let patch_set = compute_patch_set(&store, &mut index, &request.patches, request.zoom)?;

    Ok(PatchPreview {
        game_dir,
        patches: request.patches,
        changes: patch_set.changes,
    })
}

pub fn apply_patches(request: PatchRequest) -> Result<ApplyReport> {
    crate::timing!("apply_patches_total");
    ensure_game_not_running()?;

    let game_dir = resolve_game_dir(request.game_dir)?;
    let store = BundleStore::new(&game_dir);
    let backup = BackupStore::default()?;
    if backup.has_backup() {
        bail!(
            "game is already patched;\n\
             restore before applying a different patch selection\n\
             Backup: {}",
            backup.path().display()
        );
    }

    // Preflight checks
    if !store.index_path.exists() {
        bail!(
            "index file not found at {};\nverify game install or pass --game-dir",
            store.index_path.display()
        );
    }

    let mut index = match store.open_index() {
        Ok(idx) => idx,
        Err(e) => bail!(
            "failed to read/decode {};\n  cause: {}\nverify game files via Steam",
            store.index_path.display(),
            e
        ),
    };

    let patch_set = compute_patch_set(&store, &mut index, &request.patches, request.zoom)?;

    if patch_set.changes.is_empty() {
        bail!(
            "selected patches produced no changes;\n\
             the game may already be patched or this game version may be unsupported"
        );
    }

    // Verify each patch target exists before proceeding
    for change in &patch_set.changes {
        let bundle_path = store.bundle_path(&change.bundle_name);
        if !bundle_path.exists() {
            bail!(
                "bundle file required by '{}' not found: {};\nverify game files via Steam",
                change.path,
                bundle_path.display()
            );
        }
    }

    let rel_paths = vec![PathBuf::from("Bundles2/_.index.bin")];

    backup
        .ensure_originals(&game_dir, &rel_paths)
        .with_context(|| {
            format!(
                "failed to create backup at {};\n  check disk space and permissions",
                backup.path().display()
            )
        })?;

    let touched_paths = apply_bundle_replacements(&store, &mut index, &patch_set.replacements)
        .with_context(|| {
            format!(
                "failed to write generated bundle to {};\n  restore first if partially applied",
                store.bundles_dir.join("TinyPoe2Smoother").display()
            )
        })?;

    Ok(ApplyReport {
        game_dir,
        changed_files: patch_set.changes.len(),
        touched_paths,
        backup_path: backup.path().to_path_buf(),
    })
}

pub fn restore_backup(game_dir: Option<PathBuf>) -> Result<RestoreReport> {
    ensure_game_not_running()?;
    let game_dir = resolve_game_dir(game_dir)?;
    let backup = BackupStore::default()?;
    let restored_files = if backup.has_backup() {
        if backup.count()? == 0 {
            bail!(
                "backup exists but is empty at {};\n  check file integrity",
                backup.path().display()
            );
        }
        let store = BundleStore::new(&game_dir);
        let index = store.open_index().with_context(|| {
            format!(
                "failed to read current index before restore: {}",
                store.index_path.display()
            )
        })?;
        if !index.has_bundle_prefix("TinyPoe2Smoother/") && !index.has_bundle_prefix("LibGGPK3/") {
            bail!(
                "current game index does not look patched by tiny-poe2smoother;\n\
                 refusing to restore a possibly stale backup from {};\n\
                 verify game files in Steam instead",
                backup.path().display()
            );
        }
        backup.restore(&game_dir).with_context(|| {
            format!(
                "restore from {} failed;\n  backup may be corrupt",
                backup.path().display()
            )
        })?
    } else {
        eprintln!("No backup found at {}", backup.path().display());
        0
    };
    let store = BundleStore::new(&game_dir);
    store.clear_cache();
    Ok(RestoreReport {
        game_dir,
        restored_files,
    })
}

pub fn backup_entries() -> Result<(PathBuf, Vec<BackupEntry>)> {
    let backup = BackupStore::default()?;
    Ok((backup.path().to_path_buf(), backup.entries()?))
}

pub fn find_paths(
    game_dir: Option<PathBuf>,
    query: &str,
    limit: usize,
) -> Result<Vec<PathSearchResult>> {
    let game_dir = resolve_game_dir(game_dir)?;
    let store = BundleStore::new(&game_dir);
    let mut index = store.open_index()?;
    let query = query.to_ascii_lowercase();
    let paths = index.ensure_paths_built()?.to_vec();

    Ok(paths
        .iter()
        .filter(|path| path.to_ascii_lowercase().contains(&query))
        .take(limit)
        .map(|path| PathSearchResult {
            path: path.clone(),
            has_record: index.file_by_path(path).is_some(),
        })
        .collect())
}

pub fn inspect_path(
    game_dir: Option<PathBuf>,
    path: &str,
    byte_limit: usize,
) -> Result<InspectedPath> {
    let game_dir = resolve_game_dir(game_dir)?;
    let store = BundleStore::new(&game_dir);
    let index = store.open_index()?;
    let bytes = store.read_file(&index, path)?;
    let limited = bytes.iter().take(byte_limit).copied().collect::<Vec<_>>();
    let text_preview = if bytes.starts_with(&[0xff, 0xfe]) {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        String::from_utf16(&units)
            .ok()
            .map(|text| text.chars().take(byte_limit).collect())
    } else {
        std::str::from_utf8(&bytes)
            .ok()
            .map(|text| text.chars().take(byte_limit).collect())
    };

    Ok(InspectedPath {
        path: path.to_string(),
        size: bytes.len(),
        bytes: limited,
        text_preview,
    })
}
