#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod gui;

use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use tiny_poe2smoother::app::{
    apply_patches, load_effect_skill_catalog, load_monster_effect_catalog,
    load_other_effect_catalog, load_stat_catalog, load_status, restore_backup, AppStatus,
    ApplyReport, PatchRequest, RestoreReport,
};
use tiny_poe2smoother::install::display_path;
use tiny_poe2smoother::patches::{
    all_patches, default_color_mods, display_stat_text, merge_with_defaults, parse_patch,
    ColorModEntry, EffectLevel, EffectSkillCatalogEntry, EffectSkillOverride,
    MonsterEffectCatalogEntry, MonsterEffectOverride, OtherEffectCatalogEntry, PatchId,
    PatchParams,
};

const PREFS_KEY: &str = "tiny-poe2smoother.gui.v1";

fn main() -> eframe::Result {
    tiny_poe2smoother::init_tracing();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 720.0])
            .with_min_inner_size([840.0, 620.0])
            .with_icon(gui::icon::app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "tiny-poe2smoother",
        options,
        Box::new(|cc| {
            gui::theme::install_fonts(&cc.egui_ctx);
            gui::theme::install_style(&cc.egui_ctx);
            Ok(Box::new(GuiApp::new(cc.storage)))
        }),
    )
}

#[derive(Clone, Copy)]
enum MessageKind {
    Info,
    Success,
    Error,
}

/// Which tab of the effects editor modal is showing. Remembered across
/// open/close for the session so the editor reopens where the user left off.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EffectsEditorTab {
    Skills,
    Monsters,
    Others,
}

struct GuiApp {
    game_dir_input: String,
    selected_patches: HashSet<PatchId>,
    zoom: f64,
    color_mods: Vec<ColorModEntry>,
    show_color_editor: bool,
    color_search: String,
    stat_catalog: Option<Vec<CatalogRow>>,
    catalog_task: Option<Receiver<Result<Vec<CatalogRow>, String>>>,
    catalog_error: Option<String>,
    stat_catalog_dir: Option<PathBuf>,
    color_filter_key: Option<ColorFilterKey>,
    color_filter_rows: Vec<ColorRowRef>,
    effect_overrides: HashMap<String, EffectLevel>,
    show_effects_editor: bool,
    effects_editor_tab: EffectsEditorTab,
    effects_search: String,
    effect_catalog: Option<Vec<EffectFolderRow>>,
    effect_catalog_task: Option<Receiver<Result<Vec<EffectFolderRow>, String>>>,
    effect_catalog_error: Option<String>,
    effect_catalog_dir: Option<PathBuf>,
    effects_filter_key: Option<(String, usize)>,
    effects_filter_rows: Vec<usize>,
    others_search: String,
    other_catalog: Option<Vec<OtherEffectRow>>,
    other_catalog_task: Option<Receiver<Result<Vec<OtherEffectRow>, String>>>,
    other_catalog_error: Option<String>,
    other_catalog_dir: Option<PathBuf>,
    others_filter_key: Option<(String, usize)>,
    others_filter_rows: Vec<usize>,
    monster_overrides: HashMap<String, EffectLevel>,
    monsters_search: String,
    monster_catalog: Option<Vec<MonsterCatalogRow>>,
    monster_catalog_task: Option<Receiver<Result<Vec<MonsterCatalogRow>, String>>>,
    monster_catalog_error: Option<String>,
    monster_catalog_dir: Option<PathBuf>,
    monsters_filter_key: Option<(String, usize, bool)>,
    monsters_filter_rows: Vec<usize>,
    show_unnamed_monsters: bool,
    status: Option<AppStatus>,
    status_loader: fn(Option<PathBuf>) -> anyhow::Result<AppStatus>,
    message: String,
    message_kind: MessageKind,
    task: Option<Receiver<TaskResult>>,
    busy_label: Option<String>,
    confirm_apply: bool,
    confirm_restore: bool,
    show_game_running_dialog: bool,
    initialized: bool,
}

/// A stat catalog entry. `text` is the human-readable form (markup already
/// collapsed via `display_stat_text`); the lowercase caches mean
/// per-keystroke filtering never re-lowercases the ~20k-entry catalog.
struct CatalogRow {
    stat_id: String,
    text: String,
    stat_id_lower: String,
    text_lower: String,
}

/// One visible row of the color editor list: either a configured entry
/// (index into `color_mods`) or a not-yet-configured catalog suggestion
/// (index into `stat_catalog`).
#[derive(Clone, Copy)]
enum ColorRowRef {
    Config(usize),
    Catalog(usize),
}

/// Filter cache key: query + config length + catalog length. Any of them
/// changing (typing, promoting a catalog row, catalog finishing its load)
/// invalidates the cached row list; color/enabled edits don't.
type ColorFilterKey = (String, usize, usize);

/// One visible skill row in the per-skill effects editor. A row may own
/// several underlying effect folders when the game splits one skill's visuals
/// across buff/explosion/etc. folders.
struct EffectFolderRow {
    folders: Vec<String>,
    active_skill_id: String,
    action_type: String,
    display: String,
    display_lower: String,
    search_lower: String,
}

/// One visible monster row in the per-monster effects editor. A row owns
/// every monster variant (runemarked etc.) sharing its display name.
/// `context` is the muted suffix shown after fallback names from the catalog's
/// nearest derived ancestry and is `None` for table-named rows. `named`
/// mirrors the catalog entry's bit; fallback rows are hidden by default
/// behind the "Show unnamed entities" checkbox.
struct MonsterCatalogRow {
    monster_keys: Vec<String>,
    display: String,
    display_lower: String,
    search_lower: String,
    context: Option<String>,
    named: bool,
}

/// One Others-tab row: a single shared/unmapped folder under
/// `metadata/effects/spells/` (a directory, or an exact `.ao`/`.aoc` file).
struct OtherEffectRow {
    folder: String,
    display: String,
    display_lower: String,
    search_lower: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GuiPrefs {
    game_dir_input: String,
    selected_patches: Vec<String>,
    zoom: f64,
    #[serde(default)]
    color_mods: Vec<ColorModEntry>,
    #[serde(default)]
    effect_skills: Vec<EffectSkillOverride>,
    #[serde(default)]
    monster_effects: Vec<MonsterEffectOverride>,
}

enum TaskResult {
    Status(Result<AppStatus, String>),
    Apply(Result<ApplyReport, String>),
    Restore {
        result: Result<RestoreReport, String>,
        status: Result<AppStatus, String>,
    },
}

impl Default for GuiApp {
    fn default() -> Self {
        let selected_patches = [PatchId::Minimap, PatchId::Fog, PatchId::Rain]
            .into_iter()
            .collect();
        Self {
            game_dir_input: String::new(),
            selected_patches,
            zoom: 2.4,
            color_mods: default_color_mods(),
            show_color_editor: false,
            color_search: String::new(),
            stat_catalog: None,
            catalog_task: None,
            catalog_error: None,
            stat_catalog_dir: None,
            color_filter_key: None,
            color_filter_rows: Vec::new(),
            effect_overrides: HashMap::new(),
            show_effects_editor: false,
            effects_editor_tab: EffectsEditorTab::Skills,
            effects_search: String::new(),
            effect_catalog: None,
            effect_catalog_task: None,
            effect_catalog_error: None,
            effect_catalog_dir: None,
            effects_filter_key: None,
            effects_filter_rows: Vec::new(),
            others_search: String::new(),
            other_catalog: None,
            other_catalog_task: None,
            other_catalog_error: None,
            other_catalog_dir: None,
            others_filter_key: None,
            others_filter_rows: Vec::new(),
            monster_overrides: HashMap::new(),
            monsters_search: String::new(),
            monster_catalog: None,
            monster_catalog_task: None,
            monster_catalog_error: None,
            monster_catalog_dir: None,
            monsters_filter_key: None,
            monsters_filter_rows: Vec::new(),
            show_unnamed_monsters: false,
            status: None,
            status_loader: load_status,
            message: "Ready.".to_string(),
            message_kind: MessageKind::Info,
            task: None,
            busy_label: None,
            confirm_apply: false,
            confirm_restore: false,
            show_game_running_dialog: false,
            initialized: false,
        }
    }
}

impl eframe::App for GuiApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, PREFS_KEY, &self.prefs());
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.initialized {
            self.initialized = true;
            self.spawn_status();
        }

        self.poll_task(ctx);
        self.poll_catalog(ctx);
        self.poll_effect_catalog(ctx);
        self.poll_other_effect_catalog(ctx);
        self.poll_monster_catalog(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        gui::views::draw(self, ui);
    }
}

impl GuiApp {
    fn new(storage: Option<&dyn eframe::Storage>) -> Self {
        storage
            .and_then(|storage| eframe::get_value::<GuiPrefs>(storage, PREFS_KEY))
            .map(Self::from_prefs)
            .unwrap_or_default()
    }

    fn from_prefs(prefs: GuiPrefs) -> Self {
        let selected_patches = prefs
            .selected_patches
            .iter()
            .filter_map(|patch| parse_patch(patch))
            .collect::<HashSet<_>>();
        let selected_patches = if selected_patches.is_empty() {
            [PatchId::Minimap, PatchId::Fog, PatchId::Rain]
                .into_iter()
                .collect()
        } else {
            selected_patches
        };
        // An empty saved list is never a legitimate state (disabling is done
        // via the flag, entries are never removed), so it means "no saved
        // color config yet"; otherwise saved edits win and new defaults from
        // app updates are appended.
        let color_mods = if prefs.color_mods.is_empty() {
            default_color_mods()
        } else {
            merge_with_defaults(prefs.color_mods)
        };
        // Only non-default levels are ever saved; stale folders from older
        // game versions are kept silently (they simply match no path).
        let effect_overrides = prefs
            .effect_skills
            .into_iter()
            .filter(|entry| entry.level != EffectLevel::Reduced)
            .map(|entry| (entry.folder.to_ascii_lowercase(), entry.level))
            .collect();
        // Same policy for monsters: stale keys resolve to no paths at apply.
        let monster_overrides = prefs
            .monster_effects
            .into_iter()
            .filter(|entry| entry.level != EffectLevel::Reduced)
            .map(|entry| (entry.monster.to_ascii_lowercase(), entry.level))
            .collect();
        Self {
            game_dir_input: prefs.game_dir_input,
            selected_patches,
            zoom: prefs.zoom.clamp(1.2, 2.4),
            color_mods,
            effect_overrides,
            monster_overrides,
            ..Self::default()
        }
    }

    fn prefs(&self) -> GuiPrefs {
        let selected_patches = all_patches()
            .iter()
            .filter(|patch| self.selected_patches.contains(&patch.id))
            .map(|patch| patch.name.to_string())
            .collect();
        GuiPrefs {
            game_dir_input: self.game_dir_input.clone(),
            selected_patches,
            zoom: self.zoom,
            color_mods: self.color_mods.clone(),
            effect_skills: self.effect_skill_overrides(),
            monster_effects: self.monster_effect_overrides(),
        }
    }

    /// The non-default per-skill levels as a folder-sorted list (stable
    /// serialization order for prefs and `PatchParams`).
    fn effect_skill_overrides(&self) -> Vec<EffectSkillOverride> {
        let mut overrides: Vec<EffectSkillOverride> = self
            .effect_overrides
            .iter()
            .map(|(folder, level)| EffectSkillOverride {
                folder: folder.clone(),
                level: *level,
            })
            .collect();
        overrides.sort_by(|a, b| a.folder.cmp(&b.folder));
        overrides
    }

    /// The non-default per-monster levels as a key-sorted list (stable
    /// serialization order for prefs and `PatchParams`).
    fn monster_effect_overrides(&self) -> Vec<MonsterEffectOverride> {
        let mut overrides: Vec<MonsterEffectOverride> = self
            .monster_overrides
            .iter()
            .map(|(monster, level)| MonsterEffectOverride {
                monster: monster.clone(),
                level: *level,
            })
            .collect();
        overrides.sort_by(|a, b| a.monster.cmp(&b.monster));
        overrides
    }

    fn is_busy(&self) -> bool {
        self.task.is_some()
    }

    fn set_message(&mut self, message: impl Into<String>, kind: MessageKind) {
        self.message = message.into();
        self.message_kind = kind;
    }

    fn patch_request(&self) -> Result<PatchRequest, String> {
        if self.selected_patches.is_empty() {
            return Err("Select at least one patch.".to_string());
        }
        let mut patches = all_patches()
            .iter()
            .filter(|patch| self.selected_patches.contains(&patch.id))
            .map(|patch| patch.id)
            .collect::<Vec<_>>();
        if patches.contains(&PatchId::Camera) {
            patches.retain(|patch| *patch != PatchId::Camera);
            patches.push(PatchId::Camera);
        }
        if patches.contains(&PatchId::ColorMods)
            && !self.color_mods.iter().any(|entry| entry.enabled)
        {
            return Err(
                "Color mods is selected but no mods are enabled — use Edit colors…".to_string(),
            );
        }
        Ok(PatchRequest {
            game_dir: self.game_dir(),
            patches,
            params: PatchParams {
                zoom: self.zoom,
                color_mods: self.color_mods.clone(),
                effect_skills: self.effect_skill_overrides(),
                monster_effects: self.monster_effect_overrides(),
            },
        })
    }

