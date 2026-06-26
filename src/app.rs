use crate::backup::{BackupEntry, BackupStore};
use crate::bundle::{apply_bundle_replacements, BundleIndex, BundleStore};
use crate::install::{ensure_game_not_running, resolve_game_dir};
use crate::patches::{
    all_patches, all_presets, compute_patch_set, parse_patch, parse_preset, PatchChange, PatchId,
};
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchState {
    Clean,
    Patched,
    StaleBackup,
    PatchedMissingBackup,
}

impl PatchState {
    pub fn is_currently_patched(self) -> bool {
        matches!(self, Self::Patched | Self::PatchedMissingBackup)
    }

    pub fn can_restore(self) -> bool {
        matches!(self, Self::Patched)
    }

    pub fn has_stale_backup(self) -> bool {
        matches!(self, Self::StaleBackup)
    }
}

#[derive(Debug, Clone)]
pub struct AppStatus {
    pub game_dir: PathBuf,
    pub index_path: PathBuf,
    pub indexed_paths: usize,
    pub backup_path: PathBuf,
    pub has_backup: bool,
    pub patch_state: PatchState,
}

#[derive(Debug, Clone)]
pub struct PatchRequest {
    pub game_dir: Option<PathBuf>,
    pub patches: Vec<PatchId>,
    pub zoom: f64,
}

#[derive(Debug, Clone, Default)]
pub struct PatchSelection {
    pub patches: Vec<String>,
    pub presets: Vec<String>,
    pub all: bool,
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
    let has_backup = backup.has_backup();
    let patch_state = classify_patch_state(has_backup, index_is_patched(&index));

    Ok(AppStatus {
        game_dir,
        index_path: store.index_path,
        indexed_paths,
        backup_path: backup.path().to_path_buf(),
        has_backup,
        patch_state,
    })
}

pub fn list_patch_ids() -> Vec<PatchId> {
    all_patches().iter().map(|patch| patch.id).collect()
}

pub fn parse_patch_names(names: &[String], all: bool) -> Result<Vec<PatchId>> {
    resolve_patch_selection(PatchSelection {
        patches: names.to_vec(),
        all,
        ..PatchSelection::default()
    })
}

pub fn resolve_patch_selection(selection: PatchSelection) -> Result<Vec<PatchId>> {
    let mut out = Vec::new();

    if selection.all {
        for patch in all_patches() {
            out.push(patch.id);
        }
    }

    for name in &selection.presets {
        let preset = parse_preset(name).ok_or_else(|| anyhow!("unknown preset: {name}"))?;
        out.extend_from_slice(preset.patches);
    }

    for name in &selection.patches {
        let patch = parse_patch(name).ok_or_else(|| anyhow!("unknown patch: {name}"))?;
        out.push(patch);
    }

    if out.is_empty() {
        bail!("select at least one --patch/--preset or use --all");
    }

    Ok(unique_patch_ids(out))
}

pub fn patch_names(ids: &[PatchId]) -> Vec<&'static str> {
    ids.iter()
        .filter_map(|id| all_patches().iter().find(|patch| patch.id == *id))
        .map(|patch| patch.name)
        .collect()
}

pub fn preset_names() -> Vec<&'static str> {
    all_presets().iter().map(|preset| preset.name).collect()
}

