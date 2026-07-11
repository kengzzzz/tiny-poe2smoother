use crate::backup::{ggpk_backup_meta, BackupEntry, BackupStore};
use crate::bundle::{apply_bundle_replacements, BundleIndex, BundleStore};
use crate::install::{
    detect_install_layout, ensure_game_not_running, resolve_game_dir, InstallLayout,
};
use crate::patches::{
    all_patches, all_presets, build_effect_skill_catalog, build_monster_effect_catalog,
    compute_patch_set, parse_patch, parse_preset, parse_stat_catalog, unique_patches,
    EffectSkillCatalogEntry, MonsterEffectCatalogEntry, PatchChange, PatchId, PatchParams,
    StatCatalogEntry, ACTIONTYPES_DATC64_PATH, ACTIVESKILLS_DATC64_PATH,
    ITEM_VISUAL_EFFECT_DATC64_PATH,
};
use anyhow::{anyhow, bail, Context, Result};
use rayon::prelude::*;
use std::collections::HashSet;
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
    pub index_display_path: String,
    pub install_layout: InstallLayout,
    pub indexed_paths: usize,
    pub backup_path: PathBuf,
    pub has_backup: bool,
    pub patch_state: PatchState,
}

#[derive(Debug, Clone)]
pub struct PatchRequest {
    pub game_dir: Option<PathBuf>,
    pub patches: Vec<PatchId>,
    pub params: PatchParams,
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
    pub backup_removed: bool,
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
    let backup = BackupStore::from_default_location()?;
    let has_backup = backup.has_backup();
    let patch_state = classify_patch_state(has_backup, index_is_patched(&index));
    let index_display_path = store.index_display_path();

