#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod gui;

use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use tiny_poe2smoother::app::{
    apply_patches, load_stat_catalog, load_status, restore_backup, AppStatus, ApplyReport,
    PatchRequest, RestoreReport,
};
use tiny_poe2smoother::install::display_path;
use tiny_poe2smoother::patches::{
    all_patches, default_color_mods, display_stat_text, merge_with_defaults, parse_patch,
    ColorModEntry, PatchId, PatchParams,
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
    color_filter_key: Option<ColorFilterKey>,
    color_filter_rows: Vec<ColorRowRef>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GuiPrefs {
    game_dir_input: String,
    selected_patches: Vec<String>,
    zoom: f64,
    #[serde(default)]
    color_mods: Vec<ColorModEntry>,
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
            color_filter_key: None,
            color_filter_rows: Vec::new(),
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
        Self {
            game_dir_input: prefs.game_dir_input,
            selected_patches,
            zoom: prefs.zoom.clamp(1.2, 2.4),
            color_mods,
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
        }
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

    /// Kick off the background stat-catalog load for the color editor if it
    /// hasn't run yet. Deliberately NOT `spawn`: that would set `is_busy()`
    /// and lock the whole UI while the editor should stay usable.
    fn ensure_catalog_loading(&mut self) {
        if self.stat_catalog.is_some() || self.catalog_task.is_some() {
            return;
        }
        self.catalog_error = None;
        let game_dir = self.game_dir();
        let (tx, rx) = mpsc::channel();
        self.catalog_task = Some(rx);
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