fn unique_patch_ids(ids: Vec<PatchId>) -> Vec<PatchId> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for id in ids {
        if seen.insert(id) {
            out.push(id);
        }
    }
    out
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
    let patch_state = classify_patch_state(backup.has_backup(), index_is_patched(&index));
    ensure_can_apply(patch_state, backup.path())?;

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

    if patch_state.has_stale_backup() {
        backup.remove().with_context(|| {
            format!(
                "failed to remove obsolete backup at {}",
                backup.path().display()
            )
        })?;
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
        if !index_is_patched(&index) {
            bail!(
                "backup at {} is obsolete because the current game index is not patched;\n\
                 apply patches again to replace it with a fresh backup",
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

fn classify_patch_state(has_backup: bool, index_is_patched: bool) -> PatchState {
    match (has_backup, index_is_patched) {
        (false, false) => PatchState::Clean,
        (true, true) => PatchState::Patched,
        (true, false) => PatchState::StaleBackup,
        (false, true) => PatchState::PatchedMissingBackup,
    }
}

fn index_is_patched(index: &BundleIndex) -> bool {
    index.has_bundle_prefix("TinyPoe2Smoother/") || index.has_bundle_prefix("LibGGPK3/")
}

fn ensure_can_apply(patch_state: PatchState, backup_path: &Path) -> Result<()> {
    match patch_state {
        PatchState::Clean | PatchState::StaleBackup => Ok(()),
        PatchState::Patched => bail!(
            "game is already patched;\n\
             restore before applying a different patch selection\n\
             Backup: {}",
            backup_path.display()
        ),
        PatchState::PatchedMissingBackup => bail!(
            "game index is already patched by tiny-poe2smoother, but no backup was found;\n\
             verify game files via Steam before applying again"
        ),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_state_classifies_backup_and_index_combinations() {
        assert_eq!(classify_patch_state(false, false), PatchState::Clean);
        assert_eq!(classify_patch_state(true, true), PatchState::Patched);
        assert_eq!(classify_patch_state(true, false), PatchState::StaleBackup);
        assert_eq!(
            classify_patch_state(false, true),
            PatchState::PatchedMissingBackup
        );
    }

    #[test]
    fn stale_backup_does_not_block_apply() {
        let backup_path = Path::new("/tmp/poe2.bak");

        assert!(ensure_can_apply(PatchState::Clean, backup_path).is_ok());
        assert!(ensure_can_apply(PatchState::StaleBackup, backup_path).is_ok());
    }

    #[test]
    fn currently_patched_index_blocks_apply() {
        let backup_path = Path::new("/tmp/poe2.bak");

        let with_backup = ensure_can_apply(PatchState::Patched, backup_path)
            .unwrap_err()
            .to_string();
        let without_backup = ensure_can_apply(PatchState::PatchedMissingBackup, backup_path)
            .unwrap_err()
            .to_string();

        assert!(with_backup.contains("already patched"));
        assert!(without_backup.contains("no backup was found"));
    }

    #[test]
    fn all_selects_every_patch_including_capture_backed_ones() {
        let patches = resolve_patch_selection(PatchSelection {
            all: true,
            ..PatchSelection::default()
        })
        .unwrap();

        assert_eq!(patches.len(), all_patches().len());
        assert!(patches.contains(&PatchId::Fog));
        assert!(patches.contains(&PatchId::DisableSounds));
        assert!(patches.contains(&PatchId::MtxSoft));
    }

    #[test]
    fn sound_and_mtx_patches_resolve_without_any_opt_in() {
        let resolved = resolve_patch_selection(PatchSelection {
            patches: vec![
                "disable-sounds".to_string(),
                "skill-sounds".to_string(),
                "monster-sounds".to_string(),
                "mtx-soft".to_string(),
            ],
            ..PatchSelection::default()
        })
        .unwrap();

        assert_eq!(
            resolved,
            vec![
                PatchId::DisableSounds,
                PatchId::SkillSounds,
                PatchId::MonsterSounds,
                PatchId::MtxSoft,
            ]
        );
    }

    #[test]
    fn removed_destructive_patches_are_unknown() {
        for name in [
            "zero-effects",
            "black-screen",
            "remove-players",
            "remove-monsters",
            "clean-terrain",
            "zero-materials",
            "mtx-full",
        ] {
            let err = resolve_patch_selection(PatchSelection {
                patches: vec![name.to_string()],
                ..PatchSelection::default()
            })
            .unwrap_err()
            .to_string();
            assert!(err.contains("unknown patch"), "{name}: {err}");
        }
    }

    #[test]
    fn presets_expand_and_aliases_resolve() {
        let maps = resolve_patch_selection(PatchSelection {
            presets: vec!["maps-revealed".to_string()],
            patches: vec!["zero-particles".to_string()],
            ..PatchSelection::default()
        })
        .unwrap();

        assert_eq!(
            maps,
            vec![PatchId::Minimap, PatchId::AtlasFog, PatchId::Particles]
        );
    }
}