    Ok(AppStatus {
        game_dir,
        index_path: store.index_path,
        index_display_path,
        install_layout: store.layout,
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

    Ok(unique_patches(&out))
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

pub fn preview_patches(request: PatchRequest) -> Result<PatchPreview> {
    crate::timing!("preview_patches_total");
    let game_dir = resolve_game_dir(request.game_dir)?;
    let store = BundleStore::new(&game_dir);
    let mut index = store.open_index()?;
    let patch_set = compute_patch_set(&store, &mut index, &request.patches, &request.params)?;

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
    let backup = BackupStore::from_default_location()?;

    if !store.index_exists()? {
        bail!(
            "index file not found at {};\nverify game install or choose the game folder with Browse",
            store.index_display_path()
        );
    }

    let mut index = match store.open_index() {
        Ok(idx) => idx,
        Err(e) => bail!(
            "failed to read/decode {};\n  cause: {}\nverify game files with Steam or the standalone launcher",
            store.index_display_path(),
            e
        ),
    };
    let patch_state = classify_patch_state(backup.has_backup(), index_is_patched(&index));
    ensure_can_apply(patch_state, backup.path())?;

    let patch_set = compute_patch_set(&store, &mut index, &request.patches, &request.params)?;

    if patch_set.changes.is_empty() {
        bail!(
            "selected patches produced no changes;\n\
             the game may already be patched or this game version may be unsupported"
        );
    }

    // Verify each patch target exists before proceeding
    for change in &patch_set.changes {
        if !store.bundle_exists(&change.bundle_name)? {
            bail!(
                "bundle file required by '{}' not found: {};\nverify game files with Steam or the standalone launcher",
                change.path,
                store.bundle_display_path(&change.bundle_name)
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

    if !matches!(store.layout, InstallLayout::ContentGgpk) {
        let rel_paths = vec![(
            PathBuf::from("Bundles2/_.index.bin"),
            store.read_index_bytes()?,
        )];

        backup
            .ensure_original_bytes(&game_dir, &rel_paths)
            .with_context(|| {
                format!(
                    "failed to create backup at {};\n  check disk space and permissions",
                    backup.path().display()
                )
            })?;
    }

    let write_target = match store.layout {
        InstallLayout::ContentGgpk => store.index_display_path(),
        InstallLayout::LooseBundles => store
            .bundles_dir
            .join("TinyPoe2Smoother")
            .display()
            .to_string(),
    };
    let write_report =
        apply_bundle_replacements(&store, &mut index, &patch_set.replacements, Some(&backup))
            .with_context(|| {
                format!(
                    "failed to write generated bundle to {write_target};\n  restore first if partially applied"
                )
            })?;

    Ok(ApplyReport {
        game_dir,
        changed_files: patch_set.changes.len(),
        touched_paths: write_report.touched_paths,
        backup_path: backup.path().to_path_buf(),
    })
}

pub fn restore_backup(game_dir: Option<PathBuf>) -> Result<RestoreReport> {
    ensure_game_not_running()?;
    let game_dir = resolve_game_dir(game_dir)?;
    let backup = BackupStore::from_default_location()?;
    restore_backup_with_store(game_dir, backup)
}

fn restore_backup_with_store(game_dir: PathBuf, backup: BackupStore) -> Result<RestoreReport> {
    let restored_files = if backup.has_backup() {
        // Parse the backup once; the emptiness check, GGPK metadata lookup,
        // and restore below all work off the same entries.
        let entries = backup.entries()?;
        if entries.is_empty() {
            bail!(
                "backup exists but is empty at {};\n  check file integrity",
                backup.path().display()
            );
        }
        let mut store = BundleStore::new(&game_dir);
        let index = store.open_index().with_context(|| {
            format!(
                "failed to read current index before restore: {}",
                store.index_display_path()
            )
        })?;
        if !index_is_patched(&index) {
            backup.remove().with_context(|| {
                format!(
                    "failed to remove obsolete backup at {}",
                    backup.path().display()
                )
            })?;
            store.clear_cache();
            return Ok(RestoreReport {
                game_dir,
                restored_files: 0,
                backup_removed: true,
            });
        }
        let overlay_restore = matches!(
            detect_install_layout(&game_dir)?,
            InstallLayout::ContentGgpk
        );
        if overlay_restore {
            if let Some(meta) = ggpk_backup_meta(&entries)? {
                store.restore_ggpk_backup(meta).with_context(|| {
                    format!(
                        "failed to restore {};\n  backup may be corrupt",
                        store.index_display_path()
                    )
                })?;
                store.remove_legacy_overlay_files()?;
            }
        }
        backup
            .restore(&entries, &game_dir, overlay_restore)
            .with_context(|| {
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
        backup_removed: restored_files > 0,
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
    index.has_bundle_prefix("TinyPoe2Smoother/")
        || index.has_bundle_prefix("TinyPoe2Smoother_")
        || index.has_bundle_prefix("LibGGPK3/")
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
             verify game files with Steam or the standalone launcher before applying again"
        ),
    }
}

pub fn backup_entries() -> Result<(PathBuf, Vec<BackupEntry>)> {
    let backup = BackupStore::from_default_location()?;
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
    index.ensure_paths_built()?;

    Ok(index
        .paths()
        .iter()
        .filter(|path| path.to_ascii_lowercase().contains(&query))
        .take(limit)
        .map(|path| PathSearchResult {
            path: path.clone(),
            has_record: index.file_by_path(path).is_some(),
        })
        .collect())
}

/// Build the searchable stat catalog for the color-mods editor from the
/// top-level `data/statdescriptions/*.csd` files of the user's install. The
/// 560 `specific_skill_stat_descriptions/` files only re-describe ids already
/// present in `stat_descriptions.csd`, so they are skipped. Runs off the UI
/// thread (called from a background task in the GUI); never part of apply.
pub fn load_stat_catalog(game_dir: Option<PathBuf>) -> Result<Vec<StatCatalogEntry>> {
    crate::timing!("load_stat_catalog_total");
    const PREFIX: &str = "data/statdescriptions/";
    let game_dir = resolve_game_dir(game_dir)?;
    let store = BundleStore::new(&game_dir);
    let mut index = store.open_index()?;
    let entries = index.matching_paths_by(|path| {
        let normalized = path.replace('\\', "/").to_ascii_lowercase();
        normalized
            .strip_prefix(PREFIX)
            .is_some_and(|rest| rest.ends_with(".csd") && !rest.contains('/'))
    })?;
    let files: Vec<_> = entries.iter().map(|entry| entry.file).collect();
    let by_hash = store.read_files_batch(&index, &files)?;

    let parsed: Vec<Vec<StatCatalogEntry>> = files
        .par_iter()
        .map(|file| {
            let Some(bytes) = by_hash.get(&file.hash) else {
                return Vec::new();
            };
            let Ok(text) = crate::patches::decode_utf16(bytes) else {
                return Vec::new();
            };
            parse_stat_catalog(&text)
        })
        .collect();

    let mut seen = HashSet::new();
    let mut catalog: Vec<StatCatalogEntry> = parsed
        .into_iter()
        .flatten()
        .filter(|entry| seen.insert(entry.stat_id.clone()))
        .collect();
    catalog.sort_by(|a, b| a.stat_id.cmp(&b.stat_id));
    Ok(catalog)
}

/// Build the per-skill effects editor catalog from live `ActiveSkills`
/// rows, resolving each skill to effect folders through the game's own
/// visual/animation tables. Runs off the UI thread; never part of apply.
pub fn load_effect_skill_catalog(
    game_dir: Option<PathBuf>,
) -> Result<Vec<EffectSkillCatalogEntry>> {
    crate::timing!("load_effect_skill_catalog_total");
    let game_dir = resolve_game_dir(game_dir)?;
    let store = BundleStore::new(&game_dir);
    let mut index = store.open_index()?;
    index.ensure_paths_built()?;
    let activeskills = store
        .read_file(&index, ACTIVESKILLS_DATC64_PATH)
        .context("read activeskills.datc64")?;
    let actiontypes = store
        .read_file(&index, ACTIONTYPES_DATC64_PATH)
        .context("read actiontypes.datc64")?;
    let item_visual_effects = store.read_file(&index, ITEM_VISUAL_EFFECT_DATC64_PATH).ok();
    let miscanimated = store
        .read_file(&index, "data/balance/miscanimated.datc64")
        .ok();

    build_effect_skill_catalog(
        &activeskills,
        &actiontypes,
        item_visual_effects.as_deref(),
        miscanimated.as_deref(),
        index.paths(),
    )
    .ok_or_else(|| anyhow!("could not parse active skill effect catalog"))
}

/// Build the per-monster effects editor catalog from the monster tables and
/// every monster metadata file of the user's install. Heavier than the skill
/// catalog (one batch read over all monster metadata files), so the GUI
/// starts it only when the monster editor is first opened. Runs off the UI
/// thread; never part of apply.
pub fn load_monster_effect_catalog(
    game_dir: Option<PathBuf>,
) -> Result<Vec<MonsterEffectCatalogEntry>> {
    crate::timing!("load_monster_effect_catalog_total");
    let game_dir = resolve_game_dir(game_dir)?;
    let store = BundleStore::new(&game_dir);
    let mut index = store.open_index()?;
    build_monster_effect_catalog(&store, &mut index)
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
    use crate::backup::BackupStore;
    use crate::bundle::pack_uncompressed_bundle;
    use std::fs;

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
    fn restore_removes_obsolete_backup_when_index_is_clean() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        fs::create_dir_all(game.join("Bundles2")).unwrap();
        fs::write(game.join("Bundles2/_.index.bin"), test_index_bytes(&[])).unwrap();
        let backup = BackupStore::for_path(temp.path().join("poe2.bak"));
        backup
            .ensure_original_bytes(
                &game,
                &[(
                    PathBuf::from("Bundles2/_.index.bin"),
                    test_index_bytes(&["TinyPoe2Smoother/0"]),
                )],
            )
            .unwrap();

        let report = restore_backup_with_store(game, backup.clone()).unwrap();

        assert_eq!(report.restored_files, 0);
        assert!(report.backup_removed);
        assert!(!backup.has_backup());
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
        // "black-screen" used to be on this list, but was reintroduced as a
        // safe shader-stub patch (see `PatchId::BlackScreen`) rather than the
        // original tool's destructive game-wide file blanking.
        for name in [
            "zero-effects",
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

    fn test_index_bytes(bundle_names: &[&str]) -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend_from_slice(&(bundle_names.len() as u32).to_le_bytes());
        for bundle_name in bundle_names {
            raw.extend_from_slice(&(bundle_name.len() as u32).to_le_bytes());
            raw.extend_from_slice(bundle_name.as_bytes());
            raw.extend_from_slice(&0u32.to_le_bytes());
        }
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&1u32.to_le_bytes());
        raw.extend_from_slice(&0x07E47507B4A92E53u64.to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes());
        // Trailing compressed directory data; open_index builds paths from it
        // (and caches them) even when the directory record is empty.
        raw.extend_from_slice(&pack_uncompressed_bundle(&[]).unwrap());
        pack_uncompressed_bundle(&raw).unwrap()
    }
}