    fn game_dir(&self) -> Option<PathBuf> {
        let trimmed = self.game_dir_input.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        }
    }

    fn spawn_status(&mut self) {
        let game_dir = self.game_dir();
        let loader = self.status_loader;
        self.status = None;
        self.spawn("Detecting install...", move || {
            TaskResult::Status(loader(game_dir).map_err(|err| err.to_string()))
        });
    }

    fn select_game_dir(&mut self, path: PathBuf) {
        self.game_dir_input = display_path(&path);
        self.spawn_status();
    }

    fn spawn_autodetect(&mut self) {
        let loader = self.status_loader;
        self.status = None;
        self.spawn("Detecting install...", move || {
            TaskResult::Status(loader(None).map_err(|err| err.to_string()))
        });
    }

    fn spawn_apply(&mut self, request: PatchRequest) {
        self.spawn("Applying patches...", move || {
            TaskResult::Apply(apply_patches(request).map_err(|err| err.to_string()))
        });
    }

    fn spawn_restore(&mut self) {
        let game_dir = self.game_dir();
        self.spawn("Restoring backup...", move || {
            let result = restore_backup(game_dir.clone()).map_err(|err| err.to_string());
            let status = load_status(game_dir).map_err(|err| err.to_string());
            TaskResult::Restore { result, status }
        });
    }

    fn spawn(&mut self, label: &str, work: impl FnOnce() -> TaskResult + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        self.task = Some(rx);
        self.busy_label = Some(label.to_string());
        self.set_message(label, MessageKind::Info);
        thread::spawn(move || {
            let _ = tx.send(work());
        });
    }

    fn poll_task(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.task else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(33));
                return;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.task = None;
                self.busy_label = None;
                self.set_message("Background task failed unexpectedly.", MessageKind::Error);
                return;
            }
        };

        self.task = None;
        self.busy_label = None;

        match result {
            TaskResult::Status(result) => match result {
                Ok(status) => {
                    self.apply_status(status);
                    self.set_message("Install detected.", MessageKind::Success);
                }
                Err(err) => self.set_message(err, MessageKind::Error),
            },
            TaskResult::Apply(result) => match result {
                Ok(report) => {
                    self.set_message(
                        format!(
                            "Applied {} file(s). Touched {} bundle/index file(s). Backup: {}",
                            report.changed_files,
                            report.touched_paths.len(),
                            display_path(&report.backup_path)
                        ),
                        MessageKind::Success,
                    );
                    self.spawn_status();
                }
                Err(err) => self.set_message(err, MessageKind::Error),
            },
            TaskResult::Restore { result, status } => {
                match status {
                    Ok(status) => self.apply_status(status),
                    Err(_) => self.status = None,
                }
                match result {
                    Ok(report) => {
                        let message = if report.backup_removed && report.restored_files == 0 {
                            "Removed obsolete backup.".to_string()
                        } else if report.restored_files == 0 {
                            "No backup found.".to_string()
                        } else {
                            format!("Restored {} file(s).", report.restored_files)
                        };
                        self.set_message(message, MessageKind::Success);
                    }
                    Err(err) => self.set_message(err, MessageKind::Error),
                }
            }
        }
    }

    fn apply_status(&mut self, status: AppStatus) {
        self.game_dir_input = display_path(&status.game_dir);
        self.status = Some(status);
    }

    /// Discard a stale stat catalog (and its in-flight load / filter cache)
    /// when the target game dir no longer matches the one it was loaded for.
    /// Overrides/config are left alone — only the display/derived layer resets.
    fn invalidate_stat_catalog_if_stale(&mut self, game_dir: &Option<PathBuf>) {
        if self.stat_catalog_dir.as_deref() != game_dir.as_deref() {
            self.stat_catalog = None;
            self.catalog_task = None;
            self.catalog_error = None;
            self.color_filter_key = None;
            self.color_filter_rows.clear();
        }
    }

    /// Kick off the background stat-catalog load for the color editor if it
    /// hasn't run yet. Deliberately NOT `spawn`: that would set `is_busy()`
    /// and lock the whole UI while the editor should stay usable.
    fn ensure_catalog_loading(&mut self) {
        let game_dir = self.game_dir();
        self.invalidate_stat_catalog_if_stale(&game_dir);
        if self.stat_catalog.is_some() || self.catalog_task.is_some() {
            return;
        }
        self.catalog_error = None;
        let (tx, rx) = mpsc::channel();
        self.catalog_task = Some(rx);
        self.stat_catalog_dir = game_dir.clone();
        thread::spawn(move || {
            let result = load_stat_catalog(game_dir)
                .map(|entries| {
                    entries
                        .into_iter()
                        .map(|entry| {
                            let text = display_stat_text(&entry.text);
                            CatalogRow {
                                stat_id_lower: entry.stat_id.to_lowercase(),
                                text_lower: text.to_lowercase(),
                                stat_id: entry.stat_id,
                                text,
                            }
                        })
                        .collect()
                })
                .map_err(|err| err.to_string());
            let _ = tx.send(result);
        });
    }

    fn poll_catalog(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.catalog_task else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(rows)) => {
                self.catalog_task = None;
                self.stat_catalog = Some(rows);
            }
            Ok(Err(err)) => {
                self.catalog_task = None;
                self.catalog_error = Some(err);
            }
            Err(mpsc::TryRecvError::Empty) => {
                if self.show_color_editor {
                    ctx.request_repaint_after(std::time::Duration::from_millis(33));
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.catalog_task = None;
                self.catalog_error = Some("mod catalog task failed".to_string());
            }
        }
    }

    /// English display text for a configured stat id, if the catalog knows it.
    fn catalog_text(&self, stat_id: &str) -> Option<&str> {
        let catalog = self.stat_catalog.as_ref()?;
        let idx = catalog
            .binary_search_by(|row| row.stat_id.as_str().cmp(stat_id))
            .ok()?;
        let text = catalog[idx].text.as_str();
        (!text.is_empty()).then_some(text)
    }

    /// Rebuild `color_filter_rows` (the editor's visible rows: configured
    /// entries first, then catalog suggestions not yet configured) if stale.
    /// Cached; rebuilt only when the query, config set, or catalog changes.
    fn refresh_color_filter(&mut self) {
        let catalog_len = self.stat_catalog.as_ref().map_or(0, Vec::len);
        let fresh = self
            .color_filter_key
            .as_ref()
            .is_some_and(|(query, mods, catalog)| {
                *query == self.color_search
                    && *mods == self.color_mods.len()
                    && *catalog == catalog_len
            });
        if !fresh {
            let query = gui::search::SearchQuery::parse(&self.color_search);
            let matches =
                |stat_id_lower: &str, text_lower: &str| query.matches(stat_id_lower, text_lower);
            let mut rows = Vec::new();
            for (idx, entry) in self.color_mods.iter().enumerate() {
                let text_lower = self
                    .catalog_text(&entry.stat_id)
                    .map(str::to_lowercase)
                    .unwrap_or_default();
                if matches(&entry.stat_id.to_lowercase(), &text_lower) {
                    rows.push(ColorRowRef::Config(idx));
                }
            }
            if let Some(catalog) = &self.stat_catalog {
                let configured: HashSet<&str> = self
                    .color_mods
                    .iter()
                    .map(|entry| entry.stat_id.as_str())
                    .collect();
                for (idx, row) in catalog.iter().enumerate() {
                    if configured.contains(row.stat_id.as_str()) {
                        continue;
                    }
                    if matches(&row.stat_id_lower, &row.text_lower) {
                        rows.push(ColorRowRef::Catalog(idx));
                    }
                }
            }
            self.color_filter_rows = rows;
            self.color_filter_key = Some((
                self.color_search.clone(),
                self.color_mods.len(),
                catalog_len,
            ));
        }
    }

    /// Discard a stale effect catalog (and its in-flight load / filter cache)
    /// when the target game dir no longer matches the one it was loaded for.
    /// `effect_overrides` intentionally survive — they're folder-keyed and stale
    /// folders simply match no path (see `from_prefs`).
    fn invalidate_effect_catalog_if_stale(&mut self, game_dir: &Option<PathBuf>) {
        if self.effect_catalog_dir.as_deref() != game_dir.as_deref() {
            self.effect_catalog = None;
            self.effect_catalog_task = None;
            self.effect_catalog_error = None;
            self.effects_filter_key = None;
            self.effects_filter_rows.clear();
        }
    }

    /// Kick off the background skill-folder load for the effects editor if it
    /// hasn't run yet. Like `ensure_catalog_loading`, deliberately NOT
    /// `spawn`: the editor should stay usable while it loads.
    fn ensure_effect_catalog_loading(&mut self) {
        let game_dir = self.game_dir();
        self.invalidate_effect_catalog_if_stale(&game_dir);
        if self.effect_catalog.is_some() || self.effect_catalog_task.is_some() {
            return;
        }
        self.effect_catalog_error = None;
        let (tx, rx) = mpsc::channel();
        self.effect_catalog_task = Some(rx);
        self.effect_catalog_dir = game_dir.clone();
        thread::spawn(move || {
            let result = load_effect_skill_catalog(game_dir)
                .map(effect_skill_catalog_rows)
                .map_err(|err| err.to_string());
            let _ = tx.send(result);
        });
    }

    fn poll_effect_catalog(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.effect_catalog_task else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(rows)) => {
                self.effect_catalog_task = None;
                self.effect_catalog = Some(rows);
            }
            Ok(Err(err)) => {
                self.effect_catalog_task = None;
                self.effect_catalog_error = Some(err);
            }
            Err(mpsc::TryRecvError::Empty) => {
                if self.show_effects_editor {
                    ctx.request_repaint_after(std::time::Duration::from_millis(33));
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.effect_catalog_task = None;
                self.effect_catalog_error = Some("skill catalog task failed".to_string());
            }
        }
    }

    /// Rebuild `effects_filter_rows` (indices into `effect_catalog` matching
    /// the query) if stale. Level edits never invalidate the cache — the
    /// visible set only depends on the query and the catalog.
    fn refresh_effects_filter(&mut self) {
        let catalog_len = self.effect_catalog.as_ref().map_or(0, Vec::len);
        let fresh = self
            .effects_filter_key
            .as_ref()
            .is_some_and(|(query, catalog)| {
                *query == self.effects_search && *catalog == catalog_len
            });
        if !fresh {
            let query = gui::search::SearchQuery::parse(&self.effects_search);
            self.effects_filter_rows = self
                .effect_catalog
                .as_deref()
                .unwrap_or_default()
                .iter()
                .enumerate()
                .filter(|(_, row)| query.matches(&row.search_lower, &row.display_lower))
                .map(|(idx, _)| idx)
                .collect();
            self.effects_filter_key = Some((self.effects_search.clone(), catalog_len));
        }
    }

    fn apply_effect_level_to_filtered_rows(&mut self, level: EffectLevel) -> bool {
        self.refresh_effects_filter();
        let catalog = self.effect_catalog.as_deref().unwrap_or_default();
        let folders: Vec<String> = self
            .effects_filter_rows
            .iter()
            .flat_map(|&idx| catalog[idx].folders.iter().cloned())
            .collect();
        self.set_skill_folders_level(&folders, level)
    }

    fn set_skill_folders_level(&mut self, folders: &[String], level: EffectLevel) -> bool {
        if level == EffectLevel::Full {
            for folder in folders {
                self.effect_overrides
                    .insert(folder.to_ascii_lowercase(), level);
            }
            return true;
        }
        let removal: Vec<String> = folders
            .iter()
            .map(|folder| folder.to_ascii_lowercase())
            .collect();
        if !self.expand_effect_ancestors_for_removal(&removal) {
            return false;
        }
        for folder in removal {
            self.effect_overrides.remove(&folder);
        }
        true
    }

    fn effect_level_for_folders(&self, folders: &[String]) -> Option<EffectLevel> {
        let mut levels = folders
            .iter()
            .map(|folder| self.effective_effect_level(folder));
        let first = levels.next()?;
        levels.all(|level| level == first).then_some(first)
    }

    fn effective_effect_level(&self, folder: &str) -> EffectLevel {
        let folder = folder.to_ascii_lowercase();
        if self.effect_overrides.get(&folder).copied() == Some(EffectLevel::Full) {
            return EffectLevel::Full;
        }
        let bytes = folder.as_bytes();
        for (idx, byte) in bytes.iter().enumerate() {
            if *byte == b'/'
                && self.effect_overrides.get(&folder[..idx]).copied() == Some(EffectLevel::Full)
            {
                return EffectLevel::Full;
            }
        }
        EffectLevel::Reduced
    }

    /// The Skills/Others editor stays reachable for Particles-only selections,
    /// whose referenced particles are still protected by Full scopes.
    fn effects_editor_accessible(&self) -> bool {
        use tiny_poe2smoother::patches::PatchId;
        self.selected_patches.contains(&PatchId::Effects)
            || self.selected_patches.contains(&PatchId::Particles)
    }

    /// Stored broad Full parents covering any folder in `removal`.
    fn effect_ancestors_for_removal(&self, removal: &[String]) -> Vec<String> {
        self.effect_overrides
            .iter()
            .filter(|(_, level)| **level == EffectLevel::Full)
            .map(|(key, _)| key.clone())
            .filter(|key| {
                let key_lower = key.to_ascii_lowercase();
                removal
                    .iter()
                    .any(|folder| is_folder_ancestor(&key_lower, &folder.to_ascii_lowercase()))
            })
            .collect()
    }

    /// True when reducing `removal` is safe with the loaded catalogs.
    /// Only broad-Ancestor migration needs catalogs: both tabs, except
    /// ground-only ancestors which need just Others.
    fn can_reduce_effect_folders(&mut self, removal: &[String]) -> bool {
        if removal.is_empty() {
            return true;
        }
        let game_dir = self.game_dir();
        self.invalidate_effect_catalog_if_stale(&game_dir);
        self.invalidate_other_effect_catalog_if_stale(&game_dir);
        let removal: Vec<String> = removal
            .iter()
            .map(|folder| folder.to_ascii_lowercase())
            .collect();
        let ancestors = self.effect_ancestors_for_removal(&removal);
        if ancestors.is_empty() {
            return true;
        }
        for ancestor in &ancestors {
            if is_ground_effects_scope(ancestor) {
                if self.other_catalog.is_none() {
                    return false;
                }
            } else if self.effect_catalog.is_none() || self.other_catalog.is_none() {
                return false;
            }
        }
        true
    }

    /// Runs on every repaint of the main view, so it stays linear in
    /// catalog + overrides.
    fn kept_original_effect_skill_count(&self) -> usize {
        let catalog = self.effect_catalog.as_deref().unwrap_or(&[]);
        // A row counts as kept when any of its folders is overridden to Full —
        // a mixed row (stale prefs after the game regrouped folders) still
        // renders original visuals for part of the skill.
        let catalog_kept = catalog
            .iter()
            .filter(|row| {
                row.folders
                    .iter()
                    .any(|folder| self.effective_effect_level(folder) == EffectLevel::Full)
            })
            .count();
        let cataloged: HashSet<&str> = catalog
            .iter()
            .flat_map(|row| row.folders.iter().map(String::as_str))
            .collect();
        // Others folders share this map; once the Others catalog is known they
        // (and broad parents covering them) count under Others, not Skills.
        let other_folders: Vec<&str> = self
            .other_catalog
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|row| row.folder.as_str())
            .collect();
        // Stale folders absent from the catalog count once each: their
        // grouping is unknowable, and dropping them would hide skills the
        // user really kept original.
        let uncataloged_full = self
            .effect_overrides
            .iter()
            .filter(|(folder, level)| {
                **level == EffectLevel::Full
                    && !cataloged.contains(folder.as_str())
                    && !is_other_related_override(folder, &other_folders)
                    && !cataloged
                        .iter()
                        .any(|known| is_folder_ancestor(folder.as_str(), known))
            })
            .count();
        catalog_kept + uncataloged_full
    }

    /// Drop a stale Others catalog + filter cache on game-dir change.
    /// `effect_overrides` survive; stale folders simply match no path.
    fn invalidate_other_effect_catalog_if_stale(&mut self, game_dir: &Option<PathBuf>) {
        if self.other_catalog_dir.as_deref() != game_dir.as_deref() {
            self.other_catalog = None;
            self.other_catalog_task = None;
            self.other_catalog_error = None;
            self.others_filter_key = None;
            self.others_filter_rows.clear();
        }
    }

    /// Start the background Others-catalog load if needed; the editor stays
    /// usable while it loads, like `ensure_effect_catalog_loading`.
    fn ensure_other_effect_catalog_loading(&mut self) {
        let game_dir = self.game_dir();
        self.invalidate_other_effect_catalog_if_stale(&game_dir);
        if self.other_catalog.is_some() || self.other_catalog_task.is_some() {
            return;
        }
        self.other_catalog_error = None;
        let (tx, rx) = mpsc::channel();
        self.other_catalog_task = Some(rx);
        self.other_catalog_dir = game_dir.clone();
        thread::spawn(move || {
            let result = load_other_effect_catalog(game_dir)
                .map(other_effect_catalog_rows)
                .map_err(|err| err.to_string());
            let _ = tx.send(result);
        });
    }

    fn poll_other_effect_catalog(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.other_catalog_task else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(rows)) => {
                self.other_catalog_task = None;
                self.other_catalog = Some(rows);
            }
            Ok(Err(err)) => {
                self.other_catalog_task = None;
                self.other_catalog_error = Some(err);
            }
            Err(mpsc::TryRecvError::Empty) => {
                if self.show_effects_editor {
                    ctx.request_repaint_after(std::time::Duration::from_millis(33));
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.other_catalog_task = None;
                self.other_catalog_error = Some("shared effect catalog task failed".to_string());
            }
        }
    }

    /// Rebuild cached Others-filter rows if the query or catalog changed.
    fn refresh_others_filter(&mut self) {
        let catalog_len = self.other_catalog.as_ref().map_or(0, Vec::len);
        let fresh = self
            .others_filter_key
            .as_ref()
            .is_some_and(|(query, catalog)| {
                *query == self.others_search && *catalog == catalog_len
            });
        if !fresh {
            let query = gui::search::SearchQuery::parse(&self.others_search);
            self.others_filter_rows = self
                .other_catalog
                .as_deref()
                .unwrap_or_default()
                .iter()
                .enumerate()
                .filter(|(_, row)| query.matches(&row.search_lower, &row.display_lower))
                .map(|(idx, _)| idx)
                .collect();
            self.others_filter_key = Some((self.others_search.clone(), catalog_len));
        }
    }

    /// Effective level for one Others folder: Full via an exact entry or any
    /// covering broad parent; missing entries are Reduced.
    fn other_effect_level(&self, folder: &str) -> EffectLevel {
        self.effective_effect_level(folder)
    }

    /// Set one Others folder. Reducing under an inherited Full parent first
    /// expands it into explicit Full siblings (both catalogs); with a required
    /// catalog missing nothing changes.
    fn set_other_effect_level(&mut self, folder: &str, level: EffectLevel) -> bool {
        let folder = folder.to_ascii_lowercase();
        if level == EffectLevel::Full {
            self.effect_overrides.insert(folder, EffectLevel::Full);
            return true;
        }
        if !self.expand_effect_ancestors_for_removal(std::slice::from_ref(&folder)) {
            return false;
        }
        self.effect_overrides.remove(&folder);
        true
    }

    /// Bulk `set_other_effect_level` over the tab's filtered rows. Reduced
    /// returns false unchanged when a required catalog is missing.
    fn apply_other_effect_level_to_filtered_rows(&mut self, level: EffectLevel) -> bool {
        self.refresh_others_filter();
        let catalog = self.other_catalog.as_deref().unwrap_or_default();
        let folders: Vec<String> = self
            .others_filter_rows
            .iter()
            .map(|&idx| catalog[idx].folder.clone())
            .collect();
        if level == EffectLevel::Full {
            for folder in folders {
                self.effect_overrides.insert(folder, level);
            }
            true
        } else {
            if !self.expand_effect_ancestors_for_removal(&folders) {
                return false;
            }
            for folder in folders {
                self.effect_overrides.remove(&folder);
            }
            true
        }
    }

    /// Expand covering broad Full parents into explicit Full entries for
    /// catalog descendants outside `removal`, then drop them. Spans the union
    /// of Skills + Others folders (`supports` can cover both); ground-only
    /// ancestors need just Others. False + unchanged if a catalog is missing.
    fn expand_effect_ancestors_for_removal(&mut self, removal: &[String]) -> bool {
        if removal.is_empty() {
            return true;
        }
        let game_dir = self.game_dir();
        self.invalidate_effect_catalog_if_stale(&game_dir);
        self.invalidate_other_effect_catalog_if_stale(&game_dir);
        let removal: Vec<String> = removal
            .iter()
            .map(|folder| folder.to_ascii_lowercase())
            .collect();
        let ancestors = self.effect_ancestors_for_removal(&removal);
        if ancestors.is_empty() {
            return true;
        }
        for ancestor in &ancestors {
            if is_ground_effects_scope(ancestor) {
                if self.other_catalog.is_none() {
                    return false;
                }
            } else if self.effect_catalog.is_none() || self.other_catalog.is_none() {
                return false;
            }
        }
        let skill_folders: Vec<String> = self
            .effect_catalog
            .as_deref()
            .unwrap_or_default()
            .iter()
            .flat_map(|row| row.folders.iter().cloned())
            .collect();
        let other_folders: Vec<String> = self
            .other_catalog
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|row| row.folder.clone())
            .collect();
        for ancestor in ancestors {
            if !is_ground_effects_scope(&ancestor) {
                for folder in &skill_folders {
                    if !is_folder_equal_or_descendant(folder, &ancestor) {
                        continue;
                    }
                    if removal
                        .iter()
                        .any(|target| is_folder_equal_or_descendant(folder, target))
                    {
                        continue;
                    }
                    self.effect_overrides
                        .entry(folder.clone())
                        .or_insert(EffectLevel::Full);
                }
            }
            for folder in &other_folders {
                if !is_folder_equal_or_descendant(folder, &ancestor) {
                    continue;
                }
                if removal
                    .iter()
                    .any(|target| is_folder_equal_or_descendant(folder, target))
                {
                    continue;
                }
                self.effect_overrides
                    .entry(folder.clone())
                    .or_insert(EffectLevel::Full);
            }
            self.effect_overrides.remove(&ancestor);
        }
        true
    }

    /// Count Others rows at effective Full. Broad parents count through their
    /// rows, not extra; folders unknown to both catalogs count under Skills.
    fn kept_original_other_effect_count(&self) -> usize {
        let catalog = self.other_catalog.as_deref().unwrap_or(&[]);
        if catalog.is_empty() {
            return 0;
        }
        catalog
            .iter()
            .filter(|row| self.other_effect_level(&row.folder) == EffectLevel::Full)
            .count()
    }

    /// Discard a stale monster catalog (and its in-flight load / filter
    /// cache) when the target game dir no longer matches the one it was
    /// loaded for. `monster_overrides` intentionally survive — stale keys
    /// simply resolve to no paths at apply (see `from_prefs`).
    fn invalidate_monster_catalog_if_stale(&mut self, game_dir: &Option<PathBuf>) {
        if self.monster_catalog_dir.as_deref() != game_dir.as_deref() {
            self.monster_catalog = None;
            self.monster_catalog_task = None;
            self.monster_catalog_error = None;
            self.monsters_filter_key = None;
            self.monsters_filter_rows.clear();
        }
    }

    /// Kick off the catalog load backing the active effects-editor tab.
    /// Called on editor open and on every tab switch, so it also revalidates
    /// a catalog loaded for a different game dir.
    fn ensure_active_effects_tab_loading(&mut self) {
        match self.effects_editor_tab {
            EffectsEditorTab::Skills | EffectsEditorTab::Others => {
                self.ensure_effect_catalog_loading();
                self.ensure_other_effect_catalog_loading();
            }
            EffectsEditorTab::Monsters => self.ensure_monster_catalog_loading(),
        }
    }

    /// Kick off the background monster-catalog load for the monsters tab
    /// if it hasn't run yet. Like the other catalog loads, deliberately NOT
    /// `spawn`: the editor should stay usable while it loads. Unlike the
    /// skill catalog this reads every monster metadata file, so it is only
    /// started when its tab is actually shown.
    fn ensure_monster_catalog_loading(&mut self) {
        let game_dir = self.game_dir();
        self.invalidate_monster_catalog_if_stale(&game_dir);
        if self.monster_catalog.is_some() || self.monster_catalog_task.is_some() {
            return;
        }
        self.monster_catalog_error = None;
        let (tx, rx) = mpsc::channel();
        self.monster_catalog_task = Some(rx);
        self.monster_catalog_dir = game_dir.clone();
        thread::spawn(move || {
            let result = load_monster_effect_catalog(game_dir)
                .map(monster_effect_catalog_rows)
                .map_err(|err| err.to_string());
            let _ = tx.send(result);
        });
    }

    fn poll_monster_catalog(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.monster_catalog_task else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(rows)) => {
                self.monster_catalog_task = None;
                self.monster_catalog = Some(rows);
            }
            Ok(Err(err)) => {
                self.monster_catalog_task = None;
                self.monster_catalog_error = Some(err);
            }
            Err(mpsc::TryRecvError::Empty) => {
                // Gated on the whole editor, not just the monsters tab: a
                // load left in flight by a tab switch should still finish
                // animating its spinner when the user switches back.
                if self.show_effects_editor {
                    ctx.request_repaint_after(std::time::Duration::from_millis(33));
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.monster_catalog_task = None;
                self.monster_catalog_error = Some("monster catalog task failed".to_string());
            }
        }
    }

    /// Rebuild `monsters_filter_rows` (indices into `monster_catalog`
    /// matching the query) if stale. Level edits never invalidate the cache —
    /// the visible set only depends on the query, the catalog, and the
    /// "Show unnamed entities" checkbox (fallback rows are hidden while it
    /// is off; overridden hidden rows are surfaced by a hint, not the list).
    fn refresh_monsters_filter(&mut self) {
        let catalog_len = self.monster_catalog.as_ref().map_or(0, Vec::len);
        let fresh =
            self.monsters_filter_key
                .as_ref()
                .is_some_and(|(query, catalog, show_unnamed)| {
                    *query == self.monsters_search
                        && *catalog == catalog_len
                        && *show_unnamed == self.show_unnamed_monsters
                });
        if !fresh {
            let query = gui::search::SearchQuery::parse(&self.monsters_search);
            self.monsters_filter_rows = self
                .monster_catalog
                .as_deref()
                .unwrap_or_default()
                .iter()
                .enumerate()
                .filter(|(_, row)| {
                    (row.named || self.show_unnamed_monsters)
                        && query.matches(&row.search_lower, &row.display_lower)
                })
                .map(|(idx, _)| idx)
                .collect();
            self.monsters_filter_key = Some((
                self.monsters_search.clone(),
                catalog_len,
                self.show_unnamed_monsters,
            ));
        }
    }

    /// Fallback (unnamed) rows in the catalog — what the "Show unnamed
    /// entities" checkbox governs and counts.
    fn unnamed_monster_row_count(&self) -> usize {
        self.monster_catalog
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|row| !row.named)
            .count()
    }

    /// Unnamed rows with any variant keeping original visuals. While the
    /// checkbox hides unnamed rows these are still active settings, so the
    /// monsters tab flags them with a hint instead of hiding them silently.
    fn overridden_unnamed_monster_row_count(&self) -> usize {
        self.monster_catalog
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|row| {
                !row.named
                    && row
                        .monster_keys
                        .iter()
                        .any(|key| self.monster_overrides.contains_key(key))
            })
            .count()
    }

    fn apply_monster_level_to_filtered_rows(&mut self, level: EffectLevel) {
        self.refresh_monsters_filter();
        let catalog = self.monster_catalog.as_deref().unwrap_or_default();
        let keys: Vec<String> = self
            .monsters_filter_rows
            .iter()
            .flat_map(|&idx| catalog[idx].monster_keys.iter().cloned())
            .collect();
        for key in keys {
            if level == EffectLevel::Reduced {
                self.monster_overrides.remove(&key);
            } else {
                self.monster_overrides.insert(key, level);
            }
        }
    }

    fn monster_level_for_keys(&self, keys: &[String]) -> Option<EffectLevel> {
        let mut levels = keys
            .iter()
            .map(|key| self.monster_overrides.get(key).copied().unwrap_or_default());
        let first = levels.next()?;
        levels.all(|level| level == first).then_some(first)
    }

    /// Runs on every repaint of the main view, so it stays linear in
    /// catalog + overrides. Same counting policy as the skill caption: a row
    /// counts once however many of its variants are kept, and overridden
    /// keys absent from the catalog count once each.
    fn kept_original_monster_count(&self) -> usize {
        let catalog = self.monster_catalog.as_deref().unwrap_or(&[]);
        let catalog_kept = catalog
            .iter()
            .filter(|row| {
                row.monster_keys
                    .iter()
                    .any(|key| self.monster_overrides.contains_key(key))
            })
            .count();
        let cataloged: HashSet<&str> = catalog
            .iter()
            .flat_map(|row| row.monster_keys.iter().map(String::as_str))
            .collect();
        let uncataloged_full = self
            .monster_overrides
            .iter()
            .filter(|(key, level)| {
                **level == EffectLevel::Full && !cataloged.contains(key.as_str())
            })
            .count();
        catalog_kept + uncataloged_full
    }
}

