#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use tiny_poe2smoother::app::{
    apply_patches, load_status, restore_backup, AppStatus, ApplyReport, PatchRequest, PatchState,
    RestoreReport,
};
use tiny_poe2smoother::install::{display_path, is_game_running};
use tiny_poe2smoother::patches::{all_patches, parse_patch, PatchId};

const PREFS_KEY: &str = "tiny-poe2smoother.gui.v1";

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([980.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "tiny-poe2smoother",
        options,
        Box::new(|cc| {
            configure_theme(&cc.egui_ctx);
            Ok(Box::new(GuiApp::new(cc.storage)))
        }),
    )
}

fn configure_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();

    visuals.faint_bg_color = egui::Color32::from_rgb(11, 14, 20);
    visuals.extreme_bg_color = egui::Color32::from_rgb(8, 10, 15);
    visuals.window_fill = egui::Color32::from_rgb(15, 19, 28);
    visuals.panel_fill = egui::Color32::from_rgb(15, 23, 35);
    visuals.window_corner_radius = egui::CornerRadius::same(8);
    visuals.window_shadow = egui::epaint::Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: egui::Color32::from_rgba_premultiplied(0, 0, 0, 140),
    };

    visuals.hyperlink_color = egui::Color32::from_rgb(20, 184, 166);
    visuals.selection = egui::style::Selection {
        bg_fill: egui::Color32::from_rgba_premultiplied(20, 184, 166, 100),
        stroke: egui::Stroke::new(1.0, egui::Color32::from_rgb(20, 184, 166)),
    };

    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(45, 58, 78);
    visuals.widgets.inactive.fg_stroke =
        egui::Stroke::new(1.5, egui::Color32::from_rgb(20, 184, 166));
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(4);

    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(50, 65, 88);
    visuals.widgets.hovered.fg_stroke =
        egui::Stroke::new(2.0, egui::Color32::from_rgb(35, 230, 195));
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(4);

    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(20, 180, 145);
    visuals.widgets.active.fg_stroke =
        egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 255, 255));
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(4);

    visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(30, 40, 58);
    visuals.widgets.noninteractive.fg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(180, 190, 200));
    visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(4);
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(
        0.5,
        egui::Color32::from_rgba_premultiplied(20, 184, 166, 25),
    );

    visuals.override_text_color = Some(egui::Color32::from_rgb(230, 238, 250));
    visuals.code_bg_color = egui::Color32::from_rgb(20, 26, 38);

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(16.0, 8.0);
    ctx.set_style(style);
}

fn card_frame() -> egui::Frame {
    egui::Frame {
        fill: egui::Color32::from_rgb(20, 30, 45),
        stroke: egui::Stroke::new(1.0, egui::Color32::from_rgb(38, 55, 78)),
        corner_radius: egui::CornerRadius::same(10),
        inner_margin: egui::Margin::symmetric(16, 14),
        ..Default::default()
    }
}

struct GuiApp {
    game_dir_input: String,
    selected_patches: HashSet<PatchId>,
    zoom: f64,
    status: Option<AppStatus>,
    message: String,
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
    Restore(Result<RestoreReport, String>),
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

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.initialized {
            self.initialized = true;
            self.spawn_status();
        }

