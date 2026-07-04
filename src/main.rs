#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod gui;

use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use tiny_poe2smoother::app::{
    apply_patches, load_status, restore_backup, AppStatus, ApplyReport, PatchRequest, RestoreReport,
};
use tiny_poe2smoother::install::display_path;
use tiny_poe2smoother::patches::{all_patches, parse_patch, PatchId};

const PREFS_KEY: &str = "tiny-poe2smoother.gui.v1";

fn main() -> eframe::Result {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GuiPrefs {
    game_dir_input: String,
    selected_patches: Vec<String>,
    zoom: f64,
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
        Self {
            game_dir_input: prefs.game_dir_input,
            selected_patches,
            zoom: prefs.zoom.clamp(1.2, 2.4),
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
        Ok(PatchRequest {
            game_dir: self.game_dir(),
            patches,
            zoom: self.zoom,
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
        let Ok(result) = rx.try_recv() else {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
            return;
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
        });

        assert_eq!(
            app.game_dir_input,
            r"D:\SteamLibrary\steamapps\common\Path of Exile 2"
        );
        assert!(app.selected_patches.contains(&PatchId::Fog));
        assert_eq!(app.selected_patches.len(), 1);
        assert_eq!(app.zoom, 2.4);
    }
}
