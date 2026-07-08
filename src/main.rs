#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod gui;

use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use tiny_poe2smoother::app::{
    apply_patches, load_effect_skill_catalog, load_stat_catalog, load_status, restore_backup,
    AppStatus, ApplyReport, PatchRequest, RestoreReport,
};
use tiny_poe2smoother::install::display_path;
use tiny_poe2smoother::patches::{
    all_patches, default_color_mods, display_stat_text, merge_with_defaults, parse_patch,
    ColorModEntry, EffectLevel, EffectSkillCatalogEntry, EffectSkillOverride, PatchId, PatchParams,
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
    effects_search: String,
    effect_catalog: Option<Vec<EffectFolderRow>>,
    effect_catalog_task: Option<Receiver<Result<Vec<EffectFolderRow>, String>>>,
    effect_catalog_error: Option<String>,
    effect_catalog_dir: Option<PathBuf>,
    effects_filter_key: Option<(String, usize)>,
    effects_filter_rows: Vec<usize>,
    status: Option<AppStatus>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GuiPrefs {
    game_dir_input: String,
    selected_patches: Vec<String>,
    zoom: f64,
    #[serde(default)]
    color_mods: Vec<ColorModEntry>,
    #[serde(default)]
    effect_skills: Vec<EffectSkillOverride>,
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
            effects_search: String::new(),
            effect_catalog: None,
            effect_catalog_task: None,
            effect_catalog_error: None,
            effect_catalog_dir: None,
            effects_filter_key: None,
            effects_filter_rows: Vec::new(),
            status: None,
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
        Self {
            game_dir_input: prefs.game_dir_input,
            selected_patches,
            zoom: prefs.zoom.clamp(1.2, 2.4),
            color_mods,
            effect_overrides,
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
        self.spawn("Detecting install...", move || {
            TaskResult::Status(load_status(game_dir).map_err(|err| err.to_string()))
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
            // PoE2-style search (see gui::search): space-separated terms are
            // ANDed regexes matched against the stat id or display text,
            // quotes keep phrases together, `!` excludes — with a literal
            // fallback so loose phrasings like "increase chance to be omen"
            // and pasted stat ids keep working.
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

    fn apply_effect_level_to_filtered_rows(&mut self, level: EffectLevel) {
        self.refresh_effects_filter();
        let folders: Vec<String> = self
            .effect_catalog
            .as_deref()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .filter(|(idx, _)| self.effects_filter_rows.contains(idx))
            .flat_map(|(_, row)| row.folders.iter().cloned())
            .collect();
        for folder in folders {
            if level == EffectLevel::Reduced {
                self.effect_overrides.remove(&folder);
            } else {
                self.effect_overrides.insert(folder, level);
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_restore_valid_patch_names_and_ignore_unknown_entries() {
        let app = GuiApp::from_prefs(GuiPrefs {
            game_dir_input: r"D:\SteamLibrary\steamapps\common\Path of Exile 2".to_string(),
            selected_patches: vec!["fog".to_string(), "unknown".to_string()],
            zoom: 9.0,
            color_mods: Vec::new(),
            effect_skills: Vec::new(),
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
                    folder: "Cold_Herald_Of_Ice".to_string(),
                    level: EffectLevel::Hidden,
                },
                EffectSkillOverride {
                    folder: "arc_02".to_string(),
                    level: EffectLevel::Reduced,
                },
                EffectSkillOverride {
                    folder: "fireball".to_string(),
                    level: EffectLevel::Full,
                },
            ],
        });

        assert_eq!(
            app.effect_overrides.get("cold_herald_of_ice"),
            Some(&EffectLevel::Hidden)
        );
        assert_eq!(
            app.effect_overrides.get("fireball"),
            Some(&EffectLevel::Full)
        );
        // Explicit Reduced entries are meaningless and dropped on load.
        assert!(!app.effect_overrides.contains_key("arc_02"));

        // Saved back folder-sorted, non-default only.
        assert_eq!(
            app.prefs().effect_skills,
            vec![
                EffectSkillOverride {
                    folder: "cold_herald_of_ice".to_string(),
                    level: EffectLevel::Hidden,
                },
                EffectSkillOverride {
                    folder: "fireball".to_string(),
                    level: EffectLevel::Full,
                },
            ]
        );
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

        app.effect_overrides
            .insert("fireball".to_string(), EffectLevel::Hidden);
        let request = app.patch_request().unwrap();
        assert_eq!(
            request.params.effect_skills,
            vec![EffectSkillOverride {
                folder: "fireball".to_string(),
                level: EffectLevel::Hidden,
            }]
        );
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
            .insert("fireball".to_string(), EffectLevel::Hidden);

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
            Some(&EffectLevel::Hidden)
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
        app.apply_effect_level_to_filtered_rows(EffectLevel::Hidden);
        assert_eq!(
            app.effect_overrides.get("fire_heraldofash"),
            Some(&EffectLevel::Hidden)
        );
        assert_eq!(
            app.effect_overrides.get("herald_of_fire"),
            Some(&EffectLevel::Hidden)
        );
        assert!(!app.effect_overrides.contains_key("fireball"));

        app.apply_effect_level_to_filtered_rows(EffectLevel::Reduced);
        assert!(app.effect_overrides.is_empty());
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
