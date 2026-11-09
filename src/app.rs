use crate::backup::{BackupEntry, BackupStore, ggpk_backup_meta};
use crate::bundle::{BundleIndex, BundleStore, apply_bundle_replacements};
use crate::install::{
    InstallLayout, detect_install_layout, display_path, ensure_game_not_running,
    normalized_game_dir, resolve_game_dir,
};
use crate::patches::{
    ACTIONTYPES_DATC64_PATH, ACTIVESKILLS_DATC64_PATH, EffectSkillCatalogEntry,
    ITEM_VISUAL_EFFECT_DATC64_PATH, MonsterEffectCatalogEntry, MtxCatalogEntry,
    OtherEffectCatalogEntry, PatchChange, PatchId, PatchParams, StatCatalogEntry,
    build_effect_skill_catalog, build_monster_effect_catalog, build_mtx_catalog,
    build_other_effect_catalog, compute_patch_set, parse_stat_catalog,
};
use anyhow::{Context, Result, anyhow, bail};
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const GGPK_REPAIR: &str = "Run PackCheck.exe in the game folder, then start the game to repair it";

/// The standalone launcher alone never repairs Content.ggpk. PackCheck must run first.
pub fn repair_hint(layout: InstallLayout) -> &'static str {
    match layout {
        InstallLayout::LooseBundles => "Verify the game files in Steam",
        InstallLayout::ContentGgpk => "Run PackCheck.exe in the game folder, then start the game",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchState {
    Clean,
    Patched,
    StaleBackup,
    PatchedMissingBackup,
    OtherInstallBackup,
}

impl PatchState {
    pub fn is_currently_patched(self) -> bool {
        matches!(self, Self::Patched | Self::PatchedMissingBackup)
    }

    /// Restoring a stale backup cleans up after a failed Apply or Restore.
    pub fn can_restore(self) -> bool {
        matches!(self, Self::Patched | Self::StaleBackup)
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
    pub backup_owner: Option<PathBuf>,
    pub mtx_catalog: Vec<MtxCatalogEntry>,
}

#[derive(Debug, Clone)]
pub struct PatchRequest {
    pub game_dir: Option<PathBuf>,
    pub patches: Vec<PatchId>,
    pub params: PatchParams,
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
    let mut store = BundleStore::new(&game_dir);
    let index = store.open_shared_index()?;
    let indexed_paths = index.paths().len();
    let backup = BackupStore::from_default_location()?;
    let has_backup = backup.has_backup();
    let (patch_state, backup_owner) =
        install_patch_state(&backup, &game_dir, index_is_patched(&index));
    let index_display_path = store.index_display_path();
    let mtx_catalog = build_mtx_catalog(index.paths());

    Ok(AppStatus {
        game_dir,
        index_path: store.index_path,
        index_display_path,
        install_layout: store.layout,
        indexed_paths,
        backup_path: backup.path().to_path_buf(),
        has_backup,
        patch_state,
        backup_owner,
        mtx_catalog,
    })
}

pub fn preview_patches(request: PatchRequest) -> Result<PatchPreview> {
    crate::timing!("preview_patches_total");
    let game_dir = resolve_game_dir(request.game_dir)?;
    let mut store = BundleStore::new(&game_dir);
    let index = store.open_shared_index()?;
    let patch_set = compute_patch_set(&store, &index, &request.patches, &request.params)?;

    Ok(PatchPreview {
        game_dir,
        patches: request.patches,
        changes: patch_set.changes,
    })
}

pub fn apply_patches(request: PatchRequest) -> Result<ApplyReport> {
    apply_patches_with_store(request, BackupStore::from_default_location()?)
}

fn apply_patches_with_store(request: PatchRequest, backup: BackupStore) -> Result<ApplyReport> {
    crate::timing!("apply_patches_total");
    let _lock = backup.lock()?;
    let game_dir = resolve_game_dir(request.game_dir)?;
    ensure_game_not_running(&game_dir)?;
    let mut store = BundleStore::new(&game_dir);

    if !store.index_exists()? {
        bail!(
            "index file not found at {}.\nVerify game install or choose the game folder with Browse",
            store.index_display_path()
        );
    }

    let mut index = match store.open_index() {
        Ok(idx) => idx,
        Err(e) => bail!(
            "failed to read/decode {}.\n  Cause: {}\n{}",
            store.index_display_path(),
            e,
            repair_hint(store.layout)
        ),
    };
    ensure_backup_is_for(&backup, &game_dir)?;
    let patch_state = classify_patch_state(backup.has_backup(), index_is_patched(&index));
    ensure_can_apply(patch_state, backup.path(), store.layout)?;
    if patch_state.has_stale_backup()
        && matches!(store.layout, InstallLayout::ContentGgpk)
        && let Ok(entries) = backup.entries()
        && trim_stale_ggpk_tail(&mut store, &entries)?
    {
        store = BundleStore::new(&game_dir);
        index = store.open_index()?;
    }

    let patch_set = compute_patch_set(&store, &index, &request.patches, &request.params)?;

    if patch_set.changes.is_empty() {
        bail!(
            "selected patches produced no changes.\n\
             The game may already be patched or this game version may be unsupported"
        );
    }

    for change in &patch_set.changes {
        if !store.bundle_exists(&change.bundle_name)? {
            bail!(
                "bundle file required by '{}' not found: {}.\n{}",
                change.path,
                store.bundle_display_path(&change.bundle_name),
                repair_hint(store.layout)
            );
        }
    }

    // The game may have started while the patch set was built.
    ensure_game_not_running(&game_dir)?;

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
                    "failed to create backup at {}.\n  Check disk space and permissions",
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
    let write_report = apply_bundle_replacements(
        &mut store,
        &mut index,
        &patch_set.replacements,
        Some(&backup),
    )
    .map_err(|err| {
        let hint = if backup.has_backup() {
            "Restore to clean up"
        } else {
            "Nothing was changed"
        };
        anyhow!("failed to write generated bundle to {write_target}.\n  Cause: {err:#}\n  {hint}")
    })?;

    Ok(ApplyReport {
        game_dir,
        changed_files: patch_set.changes.len(),
        touched_paths: write_report.touched_paths,
        backup_path: backup.path().to_path_buf(),
    })
}

pub fn restore_backup(game_dir: Option<PathBuf>) -> Result<RestoreReport> {
    let game_dir = resolve_game_dir(game_dir)?;
    let backup = BackupStore::from_default_location()?;
    restore_backup_with_store(game_dir, backup)
}

fn restore_backup_with_store(game_dir: PathBuf, backup: BackupStore) -> Result<RestoreReport> {
    let _lock = backup.lock()?;
    ensure_game_not_running(&game_dir)?;
    ensure_backup_is_for(&backup, &game_dir)?;
    let restored_files = if backup.has_backup() {
        let entries = backup
            .entries()
            .with_context(|| format!("failed to read backup {}", backup.path().display()))?;
        if entries.is_empty() {
            bail!(
                "backup exists but is empty at {}.\n  Check file integrity",
                backup.path().display()
            );
        }
        let mut store = BundleStore::new(&game_dir);
        let patched = index_is_patched(&store.open_index().with_context(|| {
            format!(
                "failed to read current index before restore: {}",
                store.index_display_path()
            )
        })?);
        if !patched {
            trim_stale_ggpk_tail(&mut store, &entries)?;
            crate::backup::remove_generated_bundles(&game_dir)?;
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
        let mut restored_ggpk_index = 0;
        if overlay_restore {
            let meta = ggpk_backup_meta(&entries)?.ok_or_else(|| {
                anyhow!(
                    "the backup at {} has no Content.ggpk data, so it can't restore this install.\n  {GGPK_REPAIR}",
                    backup.path().display()
                )
            })?;
            let original_is_patched = store
                .ggpk_restore_target(meta)
                .and_then(|raw| {
                    let names = BundleIndex::bundle_names(&raw)
                        .context("the original index is damaged")?;
                    Ok(names.into_iter().any(is_patched_bundle))
                })
                .map_err(|err| {
                    anyhow!(
                        "Content.ggpk changed after Apply, so Restore can't undo the patch safely.\n  Cause: {err:#}\n  {GGPK_REPAIR}"
                    )
                })?;
            if original_is_patched {
                bail!("the backup points at an index that is already patched.\n  {GGPK_REPAIR}");
            }
            let index_display_path = store.index_display_path();
            store.restore_ggpk_backup(meta).map_err(|err| {
                anyhow!("failed to restore {index_display_path}.\n  Cause: {err:#}")
            })?;
            store.remove_legacy_overlay_files()?;
            restored_ggpk_index = 1;
        }
        restored_ggpk_index
            + backup
                .restore(&entries, &game_dir, overlay_restore)
                .with_context(|| format!("restore from {} failed", backup.path().display()))?
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

#[derive(Debug, PartialEq, Eq)]
enum BackupOwner {
    ThisInstall,
    OtherInstall(PathBuf),
    Unknown,
}

/// The single backup file stays with the install that wrote it.
fn backup_owner(recorded: Option<&Path>, game_dir: &Path) -> BackupOwner {
    let Some(recorded) = recorded else {
        return BackupOwner::Unknown;
    };
    if detect_install_layout(recorded).is_err() {
        return BackupOwner::Unknown;
    }
    if normalized_game_dir(recorded) == normalized_game_dir(game_dir) {
        BackupOwner::ThisInstall
    } else {
        BackupOwner::OtherInstall(recorded.to_path_buf())
    }
}

fn install_patch_state(
    backup: &BackupStore,
    game_dir: &Path,
    index_is_patched: bool,
) -> (PatchState, Option<PathBuf>) {
    let has_backup = backup.has_backup();
    if has_backup {
        let recorded = backup.recorded_game_dir().ok().flatten();
        if let BackupOwner::OtherInstall(owner) = backup_owner(recorded.as_deref(), game_dir) {
            return (PatchState::OtherInstallBackup, Some(owner));
        }
    }
    (classify_patch_state(has_backup, index_is_patched), None)
}

fn ensure_backup_is_for(backup: &BackupStore, game_dir: &Path) -> Result<()> {
    if !backup.has_backup() {
        return Ok(());
    }
    // A damaged header must not block replacing the backup.
    let recorded = backup.recorded_game_dir().ok().flatten();
    if let BackupOwner::OtherInstall(other) = backup_owner(recorded.as_deref(), game_dir) {
        bail!(
            "the backup at {} belongs to another install: {}.\n\
             Select that install and Restore it first",
            backup.path().display(),
            display_path(&other)
        );
    }
    Ok(())
}

/// Finishes a Restore that could not shrink Content.ggpk.
fn trim_stale_ggpk_tail(store: &mut BundleStore, entries: &[BackupEntry]) -> Result<bool> {
    if !matches!(store.layout, InstallLayout::ContentGgpk) {
        return Ok(false);
    }
    let Some(meta) = ggpk_backup_meta(entries)? else {
        return Ok(false);
    };
    store.trim_restored_ggpk_tail(meta)
}

fn classify_patch_state(has_backup: bool, index_is_patched: bool) -> PatchState {
    match (has_backup, index_is_patched) {
        (false, false) => PatchState::Clean,
        (true, true) => PatchState::Patched,
        (true, false) => PatchState::StaleBackup,
        (false, true) => PatchState::PatchedMissingBackup,
    }
}

const PATCHED_BUNDLE_PREFIXES: [&str; 3] = ["TinyPoe2Smoother/", "TinyPoe2Smoother_", "LibGGPK3/"];

fn index_is_patched(index: &BundleIndex) -> bool {
    PATCHED_BUNDLE_PREFIXES
        .iter()
        .any(|prefix| index.has_bundle_prefix(prefix))
}

fn is_patched_bundle(name: &str) -> bool {
    PATCHED_BUNDLE_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn ensure_can_apply(
    patch_state: PatchState,
    backup_path: &Path,
    layout: InstallLayout,
) -> Result<()> {
    match patch_state {
        PatchState::Clean | PatchState::StaleBackup => Ok(()),
        PatchState::Patched => bail!(
            "game is already patched.\n\
             Restore before applying a different patch selection\n\
             Backup: {}",
            backup_path.display()
        ),
        PatchState::PatchedMissingBackup => bail!(
            "game index is already patched by tiny-poe2smoother, but no backup was found.\n\
             {} before applying again",
            repair_hint(layout)
        ),
        PatchState::OtherInstallBackup => bail!(
            "the backup belongs to another install.\n\
             Select that install and Restore it first"
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
    let mut store = BundleStore::new(&game_dir);
    let index = store.open_shared_index()?;
    let query = query.to_ascii_lowercase();

    Ok(index
        .paths()
        .iter()
        .filter(|path| path.to_ascii_lowercase().contains(&query))
        .take(limit)
        .map(|path| PathSearchResult {
            path: path.to_string(),
            has_record: index.file_by_path(path).is_some(),
        })
        .collect())
}

/// Skips `specific_skill_stat_descriptions/`, which only repeats ids from `stat_descriptions.csd`.
pub fn load_stat_catalog(game_dir: Option<PathBuf>) -> Result<Vec<StatCatalogEntry>> {
    crate::timing!("load_stat_catalog_total");
    const PREFIX: &str = "data/statdescriptions/";
    let game_dir = resolve_game_dir(game_dir)?;
    let mut store = BundleStore::new(&game_dir);
    let index = store.open_shared_index()?;
    let entries = index.matching_paths_by(|path| {
        let normalized = path.replace('\\', "/").to_ascii_lowercase();
        normalized
            .strip_prefix(PREFIX)
            .is_some_and(|rest| rest.ends_with(".csd") && !rest.contains('/'))
    });
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

pub fn load_effect_skill_catalog(
    game_dir: Option<PathBuf>,
) -> Result<Vec<EffectSkillCatalogEntry>> {
    crate::timing!("load_effect_skill_catalog_total");
    let game_dir = resolve_game_dir(game_dir)?;
    let mut store = BundleStore::new(&game_dir);
    let index = store.open_shared_index()?;
    skill_catalog_from_index(&store, &index)
}

/// Uses the Skills tab's ownership rules, so no folder appears in both tabs.
pub fn load_other_effect_catalog(
    game_dir: Option<PathBuf>,
) -> Result<Vec<OtherEffectCatalogEntry>> {
    let game_dir = resolve_game_dir(game_dir)?;
    let mut store = BundleStore::new(&game_dir);
    let index = store.open_shared_index()?;
    let skills = skill_catalog_from_index(&store, &index)?;
    Ok(build_other_effect_catalog(index.paths(), &skills))
}

fn skill_catalog_from_index(
    store: &BundleStore,
    index: &BundleIndex,
) -> Result<Vec<EffectSkillCatalogEntry>> {
    let activeskills = store
        .read_file(index, ACTIVESKILLS_DATC64_PATH)
        .context("read activeskills.datc64")?;
    let actiontypes = store
        .read_file(index, ACTIONTYPES_DATC64_PATH)
        .context("read actiontypes.datc64")?;
    let item_visual_effects = store.read_file(index, ITEM_VISUAL_EFFECT_DATC64_PATH).ok();
    let miscanimated = store
        .read_file(index, "data/balance/miscanimated.datc64")
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

/// Reads every monster metadata file, so the GUI loads it only when its tab opens.
pub fn load_monster_effect_catalog(
    game_dir: Option<PathBuf>,
) -> Result<Vec<MonsterEffectCatalogEntry>> {
    crate::timing!("load_monster_effect_catalog_total");
    let game_dir = resolve_game_dir(game_dir)?;
    let mut store = BundleStore::new(&game_dir);
    let index = store.open_shared_index()?;
    build_monster_effect_catalog(&store, &index)
}

pub fn inspect_path(
    game_dir: Option<PathBuf>,
    path: &str,
    byte_limit: usize,
) -> Result<InspectedPath> {
    let game_dir = resolve_game_dir(game_dir)?;
    let mut store = BundleStore::new(&game_dir);
    let index = store.open_shared_index()?;
    let bytes = store.read_file(&index, path)?;
    let limited = bytes.iter().take(byte_limit).copied().collect::<Vec<_>>();
    let text_preview = if bytes.starts_with(&[0xff, 0xfe]) {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
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
    fn apply_and_restore_round_trip_both_layouts() {
        for layout in [InstallLayout::LooseBundles, InstallLayout::ContentGgpk] {
            let temp = tempfile::tempdir().unwrap();
            let game = temp.path().join("game");
            let (source, path, original_data) = write_shadow_fixture(&game, layout);
            let original_source = fs::read(&source).unwrap();
            let backup = BackupStore::for_path(temp.path().join("poe2.bak"));
            let request = PatchRequest {
                game_dir: Some(game.clone()),
                patches: vec![PatchId::Shadow],
                params: PatchParams::default(),
            };
            let report = apply_patches_with_store(request.clone(), backup.clone()).unwrap();
            assert_eq!(report.changed_files, 1);
            assert!(backup.has_backup());
            {
                let mut store = BundleStore::new(&game);
                let index = store.open_index().unwrap();
                assert!(index_is_patched(&index));
                let patched = store.read_file(&index, path).unwrap();
                assert_eq!(
                    crate::patches::decode_utf16(&patched).unwrap(),
                    "{\"shadows_enabled\": false}"
                );
            }
            assert!(apply_patches_with_store(request, backup.clone()).is_err());

            let restored = restore_backup_with_store(game.clone(), backup.clone()).unwrap();
            assert_eq!(restored.restored_files, 1);
            assert!(restored.backup_removed);
            assert!(!backup.has_backup());
            assert_eq!(fs::read(&source).unwrap(), original_source);
            assert!(!game.join("Bundles2/TinyPoe2Smoother").exists());
            let mut store = BundleStore::new(&game);
            let index = store.open_index().unwrap();
            assert!(!index_is_patched(&index));
            assert_eq!(store.read_file(&index, path).unwrap(), original_data);
        }
    }

    fn write_shadow_fixture(
        game: &Path,
        layout: InstallLayout,
    ) -> (PathBuf, &'static str, Vec<u8>) {
        use crate::bundle::store_tests::write_test_ggpk_files;
        fs::create_dir_all(game).unwrap();
        let path = "metadata/environmentsettings/test.env";
        let original_data: Vec<u8> = "\u{feff}{\"shadows_enabled\": true}"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let (index_bytes, bundle_bytes) = patch_fixture(path, &original_data);
        let source = match layout {
            InstallLayout::LooseBundles => {
                fs::create_dir_all(game.join("Bundles2")).unwrap();
                fs::write(game.join("Bundles2/_.index.bin"), &index_bytes).unwrap();
                fs::write(game.join("Bundles2/Base.bundle.bin"), &bundle_bytes).unwrap();
                game.join("Bundles2/_.index.bin")
            }
            InstallLayout::ContentGgpk => {
                write_test_ggpk_files(
                    &game.join("Content.ggpk"),
                    "Bundles2",
                    &[
                        ("_.index.bin", &index_bytes),
                        ("Base.bundle.bin", &bundle_bytes),
                    ],
                );
                game.join("Content.ggpk")
            }
        };
        (source, path, original_data)
    }

    #[test]
    fn failed_apply_that_wrote_nothing_leaves_no_backup() {
        let mut layouts = vec![InstallLayout::ContentGgpk];
        // Read-only folders still accept new files on Windows.
        if cfg!(unix) {
            layouts.push(InstallLayout::LooseBundles);
        }
        for layout in layouts {
            let temp = tempfile::tempdir().unwrap();
            let game = temp.path().join("game");
            let (source, _, _) = write_shadow_fixture(&game, layout);
            let locked = match layout {
                InstallLayout::ContentGgpk => source.clone(),
                InstallLayout::LooseBundles => game.join("Bundles2"),
            };
            let original = fs::read(&source).unwrap();
            let mut permissions = fs::metadata(&locked).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&locked, permissions.clone()).unwrap();
            let backup = BackupStore::for_path(temp.path().join("poe2.bak"));
            let request = PatchRequest {
                game_dir: Some(game.clone()),
                patches: vec![PatchId::Shadow],
                params: PatchParams::default(),
            };

            let err = apply_patches_with_store(request, backup.clone()).unwrap_err();

            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            fs::set_permissions(&locked, permissions).unwrap();
            assert!(
                format!("{err:#}").contains("Nothing was changed"),
                "{err:#}"
            );
            assert!(!backup.has_backup());
            assert_eq!(fs::read(&source).unwrap(), original);
        }
    }

    fn patch_fixture(path: &str, data: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut paths = 1u32.to_le_bytes().to_vec();
        paths.extend_from_slice(path.as_bytes());
        paths.push(0);
        let mut raw = 1u32.to_le_bytes().to_vec();
        raw.extend_from_slice(&4u32.to_le_bytes());
        raw.extend_from_slice(b"Base");
        raw.extend_from_slice(&(data.len() as u32).to_le_bytes());
        raw.extend_from_slice(&1u32.to_le_bytes());
        raw.extend_from_slice(&crate::bundle::filepath_hash(path).to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&(data.len() as u32).to_le_bytes());
        raw.extend_from_slice(&1u32.to_le_bytes());
        raw.extend_from_slice(&0x07E47507B4A92E53u64.to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&(paths.len() as u32).to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&pack_uncompressed_bundle(&paths).unwrap());
        (
            pack_uncompressed_bundle(&raw).unwrap(),
            pack_uncompressed_bundle(data).unwrap(),
        )
    }

    #[test]
    fn apply_and_restore_reject_a_busy_backup_without_writing() {
        let temp = tempfile::tempdir().unwrap();
        let game = fake_install(temp.path(), "game");
        let backup = BackupStore::for_path(temp.path().join("poe2.bak"));
        let original = fs::read(game.join("Bundles2/_.index.bin")).unwrap();
        let lock = backup.lock().unwrap();
        let request = PatchRequest {
            game_dir: Some(game.clone()),
            patches: vec![PatchId::Shadow],
            params: PatchParams::default(),
        };
        assert!(
            apply_patches_with_store(request, backup.clone())
                .unwrap_err()
                .to_string()
                .contains("Another Apply or Restore")
        );
        assert!(
            restore_backup_with_store(game.clone(), backup.clone())
                .unwrap_err()
                .to_string()
                .contains("Another Apply or Restore")
        );
        assert_eq!(
            fs::read(game.join("Bundles2/_.index.bin")).unwrap(),
            original
        );
        assert!(!backup.has_backup());
        drop(lock);
        assert!(backup.lock().is_ok());
    }

    #[test]
    fn ggpk_restore_counts_the_restored_index() {
        use crate::bundle::store_tests::{patch_test_ggpk, write_test_ggpk_files};
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        fs::create_dir_all(&game).unwrap();
        let original = test_index_bytes(&[]);
        write_test_ggpk_files(
            &game.join("Content.ggpk"),
            "Bundles2",
            &[("_.index.bin", original.as_slice())],
        );
        let meta = patch_test_ggpk(
            &game,
            "TinyPoe2Smoother_0",
            &test_index_bytes(&["TinyPoe2Smoother_0"]),
        );
        let backup = BackupStore::for_path(temp.path().join("poe2.bak"));
        backup.ensure_ggpk_backup_meta(&game, meta).unwrap();

        let report = restore_backup_with_store(game.clone(), backup.clone()).unwrap();

        assert_eq!(report.restored_files, 1);
        assert!(report.backup_removed);
        assert!(!backup.has_backup());
        assert_eq!(
            BundleStore::new(&game).read_index_bytes().unwrap(),
            original
        );
    }

    fn patched_test_ggpk(
        root: &Path,
        original_bundles: &[&str],
    ) -> (PathBuf, BackupStore, crate::backup::GgpkBackupMeta) {
        use crate::bundle::store_tests::{patch_test_ggpk, write_test_ggpk_files};
        let game = root.join("game");
        fs::create_dir_all(&game).unwrap();
        write_test_ggpk_files(
            &game.join("Content.ggpk"),
            "Bundles2",
            &[("_.index.bin", test_index_bytes(original_bundles).as_slice())],
        );
        let meta = patch_test_ggpk(
            &game,
            "TinyPoe2Smoother_0",
            &test_index_bytes(&["TinyPoe2Smoother_0"]),
        );
        let backup = BackupStore::for_path(root.join("poe2.bak"));
        backup.ensure_ggpk_backup_meta(&game, meta).unwrap();
        (game, backup, meta)
    }

    fn assert_restore_refused(game: &Path, backup: &BackupStore) {
        let before = fs::read(game.join("Content.ggpk")).unwrap();
        let err = restore_backup_with_store(game.to_path_buf(), backup.clone()).unwrap_err();
        assert!(format!("{err:#}").contains("PackCheck"), "{err:#}");
        assert!(backup.has_backup());
        assert_eq!(fs::read(game.join("Content.ggpk")).unwrap(), before);
    }

    #[test]
    fn ggpk_restore_refuses_when_bundles2_no_longer_points_at_the_patch() {
        use std::io::{Read, Seek, SeekFrom, Write};
        let temp = tempfile::tempdir().unwrap();
        let (game, backup, meta) = patched_test_ggpk(temp.path(), &[]);
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(game.join("Content.ggpk"))
            .unwrap();
        let mut pointer = [0u8; 8];
        file.seek(SeekFrom::Start(meta.bundles2_offset_pos))
            .unwrap();
        file.read_exact(&mut pointer).unwrap();
        let mut pdir = vec![0u8; (meta.appended_end - u64::from_le_bytes(pointer)) as usize];
        file.seek(SeekFrom::Start(u64::from_le_bytes(pointer)))
            .unwrap();
        file.read_exact(&mut pdir).unwrap();
        let moved = file.seek(SeekFrom::End(0)).unwrap();
        file.write_all(&pdir).unwrap();
        file.seek(SeekFrom::Start(meta.bundles2_offset_pos))
            .unwrap();
        file.write_all(&moved.to_le_bytes()).unwrap();
        drop(file);

        assert_restore_refused(&game, &backup);
    }

    #[test]
    fn ggpk_restore_refuses_when_the_original_bundles2_is_gone() {
        use std::io::{Seek, SeekFrom, Write};
        let temp = tempfile::tempdir().unwrap();
        let (game, backup, meta) = patched_test_ggpk(temp.path(), &[]);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(game.join("Content.ggpk"))
            .unwrap();
        file.seek(SeekFrom::Start(meta.original_bundles2_offset + 4))
            .unwrap();
        file.write_all(b"FREE").unwrap();
        drop(file);

        assert_restore_refused(&game, &backup);
    }

    #[test]
    fn ggpk_restore_refuses_when_the_original_index_is_patched() {
        let temp = tempfile::tempdir().unwrap();
        let (game, backup, _) = patched_test_ggpk(temp.path(), &["LibGGPK3/0"]);

        assert_restore_refused(&game, &backup);
    }

    #[test]
    fn ggpk_restore_needs_the_ggpk_backup_data() {
        let temp = tempfile::tempdir().unwrap();
        let (game, backup, _) = patched_test_ggpk(temp.path(), &[]);
        backup.remove().unwrap();
        backup
            .ensure_original_bytes(
                &game,
                &[(PathBuf::from("Bundles2/_.index.bin"), test_index_bytes(&[]))],
            )
            .unwrap();

        assert_restore_refused(&game, &backup);
    }

    #[test]
    fn stale_backup_does_not_block_apply() {
        let backup_path = Path::new("/tmp/poe2.bak");

        let layout = InstallLayout::LooseBundles;
        assert!(ensure_can_apply(PatchState::Clean, backup_path, layout).is_ok());
        assert!(ensure_can_apply(PatchState::StaleBackup, backup_path, layout).is_ok());
    }

    #[test]
    fn currently_patched_index_blocks_apply() {
        let backup_path = Path::new("/tmp/poe2.bak");

        let layout = InstallLayout::ContentGgpk;
        let with_backup = ensure_can_apply(PatchState::Patched, backup_path, layout)
            .unwrap_err()
            .to_string();
        let without_backup =
            ensure_can_apply(PatchState::PatchedMissingBackup, backup_path, layout)
                .unwrap_err()
                .to_string();

        assert!(with_backup.contains("already patched"));
        assert!(without_backup.contains("no backup was found"));
        assert!(without_backup.contains("PackCheck"));
    }

    #[test]
    fn removed_destructive_patches_are_unknown() {
        use crate::patches::parse_patch;

        for name in [
            "zero-effects",
            "remove-players",
            "remove-monsters",
            "clean-terrain",
            "zero-materials",
            "mtx-full",
        ] {
            assert_eq!(parse_patch(name), None, "{name}");
        }
    }

    fn fake_install(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(dir.join("Bundles2")).unwrap();
        fs::write(dir.join("Bundles2/_.index.bin"), test_index_bytes(&[])).unwrap();
        dir
    }

    #[test]
    fn backup_owner_tells_installs_apart() {
        let temp = tempfile::tempdir().unwrap();
        let a = fake_install(temp.path(), "a");
        let b = fake_install(temp.path(), "b");

        assert_eq!(backup_owner(Some(&a), &a), BackupOwner::ThisInstall);
        assert_eq!(
            backup_owner(Some(&a), &b),
            BackupOwner::OtherInstall(a.clone())
        );
        assert_eq!(backup_owner(None, &b), BackupOwner::Unknown);
        assert_eq!(
            backup_owner(Some(&temp.path().join("gone")), &b),
            BackupOwner::Unknown
        );
        #[cfg(unix)]
        {
            let alias = temp.path().join("alias");
            std::os::unix::fs::symlink(&a, &alias).unwrap();
            assert_eq!(backup_owner(Some(&a), &alias), BackupOwner::ThisInstall);
        }
    }

    #[test]
    fn status_names_the_install_that_owns_the_backup() {
        let temp = tempfile::tempdir().unwrap();
        let a = fake_install(temp.path(), "a");
        let b = fake_install(temp.path(), "b");
        let backup = BackupStore::for_path(temp.path().join("poe2.bak"));
        assert_eq!(
            install_patch_state(&backup, &b, false),
            (PatchState::Clean, None)
        );
        backup
            .ensure_original_bytes(
                &a,
                &[(PathBuf::from("Bundles2/_.index.bin"), b"index".to_vec())],
            )
            .unwrap();

        assert_eq!(
            install_patch_state(&backup, &a, true),
            (PatchState::Patched, None)
        );
        assert_eq!(
            install_patch_state(&backup, &a, false),
            (PatchState::StaleBackup, None)
        );
        assert_eq!(
            install_patch_state(&backup, &b, false),
            (
                PatchState::OtherInstallBackup,
                Some(fs::canonicalize(&a).unwrap())
            )
        );
        fs::remove_dir_all(&a).unwrap();
        assert_eq!(
            install_patch_state(&backup, &b, false),
            (PatchState::StaleBackup, None)
        );
    }

    #[test]
    fn stale_restore_removes_leftover_generated_bundles() {
        let temp = tempfile::tempdir().unwrap();
        let game = fake_install(temp.path(), "game");
        let leftover = game.join("Bundles2/TinyPoe2Smoother");
        fs::create_dir_all(&leftover).unwrap();
        fs::write(leftover.join("0.bundle.bin"), b"partial").unwrap();
        let backup = BackupStore::for_path(temp.path().join("poe2.bak"));
        backup
            .ensure_original_bytes(
                &game,
                &[(PathBuf::from("Bundles2/_.index.bin"), b"index".to_vec())],
            )
            .unwrap();

        let report = restore_backup_with_store(game, backup.clone()).unwrap();
        assert!(report.backup_removed);
        assert!(!leftover.exists());
        assert!(!backup.has_backup());
    }

    #[cfg(unix)]
    #[test]
    fn backup_made_through_a_symlink_stays_owned_after_the_link_is_gone() {
        let temp = tempfile::tempdir().unwrap();
        let a = fake_install(temp.path(), "a");
        let b = fake_install(temp.path(), "b");
        let alias = temp.path().join("a-alias");
        std::os::unix::fs::symlink(&a, &alias).unwrap();
        let backup = BackupStore::for_path(temp.path().join("poe2.bak"));
        backup
            .ensure_original_bytes(
                &alias,
                &[(PathBuf::from("Bundles2/_.index.bin"), b"index".to_vec())],
            )
            .unwrap();
        fs::remove_file(&alias).unwrap();

        assert_eq!(
            install_patch_state(&backup, &b, false),
            (
                PatchState::OtherInstallBackup,
                Some(fs::canonicalize(&a).unwrap())
            )
        );
    }

    #[test]
    fn damaged_backup_header_does_not_block_apply() {
        let temp = tempfile::tempdir().unwrap();
        let game = fake_install(temp.path(), "game");
        let backup = BackupStore::for_path(temp.path().join("poe2.bak"));
        fs::write(backup.path(), [0x32, 0x53]).unwrap();
        ensure_backup_is_for(&backup, &game).unwrap();
    }

    #[test]
    fn backup_from_another_install_is_kept_until_that_install_is_gone() {
        let temp = tempfile::tempdir().unwrap();
        let a = fake_install(temp.path(), "a");
        let b = fake_install(temp.path(), "b");
        let backup = BackupStore::for_path(temp.path().join("poe2.bak"));
        backup
            .ensure_original_bytes(
                &a,
                &[(
                    PathBuf::from("Bundles2/_.index.bin"),
                    b"a-original".to_vec(),
                )],
            )
            .unwrap();

        let err = restore_backup_with_store(b.clone(), backup.clone()).unwrap_err();
        assert!(
            err.to_string().contains("belongs to another install"),
            "{err}"
        );
        assert!(backup.has_backup());

        fs::remove_dir_all(&a).unwrap();
        let report = restore_backup_with_store(b, backup.clone()).unwrap();
        assert!(report.backup_removed);
        assert!(!backup.has_backup());
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
        // open_index builds and caches paths from this even when it's empty.
        raw.extend_from_slice(&pack_uncompressed_bundle(&[]).unwrap());
        pack_uncompressed_bundle(&raw).unwrap()
    }
}