        self.poll_task(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_enabled_ui(!self.is_busy(), |ui| {
                        self.draw_header(ui);
                        ui.add_space(12.0);
                        self.draw_game_directory_card(ui);
                        ui.add_space(12.0);
                        self.draw_patches_card(ui);
                    });
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&self.message)
                                .size(11.0)
                                .color(egui::Color32::from_rgb(145, 160, 180)),
                        );
                    });
                    ui.add_space(8.0);
                    self.draw_actions(ui);
                });
        });

        self.confirm_windows(ctx);
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
            status: None,
            message: "Ready.".to_string(),
            task: None,
            busy_label: None,
            confirm_apply: false,
            confirm_restore: false,
            show_game_running_dialog: false,
            initialized: false,
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

    fn draw_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new("tiny-poe2smoother")
                        .size(20.0)
                        .strong()
                        .color(egui::Color32::from_rgb(230, 238, 250)),
                );
                ui.label(
                    egui::RichText::new("Path of Exile 2 visual patch manager")
                        .size(12.0)
                        .color(egui::Color32::from_rgb(145, 160, 180)),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(label) = &self.busy_label {
                    ui.spinner();
                    ui.label(label);
                }

                let (badge_text, badge_color) = if self.status.is_some() {
                    ("Install detected", egui::Color32::from_rgb(20, 184, 166))
                } else {
                    ("Not detected", egui::Color32::from_rgb(180, 140, 140))
                };
                let badge = egui::Frame {
                    fill: egui::Color32::from_rgba_premultiplied(
                        badge_color.r(),
                        badge_color.g(),
                        badge_color.b(),
                        20,
                    ),
                    stroke: egui::Stroke::new(0.5, badge_color),
                    corner_radius: egui::CornerRadius::same(4),
                    inner_margin: egui::Margin::symmetric(7, 2),
                    ..Default::default()
                };
                badge.show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(badge_text)
                            .size(11.0)
                            .color(badge_color),
                    );
                });
            });
        });
    }

    fn draw_game_directory_card(&mut self, ui: &mut egui::Ui) {
        card_frame().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(
                egui::RichText::new("Game directory")
                    .size(14.0)
                    .strong()
                    .color(egui::Color32::from_rgb(230, 238, 250)),
            );
            ui.add_space(6.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.game_dir_input)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal(|ui| {
                if ui.button("Browse").clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        self.game_dir_input = display_path(&path);
                        self.status = None;
                    }
                }
                if ui.button("Detect").clicked() {
                    self.spawn_status();
                }
            });
            ui.add_space(6.0);
            if let Some(status) = &self.status {
                let muted = egui::Color32::from_rgb(145, 160, 180);
                ui.label(
                    egui::RichText::new(format!("Index: {}", display_path(&status.index_path)))
                        .size(12.0)
                        .color(muted),
                );
                ui.label(
                    egui::RichText::new(format!("Indexed paths: {}", status.indexed_paths))
                        .size(12.0)
                        .color(muted),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "Patch state: {}",
                        patch_state_label(status.patch_state)
                    ))
                    .size(12.0)
                    .color(muted),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "Backup: {} ({})",
                        display_path(&status.backup_path),
                        backup_label(status.patch_state)
                    ))
                    .size(12.0)
                    .color(muted),
                );
            }
        });
    }

    fn draw_patches_card(&mut self, ui: &mut egui::Ui) {
        card_frame().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(
                egui::RichText::new("Patches")
                    .size(14.0)
                    .strong()
                    .color(egui::Color32::from_rgb(230, 238, 250)),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Select all").clicked() {
                    self.selected_patches = all_patches().iter().map(|patch| patch.id).collect();
                }
                if ui.button("Select none").clicked() {
                    self.selected_patches.clear();
                }
            });
            ui.add_space(6.0);
            egui::Grid::new("patch_grid")
                .num_columns(3)
                .spacing([8.0, 8.0])
                .show(ui, |ui| {
                    for patch in all_patches() {
                        let is_camera = patch.id == PatchId::Camera;
                        let mut selected = self.selected_patches.contains(&patch.id);

                        // Col 0: checkbox
                        let r = ui.checkbox(&mut selected, "");
                        if r.changed() {
                            if selected {
                                self.selected_patches.insert(patch.id);
                            } else {
                                self.selected_patches.remove(&patch.id);
                            }
                        }

                        // Col 1: name
                        ui.set_min_width(95.0);
                        ui.label(
                            egui::RichText::new(patch.name)
                                .size(13.0)
                                .color(egui::Color32::from_rgb(230, 238, 250)),
                        );

                        // Col 2: description (stretches); camera row also has zoom controls
                        if is_camera {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(patch.description)
                                        .color(egui::Color32::from_rgb(145, 160, 180)),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(format!("{:.3}", self.zoom))
                                                .size(13.0)
                                                .color(egui::Color32::from_rgb(20, 184, 166))
                                                .monospace()
                                                .strong(),
                                        );
                                        let camera_enabled =
                                            self.selected_patches.contains(&PatchId::Camera);
                                        ui.add_enabled_ui(camera_enabled, |inner_ui| {
                                            let sw = (inner_ui.available_width() - 50.0)
                                                .clamp(100.0, 200.0);
                                            inner_ui.add_sized(
                                                [sw, 22.0],
                                                egui::Slider::new(&mut self.zoom, 1.2..=2.4)
                                                    .step_by(0.1)
                                                    .show_value(false),
                                            );
                                        });
                                    },
                                );
                            });
                        } else {
                            ui.label(
                                egui::RichText::new(patch.description)
                                    .color(egui::Color32::from_rgb(145, 160, 180)),
                            );
                        }

                        ui.end_row();
                    }
                });
        });
    }

    fn draw_actions(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space((ui.available_width() - 220.0).max(0.0));
            let already_patched = self
                .status
                .as_ref()
                .map(|status| status.patch_state.is_currently_patched())
                .unwrap_or(false);
            let can_restore = self
                .status
                .as_ref()
                .map(|status| status.patch_state.can_restore())
                .unwrap_or(false);
            let apply = egui::Button::new(
                egui::RichText::new("Apply")
                    .color(egui::Color32::from_rgb(255, 255, 255))
                    .strong(),
            )
            .fill(egui::Color32::from_rgb(20, 184, 166))
            .corner_radius(6)
            .min_size(egui::vec2(100.0, 36.0));
            if ui.add_enabled(!already_patched, apply).clicked() {
                match self.patch_request() {
                    Ok(_) => self.confirm_apply = true,
                    Err(err) => self.message = err,
                }
            }
            let restore = egui::Button::new(
                egui::RichText::new("Restore").color(egui::Color32::from_rgb(230, 238, 250)),
            )
            .fill(egui::Color32::from_rgb(40, 50, 70))
            .corner_radius(6)
            .min_size(egui::vec2(100.0, 36.0));
            if ui.add_enabled(can_restore, restore).clicked() {
                self.confirm_restore = true;
            }
        });
    }

    fn confirm_windows(&mut self, ctx: &egui::Context) {
        if self.confirm_apply {
            egui::Window::new("Confirm apply")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(
                        "This will modify Path of Exile 2 bundle files after creating a backup.",
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            self.confirm_apply = false;
                            if is_game_running() {
                                self.show_game_running_dialog = true;
                            } else {
                                match self.patch_request() {
                                    Ok(request) => self.spawn_apply(request),
                                    Err(err) => self.message = err,
                                }
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_apply = false;
                        }
                    });
                });
        }

        if self.confirm_restore {
            egui::Window::new("Confirm restore")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("This will restore files from the current tiny-poe2smoother backup.");
                    ui.horizontal(|ui| {
                        if ui.button("Restore").clicked() {
                            self.confirm_restore = false;
                            if is_game_running() {
                                self.show_game_running_dialog = true;
                            } else {
                                self.spawn_restore();
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_restore = false;
                        }
                    });
                });
        }

        if self.show_game_running_dialog {
            egui::Window::new("Path of Exile 2 is running")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Close Path of Exile 2 before applying patches or restoring.");
                    ui.label("The game must be fully closed — not minimized to tray.");
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() {
                            self.show_game_running_dialog = false;
                        }
                    });
                });
        }
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
            TaskResult::Restore(restore_backup(game_dir).map_err(|err| err.to_string()))
        });
    }

    fn spawn(&mut self, label: &str, work: impl FnOnce() -> TaskResult + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        self.task = Some(rx);
        self.busy_label = Some(label.to_string());
        self.message = label.to_string();
        thread::spawn(move || {
            let _ = tx.send(work());
        });
    }

    fn poll_task(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.task else {
            return;
        };
        let Ok(result) = rx.try_recv() else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };

        self.task = None;
        self.busy_label = None;

        match result {
            TaskResult::Status(result) => match result {
                Ok(status) => {
                    self.game_dir_input = display_path(&status.game_dir);
                    self.message = "Install detected.".to_string();
                    self.status = Some(status);
                }
                Err(err) => self.message = err,
            },
            TaskResult::Apply(result) => match result {
                Ok(report) => {
                    self.message = format!(
                        "Applied {} file(s). Touched {} bundle/index file(s). Backup: {}",
                        report.changed_files,
                        report.touched_paths.len(),
                        display_path(&report.backup_path)
                    );
                    self.spawn_status();
                }
                Err(err) => self.message = err,
            },
            TaskResult::Restore(result) => match result {
                Ok(report) => {
                    self.message = format!("Restored {} file(s).", report.restored_files);
                    self.spawn_status();
                }
                Err(err) => self.message = err,
            },
        }
    }
}

fn patch_state_label(state: PatchState) -> &'static str {
    match state {
        PatchState::Clean => "not patched",
        PatchState::Patched => "patched",
        PatchState::StaleBackup => "not patched, obsolete backup found",
        PatchState::PatchedMissingBackup => "patched, backup missing",
    }
}

fn backup_label(state: PatchState) -> &'static str {
    match state {
        PatchState::Clean => "none",
        PatchState::Patched => "present",
        PatchState::StaleBackup => "obsolete",
        PatchState::PatchedMissingBackup => "missing",
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