fn monster_effect_catalog_rows(entries: Vec<MonsterEffectCatalogEntry>) -> Vec<MonsterCatalogRow> {
    // Rows are grouped per (display, family): when several families share a
    // display name ("Daemon"), suffix the family so the rows stay tellable
    // apart in the list.
    let mut display_counts: HashMap<String, usize> = HashMap::new();
    for entry in &entries {
        *display_counts
            .entry(entry.display.to_lowercase())
            .or_default() += 1;
    }
    entries
        .into_iter()
        .map(|entry| {
            let context = monster_context_suffix(&entry.derived_context);
            let search_lower = format!(
                "{} {} {} {} {}",
                entry.display,
                entry.family,
                entry.monster_keys.join(" "),
                entry.search_aliases.join(" "),
                entry.derived_context.join(" ")
            )
            .to_lowercase();
            let display_lower = entry.display.to_lowercase();
            let display = if display_counts.get(&display_lower).copied().unwrap_or(0) > 1 {
                format!("{} ({})", entry.display, entry.family)
            } else {
                entry.display
            };
            MonsterCatalogRow {
                monster_keys: entry.monster_keys,
                display_lower,
                display,
                search_lower,
                context,
                named: entry.named,
            }
        })
        .collect()
}

/// Visible fallback context is capped, while the complete derived set remains
/// in `search_lower` for local ambiguous identities.
fn monster_context_suffix(context: &[String]) -> Option<String> {
    if context.is_empty() {
        return None;
    }
    let mut suffix = context[..context.len().min(2)].join(" / ");
    if context.len() > 2 {
        suffix.push_str(" …");
    }
    Some(suffix)
}

fn effect_skill_catalog_rows(entries: Vec<EffectSkillCatalogEntry>) -> Vec<EffectFolderRow> {
    entries
        .into_iter()
        .map(|entry| {
            let search_lower = format!(
                "{} {} {} {}",
                entry.display.to_lowercase(),
                entry.active_skill_id,
                entry.action_type.to_lowercase(),
                entry.folders.join(" ")
            );
            EffectFolderRow {
                folders: entry.folders,
                active_skill_id: entry.active_skill_id,
                action_type: entry.action_type,
                display_lower: entry.display.to_lowercase(),
                display: entry.display,
                search_lower,
            }
        })
        .collect()
}

/// True when `ancestor` is a strict `/`-separated prefix of `path`
/// (`ground_effects` vs `ground_effects/fire`). Folders are lowercase
/// relative paths, so a plain byte comparison is enough.
fn is_folder_ancestor(ancestor: &str, path: &str) -> bool {
    path.len() > ancestor.len()
        && path.starts_with(ancestor)
        && path.as_bytes().get(ancestor.len()) == Some(&b'/')
}

fn is_folder_equal_or_descendant(path: &str, ancestor: &str) -> bool {
    path == ancestor || is_folder_ancestor(ancestor, path)
}

/// True for ground-effect scopes whose containers backend never assigns to
/// skills, so cross-tab expansion for them needs only the Others catalog.
fn is_ground_effects_scope(folder: &str) -> bool {
    matches!(
        folder
            .split('/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "ground_effects" | "ground_effects_v2" | "ground_effects_v3"
    )
}

/// True when an override belongs to Others: equal to, above, or below an
/// Others folder. Empty lists claim nothing, preserving the skill fallback.
fn is_other_related_override(folder: &str, other_folders: &[&str]) -> bool {
    !other_folders.is_empty()
        && other_folders.iter().any(|other| {
            *other == folder
                || is_folder_ancestor(other, folder)
                || is_folder_ancestor(folder, other)
        })
}

fn other_effect_catalog_rows(entries: Vec<OtherEffectCatalogEntry>) -> Vec<OtherEffectRow> {
    entries
        .into_iter()
        .map(|entry| {
            let folder = entry.folder.to_ascii_lowercase();
            let search_lower = format!("{} {folder}", entry.display).to_lowercase();
            OtherEffectRow {
                display_lower: entry.display.to_lowercase(),
                display: entry.display,
                search_lower,
                folder,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patched_status(game_dir: PathBuf) -> AppStatus {
        AppStatus {
            index_path: game_dir.join("Bundles2/_.index.bin"),
            index_display_path: "Bundles2/_.index.bin".to_string(),
            game_dir,
            install_layout: tiny_poe2smoother::install::InstallLayout::LooseBundles,
            indexed_paths: 1,
            backup_path: PathBuf::from("test-backup.bak"),
            has_backup: true,
            patch_state: tiny_poe2smoother::app::PatchState::Patched,
        }
    }

    fn finish_status_task(app: &mut GuiApp) {
        // Wait for the real worker, then feed its result through normal UI polling.
        let result = app
            .task
            .take()
            .expect("status task should have started")
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("status task should finish");
        let (tx, rx) = mpsc::channel();
        tx.send(result).unwrap();
        app.task = Some(rx);
        app.poll_task(&egui::Context::default());
        assert!(!app.is_busy());
    }

    #[test]
    fn autodetect_ignores_stale_textbox_and_replaces_it_on_success() {
        let mut app = GuiApp {
            game_dir_input: "deleted-install".to_string(),
            status_loader: |path| {
                assert_eq!(path, None);
                Ok(patched_status(PathBuf::from("detected-steam-install")))
            },
            ..GuiApp::default()
        };

        app.spawn_autodetect();
        finish_status_task(&mut app);

        assert_eq!(app.game_dir_input, "detected-steam-install");
        assert_eq!(
            app.status.unwrap().game_dir,
            PathBuf::from("detected-steam-install")
        );
    }

    #[test]
    fn failed_autodetect_clears_stale_status_and_preserves_textbox() {
        let mut app = GuiApp {
            game_dir_input: "deleted-install".to_string(),
            status: Some(patched_status(PathBuf::from("deleted-install"))),
            status_loader: |path| {
                assert_eq!(path, None);
                anyhow::bail!("could not autodetect install")
            },
            ..GuiApp::default()
        };

        app.spawn_autodetect();
        assert!(app.status.is_none());
        finish_status_task(&mut app);

        assert!(app.status.is_none());
        assert_eq!(app.message, "could not autodetect install");
        assert_eq!(app.game_dir_input, "deleted-install");
    }

    #[test]
    fn browse_loads_custom_install_status_needed_for_restore() {
        let mut app = GuiApp {
            status_loader: |path| {
                assert_eq!(path, Some(PathBuf::from("custom-install")));
                Ok(patched_status(path.unwrap()))
            },
            ..GuiApp::default()
        };

        app.select_game_dir(PathBuf::from("custom-install"));
        finish_status_task(&mut app);

        assert_eq!(app.game_dir_input, "custom-install");
        assert!(app.status.as_ref().unwrap().patch_state.can_restore());
    }

    #[test]
    fn validation_uses_typed_path_and_clears_stale_status_on_failure() {
        let mut app = GuiApp {
            game_dir_input: "  invalid-custom-install  ".to_string(),
            status: Some(patched_status(PathBuf::from("previous-install"))),
            status_loader: |path| {
                assert_eq!(path, Some(PathBuf::from("invalid-custom-install")));
                anyhow::bail!("invalid install")
            },
            ..GuiApp::default()
        };

        app.spawn_status();
        assert!(app.status.is_none());
        finish_status_task(&mut app);

        assert!(app.status.is_none());
        assert_eq!(app.message, "invalid install");
        assert_eq!(app.game_dir_input, "  invalid-custom-install  ");
    }

    #[test]
    fn prefs_restore_valid_patch_names_and_ignore_unknown_entries() {
        let app = GuiApp::from_prefs(GuiPrefs {
            game_dir_input: r"D:\SteamLibrary\steamapps\common\Path of Exile 2".to_string(),
            selected_patches: vec!["fog".to_string(), "unknown".to_string()],
            zoom: 9.0,
            color_mods: Vec::new(),
            effect_skills: Vec::new(),
            monster_effects: Vec::new(),
        });

        assert_eq!(
            app.game_dir_input,
            r"D:\SteamLibrary\steamapps\common\Path of Exile 2"
        );
        assert!(app.selected_patches.contains(&PatchId::Fog));
        assert_eq!(app.selected_patches.len(), 1);
        assert_eq!(app.zoom, 2.4);
        // No saved color config -> defaults, all enabled.
        assert_eq!(app.color_mods, default_color_mods());
    }

    #[test]
    fn prefs_merge_saved_color_mods_with_defaults() {
        let saved = vec![ColorModEntry {
            stat_id: "map_monsters_damage_+%".to_string(),
            color: [1, 2, 3],
            enabled: false,
        }];
        let app = GuiApp::from_prefs(GuiPrefs {
            game_dir_input: String::new(),
            selected_patches: vec!["fog".to_string()],
            zoom: 2.4,
            color_mods: saved,
            effect_skills: Vec::new(),
            monster_effects: Vec::new(),
        });

        let edited = app
            .color_mods
            .iter()
            .find(|entry| entry.stat_id == "map_monsters_damage_+%")
            .unwrap();
        assert_eq!(edited.color, [1, 2, 3]);
        assert!(!edited.enabled);
        // Defaults the user never saw are appended.
        assert_eq!(app.color_mods.len(), default_color_mods().len());
    }

    #[test]
    fn prefs_round_trip_effect_skill_overrides_and_drop_reduced_entries() {
        let app = GuiApp::from_prefs(GuiPrefs {
            game_dir_input: String::new(),
            selected_patches: vec!["fog".to_string()],
            zoom: 2.4,
            color_mods: Vec::new(),
            effect_skills: vec![
                EffectSkillOverride {
                    folder: "arc_02".to_string(),
                    level: EffectLevel::Reduced,
                },
                EffectSkillOverride {
                    folder: "fireball".to_string(),
                    level: EffectLevel::Full,
                },
            ],
            monster_effects: Vec::new(),
        });

        assert_eq!(
            app.effect_overrides.get("fireball"),
            Some(&EffectLevel::Full)
        );
        // Explicit Reduced entries are meaningless and dropped on load.
        assert!(!app.effect_overrides.contains_key("arc_02"));

        // Saved back folder-sorted, non-default only.
        assert_eq!(
            app.prefs().effect_skills,
            vec![EffectSkillOverride {
                folder: "fireball".to_string(),
                level: EffectLevel::Full,
            }]
        );
    }

    #[test]
    fn legacy_effect_level_pref_deserializes_to_default() {
        #[derive(Default)]
        struct TestStorage {
            values: HashMap<String, String>,
        }

        impl eframe::Storage for TestStorage {
            fn get_string(&self, key: &str) -> Option<String> {
                self.values.get(key).cloned()
            }

            fn set_string(&mut self, key: &str, value: String) {
                self.values.insert(key.to_string(), value);
            }

            fn remove_string(&mut self, key: &str) {
                self.values.remove(key);
            }

            fn flush(&mut self) {}
        }

        let old_level = concat!("hid", "den");
        let mut storage = TestStorage::default();
        eframe::set_value(
            &mut storage,
            PREFS_KEY,
            &GuiPrefs {
                game_dir_input: String::new(),
                selected_patches: vec!["fog".to_string()],
                zoom: 2.4,
                color_mods: Vec::new(),
                effect_skills: vec![EffectSkillOverride {
                    folder: "fireball".to_string(),
                    level: EffectLevel::Reduced,
                }],
                monster_effects: Vec::new(),
            },
        );
        let saved = storage.values.get_mut(PREFS_KEY).unwrap();
        *saved = saved.replace("reduced", old_level);

        let prefs = eframe::get_value::<GuiPrefs>(&storage, PREFS_KEY).unwrap();
        assert_eq!(prefs.effect_skills[0].level, EffectLevel::Reduced);
        let app = GuiApp::from_prefs(prefs);
        assert!(app.effect_overrides.is_empty());
        assert!(app.prefs().effect_skills.is_empty());
    }

    #[test]
    fn patch_request_embeds_overrides() {
        let mut app = GuiApp {
            selected_patches: [PatchId::Effects].into_iter().collect(),
            ..GuiApp::default()
        };
        // All-Full is no longer rejected at the GUI layer — the apply-side
        // bail is the single source of truth. The Full override is still
        // embedded verbatim.
        app.effect_overrides
            .insert("fireball".to_string(), EffectLevel::Full);
        let request = app.patch_request().unwrap();
        assert_eq!(
            request.params.effect_skills,
            vec![EffectSkillOverride {
                folder: "fireball".to_string(),
                level: EffectLevel::Full,
            }]
        );

        app.effect_overrides.clear();
        let request = app.patch_request().unwrap();
        assert!(request.params.effect_skills.is_empty());
    }

    fn monster_row(display: &str, keys: &[&str]) -> MonsterCatalogRow {
        let monster_keys: Vec<String> = keys.iter().map(|key| key.to_string()).collect();
        MonsterCatalogRow {
            display_lower: display.to_lowercase(),
            search_lower: format!("{} {}", display.to_lowercase(), monster_keys.join(" ")),
            display: display.to_string(),
            monster_keys,
            context: None,
            named: true,
        }
    }

    fn unnamed_monster_row(display: &str, keys: &[&str]) -> MonsterCatalogRow {
        MonsterCatalogRow {
            named: false,
            ..monster_row(display, keys)
        }
    }

    #[test]
    fn prefs_round_trip_monster_overrides_and_drop_reduced_entries() {
        let mother = "metadata/monsters/anchorite/anchoritemother/anchoritemother";
        let app = GuiApp::from_prefs(GuiPrefs {
            game_dir_input: String::new(),
            selected_patches: vec!["fog".to_string()],
            zoom: 2.4,
            color_mods: Vec::new(),
            effect_skills: Vec::new(),
            monster_effects: vec![
                MonsterEffectOverride {
                    monster: "metadata/monsters/boghulk/boghulk".to_string(),
                    level: EffectLevel::Reduced,
                },
                MonsterEffectOverride {
                    // Mixed case in old prefs still keys correctly.
                    monster: mother.to_uppercase(),
                    level: EffectLevel::Full,
                },
            ],
        });

        assert_eq!(app.monster_overrides.get(mother), Some(&EffectLevel::Full));
        // Explicit Reduced entries are meaningless and dropped on load.
        assert!(!app
            .monster_overrides
            .contains_key("metadata/monsters/boghulk/boghulk"));

        // Saved back key-sorted, non-default only.
        assert_eq!(
            app.prefs().monster_effects,
            vec![MonsterEffectOverride {
                monster: mother.to_string(),
                level: EffectLevel::Full,
            }]
        );
    }

    #[test]
    fn patch_request_embeds_monster_overrides() {
        let mut app = GuiApp {
            selected_patches: [PatchId::Effects].into_iter().collect(),
            ..GuiApp::default()
        };
        app.monster_overrides.insert(
            "metadata/monsters/boghulk/boghulk".to_string(),
            EffectLevel::Full,
        );

        let request = app.patch_request().unwrap();

        assert_eq!(
            request.params.monster_effects,
            vec![MonsterEffectOverride {
                monster: "metadata/monsters/boghulk/boghulk".to_string(),
                level: EffectLevel::Full,
            }]
        );
    }

    #[test]
    fn monsters_filter_matches_display_and_key_text() {
        let mut app = GuiApp {
            monster_catalog: Some(vec![
                monster_row(
                    "Filthy First-born",
                    &["metadata/monsters/anchorite/anchoritemother/anchoritemother"],
                ),
                monster_row("Bog Hulk", &["metadata/monsters/boghulk/boghulk"]),
            ]),
            ..GuiApp::default()
        };

        app.monsters_search = "anchoritemother".to_string();
        app.refresh_monsters_filter();
        assert_eq!(app.monsters_filter_rows, vec![0]);

        app.monsters_search.clear();
        app.refresh_monsters_filter();
        assert_eq!(app.monsters_filter_rows, vec![0, 1]);
    }

    #[test]
    fn monster_bulk_level_applies_to_filtered_rows_only() {
        let mother = "metadata/monsters/anchorite/anchoritemother/anchoritemother";
        let runemarked =
            "metadata/monsters/anchorite/anchoritemother/runemarked/anchoritemotherrunemarked";
        let mut app = GuiApp {
            monster_catalog: Some(vec![
                monster_row("Filthy First-born", &[mother, runemarked]),
                monster_row("Bog Hulk", &["metadata/monsters/boghulk/boghulk"]),
            ]),
            ..GuiApp::default()
        };

        app.monsters_search = "first-born".to_string();
        app.apply_monster_level_to_filtered_rows(EffectLevel::Full);
        assert_eq!(app.monster_overrides.get(mother), Some(&EffectLevel::Full));
        assert_eq!(
            app.monster_overrides.get(runemarked),
            Some(&EffectLevel::Full)
        );
        assert!(!app
            .monster_overrides
            .contains_key("metadata/monsters/boghulk/boghulk"));

        app.apply_monster_level_to_filtered_rows(EffectLevel::Reduced);
        assert!(app.monster_overrides.is_empty());
    }

    #[test]
    fn kept_original_monster_count_groups_rows_and_keeps_stale_keys() {
        let mother = "metadata/monsters/anchorite/anchoritemother/anchoritemother";
        let runemarked =
            "metadata/monsters/anchorite/anchoritemother/runemarked/anchoritemotherrunemarked";
        let mut app = GuiApp::default();
        app.monster_overrides
            .insert(mother.to_string(), EffectLevel::Full);
        app.monster_overrides
            .insert(runemarked.to_string(), EffectLevel::Full);
        // Without a catalog every overridden key counts once.
        assert_eq!(app.kept_original_monster_count(), 2);

        // With the catalog the two variants collapse into one row; a stale
        // key absent from the catalog still counts.
        app.monster_catalog = Some(vec![monster_row(
            "Filthy First-born",
            &[mother, runemarked],
        )]);
        assert_eq!(app.kept_original_monster_count(), 1);
        app.monster_overrides.insert(
            "metadata/monsters/removed/removed".to_string(),
            EffectLevel::Full,
        );
        assert_eq!(app.kept_original_monster_count(), 2);

        // A mixed row (only one variant still Full) stays counted.
        app.monster_overrides.remove(runemarked);
        assert_eq!(app.kept_original_monster_count(), 2);
    }

    #[test]
    fn monster_catalog_invalidates_only_when_game_dir_changes() {
        let mut app = GuiApp {
            monster_catalog: Some(vec![monster_row(
                "Bog Hulk",
                &["metadata/monsters/boghulk/boghulk"],
            )]),
            monster_catalog_dir: Some(PathBuf::from("/install/A")),
            monsters_filter_key: Some(("q".to_string(), 1, false)),
            monsters_filter_rows: vec![0],
            ..GuiApp::default()
        };
        app.monster_overrides.insert(
            "metadata/monsters/boghulk/boghulk".to_string(),
            EffectLevel::Full,
        );

        // Same dir: nothing is discarded.
        app.invalidate_monster_catalog_if_stale(&Some(PathBuf::from("/install/A")));
        assert!(app.monster_catalog.is_some());
        assert_eq!(app.monsters_filter_rows, vec![0]);

        // Different dir: catalog + filter cache reset, overrides untouched.
        app.invalidate_monster_catalog_if_stale(&Some(PathBuf::from("/install/B")));
        assert!(app.monster_catalog.is_none());
        assert!(app.monster_catalog_task.is_none());
        assert!(app.monster_catalog_error.is_none());
        assert!(app.monsters_filter_key.is_none());
        assert!(app.monsters_filter_rows.is_empty());
        assert_eq!(
            app.monster_overrides
                .get("metadata/monsters/boghulk/boghulk"),
            Some(&EffectLevel::Full)
        );
    }

    #[test]
    fn monster_catalog_rows_carry_search_text_and_variant_keys() {
        let rows = monster_effect_catalog_rows(vec![MonsterEffectCatalogEntry {
            display: "Filthy First-born".to_string(),
            monster_keys: vec![
                "metadata/monsters/anchorite/anchoritemother/anchoritemother".to_string(),
            ],
            family: "anchorite".to_string(),
            search_aliases: vec!["shroomlady".to_string()],
            derived_context: Vec::new(),
            named: true,
        }]);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].display, "Filthy First-born");
        assert_eq!(rows[0].display_lower, "filthy first-born");
        assert!(rows[0].search_lower.contains("anchorite"));
        assert!(rows[0].search_lower.contains("anchoritemother"));
        // Hand-verified aliases are searchable but never shown as the name.
        assert!(rows[0].search_lower.contains("shroomlady"));
        assert!(!rows[0].display.contains("shroomlady"));
    }

    fn catalog_entry(
        display: &str,
        family: &str,
        key: &str,
        named: bool,
        aliases: &[&str],
        context: &[&str],
    ) -> MonsterEffectCatalogEntry {
        MonsterEffectCatalogEntry {
            display: display.to_string(),
            monster_keys: vec![key.to_string()],
            family: family.to_string(),
            search_aliases: aliases.iter().map(|alias| alias.to_string()).collect(),
            derived_context: context.iter().map(|name| name.to_string()).collect(),
            named,
        }
    }

    #[test]
    fn monsters_filter_scopes_exact_and_poe_style_search_to_local_context() {
        let alpha = "metadata/monsters/family/alpha/alpha";
        let beta = "metadata/monsters/family/beta/beta";
        let helper = "metadata/monsters/family/beta/helper";
        let mut app = GuiApp {
            monster_catalog: Some(monster_effect_catalog_rows(vec![
                catalog_entry("Alpha Monster", "family", alpha, true, &[], &[]),
                catalog_entry("Beta Monster", "family", beta, true, &[], &[]),
                catalog_entry("helper", "family", helper, false, &[], &["Beta Monster"]),
            ])),
            ..GuiApp::default()
        };

        app.monsters_search = "beta monster".to_string();
        app.refresh_monsters_filter();
        assert_eq!(app.monsters_filter_rows, vec![1]);

        // Toggling unnamed rows invalidates the populated filter cache.
        app.show_unnamed_monsters = true;
        app.refresh_monsters_filter();
        assert_eq!(app.monsters_filter_rows, vec![1, 2]);

        app.monsters_search = "\"Beta Monster\"".to_string();
        app.refresh_monsters_filter();
        assert_eq!(app.monsters_filter_rows, vec![1, 2]);

        app.monsters_search = "\"Alpha Monster\"".to_string();
        app.refresh_monsters_filter();
        assert_eq!(app.monsters_filter_rows, vec![0]);
        assert!(!app.monster_catalog.as_ref().unwrap()[2]
            .search_lower
            .contains("alpha monster"));
    }

    #[test]
    fn monster_bulk_actions_do_not_touch_sibling_context_helpers() {
        let alpha = "metadata/monsters/family/alpha/alpha";
        let beta = "metadata/monsters/family/beta/beta";
        let helper = "metadata/monsters/family/beta/helper";
        let mut app = GuiApp {
            monster_catalog: Some(monster_effect_catalog_rows(vec![
                catalog_entry("Alpha Monster", "family", alpha, true, &[], &[]),
                catalog_entry("Beta Monster", "family", beta, true, &[], &[]),
                catalog_entry("helper", "family", helper, false, &[], &["Beta Monster"]),
            ])),
            monsters_search: "\"Alpha Monster\"".to_string(),
            show_unnamed_monsters: true,
            ..GuiApp::default()
        };

        app.refresh_monsters_filter();
        let filtered_keys: Vec<&str> = app
            .monsters_filter_rows
            .iter()
            .flat_map(|&idx| {
                app.monster_catalog.as_ref().unwrap()[idx]
                    .monster_keys
                    .iter()
            })
            .map(String::as_str)
            .collect();
        assert_eq!(filtered_keys, vec![alpha]);

        app.monster_overrides
            .insert(helper.to_string(), EffectLevel::Full);
        app.apply_monster_level_to_filtered_rows(EffectLevel::Reduced);
        assert_eq!(app.monster_overrides.get(helper), Some(&EffectLevel::Full));

        app.monster_overrides.remove(helper);
        app.apply_monster_level_to_filtered_rows(EffectLevel::Full);
        assert!(!app.monster_overrides.contains_key(helper));
        assert_eq!(app.monster_overrides.get(alpha), Some(&EffectLevel::Full));
    }

    #[test]
    fn monsters_filter_hides_unnamed_rows_until_toggled() {
        let mut app = GuiApp {
            monster_catalog: Some(vec![
                monster_row("Bog Hulk", &["metadata/monsters/boghulk/boghulk"]),
                unnamed_monster_row(
                    "baronphase 2 wolf",
                    &["metadata/monsters/baron/phase2/baronphase2wolf"],
                ),
            ]),
            ..GuiApp::default()
        };

        app.refresh_monsters_filter();
        assert_eq!(app.monsters_filter_rows, vec![0]);
        assert_eq!(app.unnamed_monster_row_count(), 1);

        app.show_unnamed_monsters = true;
        app.refresh_monsters_filter();
        assert_eq!(app.monsters_filter_rows, vec![0, 1]);
    }

    #[test]
    fn monster_bulk_level_skips_hidden_unnamed_rows_and_hint_counts_them() {
        let hulk = "metadata/monsters/boghulk/boghulk";
        let wolf = "metadata/monsters/baron/phase2/baronphase2wolf";
        let mut app = GuiApp {
            monster_catalog: Some(vec![
                monster_row("Bog Hulk", &[hulk]),
                unnamed_monster_row("baronphase 2 wolf", &[wolf]),
            ]),
            ..GuiApp::default()
        };
        // Old-config style override on a row that is hidden by default.
        app.monster_overrides
            .insert(wolf.to_string(), EffectLevel::Full);

        // The active override on the hidden row is surfaced by the hint
        // count rather than the row list.
        app.refresh_monsters_filter();
        assert_eq!(app.monsters_filter_rows, vec![0]);
        assert_eq!(app.overridden_unnamed_monster_row_count(), 1);

        // "Check all" with unnamed rows hidden touches only visible rows:
        // the hidden override survives.
        app.apply_monster_level_to_filtered_rows(EffectLevel::Reduced);
        assert_eq!(app.monster_overrides.get(wolf), Some(&EffectLevel::Full));

        // "Uncheck all" likewise only reaches the visible named row.
        app.apply_monster_level_to_filtered_rows(EffectLevel::Full);
        assert_eq!(app.monster_overrides.get(hulk), Some(&EffectLevel::Full));

        // With the checkbox on the bulk buttons reach everything again.
        app.show_unnamed_monsters = true;
        app.apply_monster_level_to_filtered_rows(EffectLevel::Reduced);
        assert!(app.monster_overrides.is_empty());
        assert_eq!(app.overridden_unnamed_monster_row_count(), 0);
    }

    #[test]
    fn monster_catalog_rows_attach_context_suffix_to_fallback_rows_only() {
        assert_eq!(
            monster_context_suffix(&[
                "Count Geonor".to_string(),
                "Geonor, the Putrid Wolf".to_string(),
                "Geonor Guard".to_string(),
            ]),
            Some("Count Geonor / Geonor, the Putrid Wolf …".to_string())
        );
        assert_eq!(monster_context_suffix(&[]), None);

        let rows = monster_effect_catalog_rows(vec![
            catalog_entry(
                "Count Geonor",
                "baron",
                "metadata/monsters/baron/a",
                true,
                &[],
                &[],
            ),
            catalog_entry(
                "Geonor, the Putrid Wolf",
                "baron",
                "metadata/monsters/baron/b",
                true,
                &[],
                &[],
            ),
            catalog_entry(
                "Geonor Guard",
                "baron",
                "metadata/monsters/baron/c",
                true,
                &[],
                &[],
            ),
            catalog_entry(
                "baronphase 2 wolf",
                "baron",
                "metadata/monsters/baron/phase2/wolf",
                false,
                &[],
                &["Count Geonor", "Geonor, the Putrid Wolf", "Geonor Guard"],
            ),
            catalog_entry(
                "atziriphase 1",
                "atziri",
                "metadata/monsters/atziri/p1",
                false,
                &[],
                &["atziri"],
            ),
        ]);

        // Named rows have no suffix; fallback rows use the catalog-derived
        // nearest context, capped at two visible names.
        assert!(rows.iter().take(3).all(|row| row.context.is_none()));
        assert_eq!(
            rows[3].context.as_deref(),
            Some("Count Geonor / Geonor, the Putrid Wolf …")
        );
        assert_eq!(rows[4].context.as_deref(), Some("atziri"));
    }

    #[test]
    fn monster_catalog_rows_suffix_family_on_duplicate_display_names() {
        let entry = |display: &str, family: &str, key: &str| MonsterEffectCatalogEntry {
            display: display.to_string(),
            monster_keys: vec![key.to_string()],
            family: family.to_string(),
            search_aliases: Vec::new(),
            derived_context: Vec::new(),
            named: true,
        };
        let rows = monster_effect_catalog_rows(vec![
            entry("Daemon", "daemon", "metadata/monsters/daemon/daemon"),
            entry(
                "Daemon",
                "leaguedelirium",
                "metadata/monsters/leaguedelirium/deliriumskilldaemoncold",
            ),
            entry("Bog Hulk", "boghulk", "metadata/monsters/boghulk/boghulk"),
        ]);

        // Duplicate display names get the family suffix; unique ones do not.
        assert_eq!(rows[0].display, "Daemon (daemon)");
        assert_eq!(rows[1].display, "Daemon (leaguedelirium)");
        assert_eq!(rows[2].display, "Bog Hulk");
        // The filter still keys off the raw display text.
        assert!(rows.iter().take(2).all(|row| row.display_lower == "daemon"));
    }

    #[test]
    fn effects_filter_matches_folder_and_display_text() {
        let mut app = GuiApp {
            effect_catalog: Some(
                [
                    ("cold_herald_of_ice", "Herald of Ice"),
                    ("fireball", "Fireball"),
                ]
                .into_iter()
                .map(|(folder, display)| {
                    let display = display.to_string();
                    EffectFolderRow {
                        folders: vec![folder.to_string()],
                        active_skill_id: display.to_lowercase().replace(' ', "_"),
                        action_type: display.replace(' ', ""),
                        display_lower: display.to_lowercase(),
                        display,
                        search_lower: folder.to_string(),
                    }
                })
                .collect(),
            ),
            ..GuiApp::default()
        };

        app.effects_search = "herald ice".to_string();
        app.refresh_effects_filter();
        assert_eq!(app.effects_filter_rows, vec![0]);

        app.effects_search.clear();
        app.refresh_effects_filter();
        assert_eq!(app.effects_filter_rows, vec![0, 1]);
    }

    #[test]
    fn effect_catalog_invalidates_only_when_game_dir_changes() {
        let mut app = GuiApp {
            effect_catalog: Some(vec![EffectFolderRow {
                folders: vec!["fireball".to_string()],
                active_skill_id: "fireball".to_string(),
                action_type: "GreaterFireball".to_string(),
                display: "Fireball".to_string(),
                display_lower: "fireball".to_string(),
                search_lower: "fireball greaterfireball".to_string(),
            }]),
            effect_catalog_dir: Some(PathBuf::from("/install/A")),
            effects_filter_key: Some(("q".to_string(), 1)),
            effects_filter_rows: vec![0],
            ..GuiApp::default()
        };
        app.effect_overrides
            .insert("fireball".to_string(), EffectLevel::Full);

        // Same dir: nothing is discarded.
        app.invalidate_effect_catalog_if_stale(&Some(PathBuf::from("/install/A")));
        assert!(app.effect_catalog.is_some());
        assert!(app.effects_filter_key.is_some());
        assert_eq!(app.effects_filter_rows, vec![0]);

        // Different dir: catalog + filter cache reset, overrides untouched.
        app.invalidate_effect_catalog_if_stale(&Some(PathBuf::from("/install/B")));
        assert!(app.effect_catalog.is_none());
        assert!(app.effect_catalog_task.is_none());
        assert!(app.effect_catalog_error.is_none());
        assert!(app.effects_filter_key.is_none());
        assert!(app.effects_filter_rows.is_empty());
        assert_eq!(
            app.effect_overrides.get("fireball"),
            Some(&EffectLevel::Full)
        );
    }

    #[test]
    fn effect_bulk_level_applies_to_filtered_rows_only() {
        let mut app = GuiApp {
            effect_catalog: Some(vec![
                EffectFolderRow {
                    folders: vec!["fire_heraldofash".to_string(), "herald_of_fire".to_string()],
                    active_skill_id: "herald_of_ash".to_string(),
                    action_type: "HeraldOfAsh".to_string(),
                    display: "Herald of Ash".to_string(),
                    display_lower: "herald of ash".to_string(),
                    search_lower:
                        "herald of ash herald_of_ash heraldofash fire_heraldofash herald_of_fire"
                            .to_string(),
                },
                EffectFolderRow {
                    folders: vec!["fireball".to_string()],
                    active_skill_id: "fireball".to_string(),
                    action_type: "GreaterFireball".to_string(),
                    display: "Fireball".to_string(),
                    display_lower: "fireball".to_string(),
                    search_lower: "fireball greaterfireball".to_string(),
                },
            ]),
            ..GuiApp::default()
        };

        app.effects_search = "herald".to_string();
        app.apply_effect_level_to_filtered_rows(EffectLevel::Full);
        assert_eq!(
            app.effect_overrides.get("fire_heraldofash"),
            Some(&EffectLevel::Full)
        );
        assert_eq!(
            app.effect_overrides.get("herald_of_fire"),
            Some(&EffectLevel::Full)
        );
        assert!(!app.effect_overrides.contains_key("fireball"));

        app.apply_effect_level_to_filtered_rows(EffectLevel::Reduced);
        assert!(app.effect_overrides.is_empty());
    }

    #[test]
    fn kept_original_count_uses_skill_rows_when_catalog_is_loaded() {
        let mut app = GuiApp {
            effect_catalog: Some(vec![
                EffectFolderRow {
                    folders: vec!["lightning_herald".to_string(), "herald_of_thunder".to_string()],
                    active_skill_id: "herald_of_thunder".to_string(),
                    action_type: "HeraldOfThunder".to_string(),
                    display: "Herald of Thunder".to_string(),
                    display_lower: "herald of thunder".to_string(),
                    search_lower: "herald of thunder herald_of_thunder heraldofthunder lightning_herald herald_of_thunder".to_string(),
                },
                EffectFolderRow {
                    folders: vec!["fireball".to_string()],
                    active_skill_id: "fireball".to_string(),
                    action_type: "GreaterFireball".to_string(),
                    display: "Fireball".to_string(),
                    display_lower: "fireball".to_string(),
                    search_lower: "fireball greaterfireball".to_string(),
                },
            ]),
            ..GuiApp::default()
        };
        app.effect_overrides
            .insert("lightning_herald".to_string(), EffectLevel::Full);
        app.effect_overrides
            .insert("herald_of_thunder".to_string(), EffectLevel::Full);
        assert_eq!(app.kept_original_effect_skill_count(), 1);

        app.effect_overrides
            .insert("fireball".to_string(), EffectLevel::Full);
        assert_eq!(app.kept_original_effect_skill_count(), 2);

        // A mixed row (only one of the skill's folders still Full) keeps
        // rendering original visuals, so it stays counted.
        app.effect_overrides.remove("herald_of_thunder");
        assert_eq!(app.kept_original_effect_skill_count(), 2);
    }

    #[test]
    fn kept_original_count_falls_back_to_folders_without_catalog_rows() {
        let mut app = GuiApp::default();
        app.effect_overrides
            .insert("lightning_herald".to_string(), EffectLevel::Full);
        app.effect_overrides
            .insert("herald_of_thunder".to_string(), EffectLevel::Full);
        // No catalog and an empty catalog count the same way: per folder.
        assert_eq!(app.kept_original_effect_skill_count(), 2);
        app.effect_catalog = Some(Vec::new());
        assert_eq!(app.kept_original_effect_skill_count(), 2);
    }

    #[test]
    fn kept_original_count_preserves_folder_fallbacks() {
        let mut app = GuiApp::default();
        app.effect_overrides
            .insert("lightning_herald".to_string(), EffectLevel::Full);
        app.effect_overrides
            .insert("herald_of_thunder".to_string(), EffectLevel::Full);
        assert_eq!(app.kept_original_effect_skill_count(), 2);

        app.effect_catalog = Some(vec![EffectFolderRow {
            folders: vec!["fireball".to_string()],
            active_skill_id: "fireball".to_string(),
            action_type: "GreaterFireball".to_string(),
            display: "Fireball".to_string(),
            display_lower: "fireball".to_string(),
            search_lower: "fireball greaterfireball".to_string(),
        }]);
        app.effect_overrides
            .insert("fireball".to_string(), EffectLevel::Full);
        assert_eq!(app.kept_original_effect_skill_count(), 3);
    }

    #[test]
    fn effect_catalog_rows_keep_skill_first_grouping() {
        let rows = effect_skill_catalog_rows(vec![
            EffectSkillCatalogEntry {
                active_skill_id: "herald_of_ash".to_string(),
                display: "Herald of Ash".to_string(),
                action_type: "HeraldOfAsh".to_string(),
                folders: vec!["fire_heraldofash".to_string(), "herald_of_fire".to_string()],
            },
            EffectSkillCatalogEntry {
                active_skill_id: "fireball".to_string(),
                display: "Fireball".to_string(),
                action_type: "GreaterFireball".to_string(),
                folders: vec!["fireball".to_string()],
            },
        ]);

        assert_eq!(rows.len(), 2);
        let herald = rows
            .iter()
            .find(|row| row.display == "Herald of Ash")
            .unwrap();
        assert_eq!(
            herald.folders,
            vec!["fire_heraldofash".to_string(), "herald_of_fire".to_string()]
        );
        assert_eq!(herald.active_skill_id, "herald_of_ash");
        assert_eq!(herald.action_type, "HeraldOfAsh");
        assert!(herald.search_lower.contains("herald_of_ash"));
        assert!(herald.search_lower.contains("heraldofash"));
        assert!(herald.search_lower.contains("fire_heraldofash"));
        assert!(herald.search_lower.contains("herald_of_fire"));
    }

    fn other_catalog_entries() -> Vec<OtherEffectCatalogEntry> {
        vec![
            OtherEffectCatalogEntry {
                folder: "ground_effects/fire".to_string(),
                display: "Ground Fire".to_string(),
            },
            OtherEffectCatalogEntry {
                folder: "ground_effects/cold".to_string(),
                display: "Ground Cold".to_string(),
            },
            OtherEffectCatalogEntry {
                folder: "ambient_sparkles.ao".to_string(),
                display: "Ambient Sparkles".to_string(),
            },
        ]
    }

    #[test]
    fn other_effect_catalog_rows_lowercase_folders_and_index_search_text() {
        let rows = other_effect_catalog_rows(other_catalog_entries());

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].folder, "ground_effects/fire");
        assert_eq!(rows[0].display, "Ground Fire");
        assert_eq!(rows[0].display_lower, "ground fire");
        assert!(rows[0].search_lower.contains("ground fire"));
        assert!(rows[0].search_lower.contains("ground_effects/fire"));
        assert_eq!(rows[2].folder, "ambient_sparkles.ao");
    }

    #[test]
    fn other_effect_level_reports_inherited_legacy_full_parents() {
        let mut app = GuiApp {
            other_catalog: Some(other_effect_catalog_rows(other_catalog_entries())),
            ..GuiApp::default()
        };
        assert_eq!(
            app.other_effect_level("ground_effects/fire"),
            EffectLevel::Reduced
        );

        app.effect_overrides
            .insert("ground_effects".to_string(), EffectLevel::Full);
        assert_eq!(
            app.other_effect_level("ground_effects/fire"),
            EffectLevel::Full
        );
        assert_eq!(
            app.other_effect_level("ground_effects/cold"),
            EffectLevel::Full
        );
        assert_eq!(
            app.other_effect_level("ambient_sparkles.ao"),
            EffectLevel::Reduced
        );
    }

    #[test]
    fn other_toggle_to_reduced_expands_legacy_parent_preserving_siblings() {
        let mut app = GuiApp {
            other_catalog: Some(other_effect_catalog_rows(other_catalog_entries())),
            ..GuiApp::default()
        };
        app.effect_overrides
            .insert("ground_effects".to_string(), EffectLevel::Full);

        app.set_other_effect_level("ground_effects/fire", EffectLevel::Reduced);

        assert!(!app.effect_overrides.contains_key("ground_effects"));
        assert!(!app.effect_overrides.contains_key("ground_effects/fire"));
        assert_eq!(
            app.effect_overrides.get("ground_effects/cold"),
            Some(&EffectLevel::Full)
        );
        assert_eq!(
            app.other_effect_level("ground_effects/fire"),
            EffectLevel::Reduced
        );
        assert_eq!(
            app.other_effect_level("ground_effects/cold"),
            EffectLevel::Full
        );
        assert!(!app.effect_overrides.contains_key("ambient_sparkles.ao"));
    }

    #[test]
    fn other_toggle_without_catalog_never_loses_legacy_parent() {
        let mut app = GuiApp::default();
        app.effect_overrides
            .insert("ground_effects".to_string(), EffectLevel::Full);

        // Without a catalog the broad parent cannot be expanded, so it stays.
        app.set_other_effect_level("ground_effects/fire", EffectLevel::Reduced);

        assert_eq!(
            app.effect_overrides.get("ground_effects"),
            Some(&EffectLevel::Full)
        );
    }

    #[test]
    fn other_bulk_level_applies_to_filtered_rows_only_and_keeps_skills() {
        let mut app = GuiApp {
            other_catalog: Some(other_effect_catalog_rows(other_catalog_entries())),
            effect_catalog: Some(vec![EffectFolderRow {
                folders: vec!["fireball".to_string()],
                active_skill_id: "fireball".to_string(),
                action_type: "GreaterFireball".to_string(),
                display: "Fireball".to_string(),
                display_lower: "fireball".to_string(),
                search_lower: "fireball greaterfireball".to_string(),
            }]),
            ..GuiApp::default()
        };
        app.effect_overrides
            .insert("fireball".to_string(), EffectLevel::Full);

        app.others_search = "ground fire".to_string();
        app.apply_other_effect_level_to_filtered_rows(EffectLevel::Full);
        assert_eq!(
            app.effect_overrides.get("ground_effects/fire"),
            Some(&EffectLevel::Full)
        );
        assert!(!app.effect_overrides.contains_key("ground_effects/cold"));
        assert_eq!(
            app.effect_overrides.get("fireball"),
            Some(&EffectLevel::Full)
        );

        app.apply_other_effect_level_to_filtered_rows(EffectLevel::Reduced);
        assert!(!app.effect_overrides.contains_key("ground_effects/fire"));
        assert_eq!(
            app.effect_overrides.get("fireball"),
            Some(&EffectLevel::Full)
        );

        app.effects_search = "fireball".to_string();
        app.apply_effect_level_to_filtered_rows(EffectLevel::Reduced);
        assert!(!app.effect_overrides.contains_key("fireball"));
        assert!(!app.effect_overrides.contains_key("ground_effects/fire"));
    }

    #[test]
    fn other_bulk_reduced_under_legacy_parent_preserves_off_filter_siblings() {
        let mut app = GuiApp {
            other_catalog: Some(other_effect_catalog_rows(other_catalog_entries())),
            ..GuiApp::default()
        };
        app.effect_overrides
            .insert("ground_effects".to_string(), EffectLevel::Full);

        app.others_search = "ground fire".to_string();
        app.apply_other_effect_level_to_filtered_rows(EffectLevel::Reduced);

        assert!(!app.effect_overrides.contains_key("ground_effects"));
        assert!(!app.effect_overrides.contains_key("ground_effects/fire"));
        assert_eq!(
            app.effect_overrides.get("ground_effects/cold"),
            Some(&EffectLevel::Full)
        );
    }

    #[test]
    fn others_filter_matches_display_and_folder_text() {
        let mut app = GuiApp {
            other_catalog: Some(other_effect_catalog_rows(other_catalog_entries())),
            ..GuiApp::default()
        };

        app.others_search = "ground fire".to_string();
        app.refresh_others_filter();
        assert_eq!(app.others_filter_rows, vec![0]);

        app.others_search = "ambient_sparkles".to_string();
        app.refresh_others_filter();
        assert_eq!(app.others_filter_rows, vec![2]);

        app.others_search.clear();
        app.refresh_others_filter();
        assert_eq!(app.others_filter_rows, vec![0, 1, 2]);
    }

    #[test]
    fn skill_count_excludes_others_folders_once_others_catalog_loads() {
        let mut app = GuiApp::default();
        app.effect_overrides
            .insert("fireball".to_string(), EffectLevel::Full);
        app.effect_overrides
            .insert("ground_effects/fire".to_string(), EffectLevel::Full);
        app.effect_overrides
            .insert("ground_effects".to_string(), EffectLevel::Full);

        app.effect_catalog = Some(vec![EffectFolderRow {
            folders: vec!["fireball".to_string()],
            active_skill_id: "fireball".to_string(),
            action_type: "GreaterFireball".to_string(),
            display: "Fireball".to_string(),
            display_lower: "fireball".to_string(),
            search_lower: "fireball greaterfireball".to_string(),
        }]);
        assert_eq!(app.kept_original_effect_skill_count(), 3);
        assert_eq!(app.kept_original_other_effect_count(), 0);

        // Once loaded, Others folders and their broad parent move counts.
        app.other_catalog = Some(other_effect_catalog_rows(other_catalog_entries()));
        assert_eq!(app.kept_original_effect_skill_count(), 1);
        assert_eq!(app.kept_original_other_effect_count(), 2);

        app.set_other_effect_level("ground_effects/fire", EffectLevel::Reduced);
        assert_eq!(app.kept_original_other_effect_count(), 1);
        assert_eq!(app.kept_original_effect_skill_count(), 1);
    }

    fn supports_skill_row() -> EffectFolderRow {
        EffectFolderRow {
            folders: vec!["supports/runicsupports/bitterdead".to_string()],
            active_skill_id: "bitterdead".to_string(),
            action_type: "RunicSupport".to_string(),
            display: "Bitterdead".to_string(),
            display_lower: "bitterdead".to_string(),
            search_lower: "bitterdead supports/runicsupports/bitterdead".to_string(),
        }
    }

    fn supports_other_entries() -> Vec<OtherEffectCatalogEntry> {
        vec![
            OtherEffectCatalogEntry {
                folder: "supports/runicsupports/unknown".to_string(),
                display: "Unknown".to_string(),
            },
            OtherEffectCatalogEntry {
                folder: "supports/sibling".to_string(),
                display: "Sibling".to_string(),
            },
        ]
    }

    fn supports_app() -> GuiApp {
        GuiApp {
            effect_catalog: Some(vec![supports_skill_row()]),
            other_catalog: Some(other_effect_catalog_rows(supports_other_entries())),
            ..GuiApp::default()
        }
    }

    #[test]
    fn supports_parent_reducing_others_preserves_skill_sibling() {
        let mut app = supports_app();
        app.effect_overrides
            .insert("supports".to_string(), EffectLevel::Full);

        assert_eq!(
            app.other_effect_level("supports/runicsupports/unknown"),
            EffectLevel::Full
        );
        assert_eq!(
            app.effect_level_for_folders(&["supports/runicsupports/bitterdead".to_string()]),
            Some(EffectLevel::Full)
        );
        assert_eq!(app.kept_original_effect_skill_count(), 1);
        assert_eq!(app.kept_original_other_effect_count(), 2);

        assert!(app.set_other_effect_level("supports/runicsupports/unknown", EffectLevel::Reduced));

        assert!(!app.effect_overrides.contains_key("supports"));
        assert!(!app
            .effect_overrides
            .contains_key("supports/runicsupports/unknown"));
        assert_eq!(
            app.effect_overrides
                .get("supports/runicsupports/bitterdead"),
            Some(&EffectLevel::Full)
        );
        assert_eq!(
            app.effect_overrides.get("supports/sibling"),
            Some(&EffectLevel::Full)
        );
        assert_eq!(
            app.other_effect_level("supports/runicsupports/unknown"),
            EffectLevel::Reduced
        );
        assert_eq!(
            app.other_effect_level("supports/sibling"),
            EffectLevel::Full
        );
        assert_eq!(
            app.effect_level_for_folders(&["supports/runicsupports/bitterdead".to_string()]),
            Some(EffectLevel::Full)
        );
        assert_eq!(app.kept_original_effect_skill_count(), 1);
        assert_eq!(app.kept_original_other_effect_count(), 1);
    }

    #[test]
    fn supports_parent_reducing_skill_preserves_others_siblings() {
        let mut app = supports_app();
        app.effect_overrides
            .insert("supports".to_string(), EffectLevel::Full);

        assert!(app.set_skill_folders_level(
            &["supports/runicsupports/bitterdead".to_string()],
            EffectLevel::Reduced
        ));

        assert!(!app.effect_overrides.contains_key("supports"));
        assert!(!app
            .effect_overrides
            .contains_key("supports/runicsupports/bitterdead"));
        assert_eq!(
            app.effect_overrides.get("supports/runicsupports/unknown"),
            Some(&EffectLevel::Full)
        );
        assert_eq!(
            app.effect_overrides.get("supports/sibling"),
            Some(&EffectLevel::Full)
        );
        assert_eq!(
            app.effect_level_for_folders(&["supports/runicsupports/bitterdead".to_string()]),
            Some(EffectLevel::Reduced)
        );
        assert_eq!(
            app.other_effect_level("supports/runicsupports/unknown"),
            EffectLevel::Full
        );
        assert_eq!(app.kept_original_effect_skill_count(), 0);
        assert_eq!(app.kept_original_other_effect_count(), 2);
    }

    #[test]
    fn supports_migration_without_skill_catalog_leaves_everything_intact() {
        let mut app = GuiApp {
            other_catalog: Some(other_effect_catalog_rows(supports_other_entries())),
            ..GuiApp::default()
        };
        app.effect_overrides
            .insert("supports".to_string(), EffectLevel::Full);
        app.effect_overrides
            .insert("supports/sibling".to_string(), EffectLevel::Full);
        app.effect_overrides.insert(
            "supports/runicsupports/unknown".to_string(),
            EffectLevel::Full,
        );

        // The skill sibling is unknowable without its catalog: all intact.
        assert!(!app.set_other_effect_level("supports/runicsupports/unknown", EffectLevel::Reduced));
        assert_eq!(
            app.effect_overrides.get("supports"),
            Some(&EffectLevel::Full)
        );
        assert_eq!(
            app.effect_overrides.get("supports/sibling"),
            Some(&EffectLevel::Full)
        );
        assert_eq!(
            app.effect_overrides.get("supports/runicsupports/unknown"),
            Some(&EffectLevel::Full)
        );

        app.others_search = "unknown".to_string();
        assert!(!app.apply_other_effect_level_to_filtered_rows(EffectLevel::Reduced));
        assert_eq!(
            app.effect_overrides.get("supports"),
            Some(&EffectLevel::Full)
        );
        assert_eq!(
            app.effect_overrides.get("supports/runicsupports/unknown"),
            Some(&EffectLevel::Full)
        );

        // Exact removals without a covering parent still work while incomplete.
        let mut exact = GuiApp {
            other_catalog: Some(other_effect_catalog_rows(supports_other_entries())),
            ..GuiApp::default()
        };
        exact
            .effect_overrides
            .insert("supports/sibling".to_string(), EffectLevel::Full);
        assert!(exact.set_other_effect_level("supports/sibling", EffectLevel::Reduced));
        assert!(!exact.effect_overrides.contains_key("supports/sibling"));
    }

    #[test]
    fn ground_migration_does_not_need_skill_catalog() {
        let mut app = GuiApp {
            other_catalog: Some(other_effect_catalog_rows(other_catalog_entries())),
            ..GuiApp::default()
        };
        app.effect_overrides
            .insert("ground_effects".to_string(), EffectLevel::Full);

        assert!(app.set_other_effect_level("ground_effects/fire", EffectLevel::Reduced));
        assert!(!app.effect_overrides.contains_key("ground_effects"));
        assert_eq!(
            app.effect_overrides.get("ground_effects/cold"),
            Some(&EffectLevel::Full)
        );
    }

    #[test]
    fn skill_effect_level_and_counts_follow_inherited_parents() {
        let mut app = supports_app();
        app.effect_overrides
            .insert("supports".to_string(), EffectLevel::Full);

        assert_eq!(
            app.effect_level_for_folders(&["supports/runicsupports/bitterdead".to_string()]),
            Some(EffectLevel::Full)
        );
        assert_eq!(app.kept_original_effect_skill_count(), 1);

        app.effect_catalog = Some(vec![EffectFolderRow {
            folders: vec![
                "supports/runicsupports/bitterdead".to_string(),
                "fireball".to_string(),
            ],
            active_skill_id: "mixed".to_string(),
            action_type: "Mixed".to_string(),
            display: "Mixed".to_string(),
            display_lower: "mixed".to_string(),
            search_lower: "mixed".to_string(),
        }]);
        assert_eq!(
            app.effect_level_for_folders(&[
                "supports/runicsupports/bitterdead".to_string(),
                "fireball".to_string()
            ]),
            None
        );
        assert_eq!(app.kept_original_effect_skill_count(), 1);
    }

    #[test]
    fn active_tab_loading_invalidates_stale_skill_and_other_catalogs() {
        let mut app = GuiApp {
            effect_catalog: Some(vec![supports_skill_row()]),
            effect_catalog_dir: Some(PathBuf::from("/install/A")),
            other_catalog: Some(other_effect_catalog_rows(supports_other_entries())),
            other_catalog_dir: Some(PathBuf::from("/install/A")),
            game_dir_input: "/install/B".to_string(),
            effects_editor_tab: EffectsEditorTab::Others,
            ..GuiApp::default()
        };

        app.ensure_active_effects_tab_loading();

        // Stale catalogs must not be trusted across game directories.
        assert!(app.effect_catalog.is_none());
        assert!(app.other_catalog.is_none());
        assert!(app.effect_catalog_task.is_some());
        assert!(app.other_catalog_task.is_some());
    }

    #[test]
    fn effects_editor_accessible_for_effects_or_particles_only() {
        let mut app = GuiApp::default();
        app.selected_patches.clear();
        assert!(!app.effects_editor_accessible());

        app.selected_patches.insert(PatchId::Effects);
        assert!(app.effects_editor_accessible());

        app.selected_patches.clear();
        app.selected_patches.insert(PatchId::Particles);
        assert!(app.effects_editor_accessible());

        app.selected_patches.insert(PatchId::Effects);
        assert!(app.effects_editor_accessible());
    }

    #[test]
    fn prefs_round_trip_others_folders_through_existing_effect_skills_field() {
        let app = GuiApp::from_prefs(GuiPrefs {
            game_dir_input: String::new(),
            selected_patches: vec!["fog".to_string()],
            zoom: 2.4,
            color_mods: Vec::new(),
            effect_skills: vec![
                EffectSkillOverride {
                    folder: "fireball".to_string(),
                    level: EffectLevel::Full,
                },
                EffectSkillOverride {
                    folder: "ground_effects/fire".to_string(),
                    level: EffectLevel::Full,
                },
            ],
            monster_effects: Vec::new(),
        });

        assert_eq!(
            app.effect_overrides.get("ground_effects/fire"),
            Some(&EffectLevel::Full)
        );
        assert_eq!(
            app.prefs().effect_skills,
            vec![
                EffectSkillOverride {
                    folder: "fireball".to_string(),
                    level: EffectLevel::Full,
                },
                EffectSkillOverride {
                    folder: "ground_effects/fire".to_string(),
                    level: EffectLevel::Full,
                },
            ]
        );
        let request = app.patch_request().unwrap();
        assert!(request.params.effect_skills.contains(&EffectSkillOverride {
            folder: "ground_effects/fire".to_string(),
            level: EffectLevel::Full,
        }));
    }

    #[test]
    fn other_catalog_invalidates_only_when_game_dir_changes() {
        let mut app = GuiApp {
            other_catalog: Some(other_effect_catalog_rows(other_catalog_entries())),
            other_catalog_dir: Some(PathBuf::from("/install/A")),
            others_filter_key: Some(("q".to_string(), 3)),
            others_filter_rows: vec![0],
            ..GuiApp::default()
        };
        app.effect_overrides
            .insert("ground_effects/fire".to_string(), EffectLevel::Full);

        app.invalidate_other_effect_catalog_if_stale(&Some(PathBuf::from("/install/A")));
        assert!(app.other_catalog.is_some());
        assert!(app.others_filter_key.is_some());
        assert_eq!(app.others_filter_rows, vec![0]);

        // Different dir resets catalog + cache, keeping overrides.
        app.invalidate_other_effect_catalog_if_stale(&Some(PathBuf::from("/install/B")));
        assert!(app.other_catalog.is_none());
        assert!(app.other_catalog_task.is_none());
        assert!(app.other_catalog_error.is_none());
        assert!(app.others_filter_key.is_none());
        assert!(app.others_filter_rows.is_empty());
        assert_eq!(
            app.effect_overrides.get("ground_effects/fire"),
            Some(&EffectLevel::Full)
        );
    }

    fn catalog_row(stat_id: &str, text: &str) -> CatalogRow {
        let text = display_stat_text(text);
        CatalogRow {
            stat_id_lower: stat_id.to_lowercase(),
            text_lower: text.to_lowercase(),
            stat_id: stat_id.to_string(),
            text,
        }
    }

    #[test]
    fn color_filter_matches_query_words_in_any_order_against_display_text() {
        let mut app = GuiApp::from_prefs(GuiPrefs {
            game_dir_input: String::new(),
            selected_patches: Vec::new(),
            zoom: 2.4,
            color_mods: Vec::new(),
            effect_skills: Vec::new(),
            monster_effects: Vec::new(),
        });
        app.stat_catalog = Some(vec![
            catalog_row(
                "map_ritual_omen_chance_+%",
                "[ContainsRitual|Ritual] Favours in Map have {0}% increased chance to be [Omen|Omens]",
            ),
            catalog_row("map_monsters_life_+%", "{0}% more Monster Life"),
        ]);

        // Loose phrasing with markup-hidden words and different word forms.
        app.color_search = "increase chance to be omen".to_string();
        app.refresh_color_filter();
        assert_eq!(app.color_filter_rows.len(), 1);
        assert!(matches!(app.color_filter_rows[0], ColorRowRef::Catalog(0)));

        // Configured entries match on their display text too, not just id.
        app.color_mods = default_color_mods();
        app.stat_catalog.as_mut().unwrap().push(catalog_row(
            "map_monsters_damage_+%",
            "{0}% more Monster Damage",
        ));
        app.stat_catalog
            .as_mut()
            .unwrap()
            .sort_by(|a, b| a.stat_id.cmp(&b.stat_id));
        app.color_search = "more damage monster".to_string();
        app.color_filter_key = None;
        app.refresh_color_filter();
        assert!(app
            .color_filter_rows
            .iter()
            .any(|row| matches!(row, ColorRowRef::Config(idx)
                if app.color_mods[*idx].stat_id == "map_monsters_damage_+%")));

        // PoE2-style regex: alternation, quoted phrases, and `!` exclusion.
        app.color_search = "omen|monster".to_string();
        app.color_filter_key = None;
        app.refresh_color_filter();
        assert!(app.color_filter_rows.len() >= 3);
        app.color_search = "\"chance to be\" !monster".to_string();
        app.color_filter_key = None;
        app.refresh_color_filter();
        assert_eq!(app.color_filter_rows.len(), 1);
        assert!(matches!(app.color_filter_rows[0], ColorRowRef::Catalog(_)));

        // Empty query shows everything.
        app.color_search.clear();
        app.refresh_color_filter();
        let catalog_len = app.stat_catalog.as_ref().unwrap().len();
        let configured_in_catalog = app
            .stat_catalog
            .as_ref()
            .unwrap()
            .iter()
            .filter(|row| app.color_mods.iter().any(|e| e.stat_id == row.stat_id))
            .count();
        assert_eq!(
            app.color_filter_rows.len(),
            app.color_mods.len() + catalog_len - configured_in_catalog
        );
    }
}
