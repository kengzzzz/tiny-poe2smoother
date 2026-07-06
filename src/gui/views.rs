use eframe::egui;
use tiny_poe2smoother::app::PatchState;
use tiny_poe2smoother::install::{display_path, is_game_running};
use tiny_poe2smoother::patches::{
    all_patches, all_presets, default_color_mods, ColorModEntry, PatchId, PatchInfo, PRESET_COLORS,
};

use super::icon;
use super::theme::{self, palette};
use super::widgets;
use crate::{ColorRowRef, GuiApp, MessageKind};

const GROUPS: &[(&str, &[PatchId])] = &[
    (
        "Camera & Map",
        &[PatchId::Camera, PatchId::Minimap, PatchId::AtlasFog],
    ),
    (
        "Environment",
        &[
            PatchId::Fog,
            PatchId::Rain,
            PatchId::Clouds,
            PatchId::EnvParticles,
            PatchId::Shadow,
            PatchId::Light,
        ],
    ),
    (
        "Effects",
        &[
            PatchId::Delirium,
            PatchId::Particles,
            PatchId::Effects,
            PatchId::MtxSoft,
        ],
    ),
    (
        "Audio",
        &[
            PatchId::DisableSounds,
            PatchId::SkillSounds,
            PatchId::MonsterSounds,
        ],
    ),
    ("Interface", &[PatchId::ColorMods, PatchId::MonsterHpBar]),
];

// Column packing chosen to roughly balance row counts (3+4+2 vs 6+3).
const COLUMNS: [&[usize]; 2] = [&[0, 2, 4], &[1, 3]];

pub fn draw(app: &mut GuiApp, ui: &mut egui::Ui) {
    draw_header(app, ui);
    draw_action_bar(app, ui);

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(palette::BG_APP)
                .inner_margin(egui::Margin::same(16)),
        )
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_enabled_ui(!app.is_busy(), |ui| {
                        draw_directory_card(app, ui);
                        ui.add_space(12.0);
                        draw_preset_toolbar(app, ui);
                        ui.add_space(12.0);
                        draw_patch_groups(app, ui);
                    });
                });
        });

    let ctx = ui.ctx().clone();
    draw_modals(app, &ctx);
}

fn draw_header(app: &GuiApp, ui: &mut egui::Ui) {
    egui::Panel::top("header")
        .frame(
            egui::Frame::new()
                .fill(palette::BG_APP)
                .inner_margin(egui::Margin::symmetric(16, 12)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
                icon::paint_logo_mark(ui.painter(), rect);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(theme::title_text("tiny-poe2smoother"));
                    ui.label(theme::caption_text(concat!(
                        "Path of Exile 2 visual patch manager · v",
                        env!("CARGO_PKG_VERSION")
                    )));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(status) = &app.status {
                        let (text, color) = patch_state_pill(status.patch_state);
                        widgets::status_pill(ui, text, color);
                        widgets::status_pill(ui, "Install detected", palette::SUCCESS);
                    } else {
                        widgets::status_pill(ui, "No install detected", palette::WARNING);
                    }
                });
            });
        });
}

fn draw_directory_card(app: &mut GuiApp, ui: &mut egui::Ui) {
    widgets::card().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.label(theme::heading_text("Game directory"));
        ui.add_space(6.0);
        ui.add(
            egui::TextEdit::singleline(&mut app.game_dir_input)
                .font(egui::TextStyle::Monospace)
                .hint_text("Path of Exile 2 install directory")
                .desired_width(f32::INFINITY)
                .margin(egui::Margin::symmetric(10, 8)),
        );
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if ui.button("Browse…").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    app.game_dir_input = display_path(&path);
                    app.status = None;
                }
            }
            if ui.button("Detect").clicked() {
                app.spawn_status();
            }
        });
        if let Some(status) = &app.status {
            ui.add_space(8.0);
            egui::Grid::new("status_grid")
                .num_columns(2)
                .spacing([16.0, 3.0])
                .show(ui, |ui| {
                    let label = |ui: &mut egui::Ui, text: &str| {
                        ui.label(
                            egui::RichText::new(text)
                                .size(11.5)
                                .color(palette::TEXT_FAINT),
                        );
                    };
                    label(ui, "Layout");
                    ui.label(theme::caption_text(status.install_layout.label()));
                    ui.end_row();
                    label(ui, "Index");
                    ui.label(
                        egui::RichText::new(&status.index_display_path)
                            .size(11.5)
                            .monospace()
                            .color(palette::TEXT_MUTED),
                    );
                    ui.end_row();
                    label(ui, "Indexed paths");
                    ui.label(theme::caption_text(&status.indexed_paths.to_string()));
                    ui.end_row();
                    label(ui, "State");
                    let (state_text, state_color) = patch_state_pill(status.patch_state);
                    ui.label(
                        egui::RichText::new(state_text)
                            .size(11.5)
                            .color(state_color),
                    );
                    ui.end_row();
                    label(ui, "Backup");
                    ui.label(theme::caption_text(&format!(
                        "{} ({})",
                        display_path(&status.backup_path),
                        backup_label(status.patch_state)
                    )));
                    ui.end_row();
                });
        }
    });
}

fn draw_preset_toolbar(app: &mut GuiApp, ui: &mut egui::Ui) {
    widgets::card().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Presets")
                    .size(11.5)
                    .family(theme::family_medium())
                    .color(palette::TEXT_FAINT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if small_action(ui, "Select none") {
                    app.selected_patches.clear();
                }
                if small_action(ui, "Select all") {
                    app.selected_patches = all_patches().iter().map(|patch| patch.id).collect();
                }
            });
        });
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            for preset in all_presets() {
                let active = preset
                    .patches
                    .iter()
                    .all(|patch| app.selected_patches.contains(patch));
                let chip = widgets::chip(&display_name(preset.name), active);
                if ui.add(chip).on_hover_text(preset.description).clicked() {
                    for patch in preset.patches {
                        app.selected_patches.insert(*patch);
                    }
                }
            }
        });
    });
}

fn draw_patch_groups(app: &mut GuiApp, ui: &mut egui::Ui) {
    ui.columns(2, |columns| {
        for (column, group_indices) in columns.iter_mut().zip(COLUMNS) {
            for (i, &group_index) in group_indices.iter().enumerate() {
                if i > 0 {
                    column.add_space(12.0);
                }
                let (title, patch_ids) = GROUPS[group_index];
                draw_patch_group(app, column, title, patch_ids);
            }
        }
    });
}

fn draw_patch_group(app: &mut GuiApp, ui: &mut egui::Ui, title: &str, patch_ids: &[PatchId]) {
    widgets::card().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        let selected_count = patch_ids
            .iter()
            .filter(|id| app.selected_patches.contains(id))
            .count();
        ui.horizontal(|ui| {
            ui.label(theme::heading_text(title));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("{selected_count}/{}", patch_ids.len()))
                        .size(11.5)
                        .color(palette::TEXT_FAINT),
                );
            });
        });
        ui.add_space(4.0);
        for &patch_id in patch_ids {
            let patch = patch_info(patch_id);
            let mut selected = app.selected_patches.contains(&patch_id);
            widgets::patch_row(
                ui,
                &mut selected,
                &display_name(patch.name),
                patch.description,
            );
            if selected {
                app.selected_patches.insert(patch_id);
            } else {
                app.selected_patches.remove(&patch_id);
            }
            if patch_id == PatchId::Camera {
                draw_zoom_row(app, ui);
            }
            if patch_id == PatchId::ColorMods {
                draw_color_mods_row(app, ui);
            }
        }
    });
}

fn draw_color_mods_row(app: &mut GuiApp, ui: &mut egui::Ui) {
    if !app.selected_patches.contains(&PatchId::ColorMods) {
        return;
    }
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        if small_action(ui, "Edit colors…") {
            app.show_color_editor = true;
            app.ensure_catalog_loading();
        }
        let enabled = app.color_mods.iter().filter(|entry| entry.enabled).count();
        ui.label(theme::caption_text(&format!("{enabled} mod(s) enabled")));
    });
}

fn draw_zoom_row(app: &mut GuiApp, ui: &mut egui::Ui) {
    let camera_selected = app.selected_patches.contains(&PatchId::Camera);
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        ui.add_enabled_ui(camera_selected, |ui| {
            ui.label(
                egui::RichText::new("Zoom")
                    .size(11.5)
                    .color(palette::TEXT_FAINT),
            );
            ui.add(
                egui::Slider::new(&mut app.zoom, 1.2..=2.4)
                    .step_by(0.1)
                    .show_value(false),
            );
            ui.label(
                egui::RichText::new(format!("{:.1}×", app.zoom))
                    .size(12.5)
                    .monospace()
                    .color(if camera_selected {
                        palette::ACCENT
                    } else {
                        palette::TEXT_FAINT
                    }),
            );
        });
    });
}

fn draw_action_bar(app: &mut GuiApp, ui: &mut egui::Ui) {
    egui::Panel::bottom("actions")
        .frame(
            egui::Frame::new()
                .fill(palette::BG_CARD)
                .inner_margin(egui::Margin::symmetric(16, 10)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let busy = app.is_busy();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let already_patched = app
                        .status
                        .as_ref()
                        .map(|status| status.patch_state.is_currently_patched())
                        .unwrap_or(false);
                    let can_restore = app
                        .status
                        .as_ref()
                        .map(|status| status.patch_state.can_restore())
                        .unwrap_or(false);

                    let apply = widgets::primary_button("Apply");
                    if ui.add_enabled(!already_patched && !busy, apply).clicked() {
                        match app.patch_request() {
                            Ok(_) => app.confirm_apply = true,
                            Err(err) => app.set_message(err, MessageKind::Error),
                        }
                    }
                    let restore = widgets::secondary_button("Restore");
                    if ui.add_enabled(can_restore && !busy, restore).clicked() {
                        app.confirm_restore = true;
                    }

                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        if busy {
                            ui.add(egui::Spinner::new().color(palette::ACCENT));
                            if let Some(label) = &app.busy_label {
                                ui.label(theme::caption_text(label));
                            }
                            let t = ((ui.input(|i| i.time) * 0.9) % 1.0) as f32;
                            ui.add(
                                egui::ProgressBar::new(t)
                                    .desired_width(140.0)
                                    .desired_height(4.0)
                                    .fill(palette::ACCENT)
                                    .corner_radius(2),
                            );
                        }
                    });
                });
            });
        });
}

fn draw_modals(app: &mut GuiApp, ctx: &egui::Context) {
    if app.confirm_apply {
        let modal = confirm_modal(ctx, "confirm_apply", |ui| {
            ui.label(theme::heading_text("Apply patches?"));
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "This will modify Path of Exile 2 bundle files after creating a backup.",
                )
                .color(palette::TEXT_MUTED),
            );
            ui.add_space(2.0);
            ui.label(theme::caption_text(&format!(
                "{} patch(es) selected",
                app.selected_patches.len()
            )));
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(widgets::primary_button("Apply")).clicked() {
                    app.confirm_apply = false;
                    if is_game_running() {
                        app.show_game_running_dialog = true;
                    } else {
                        match app.patch_request() {
                            Ok(request) => app.spawn_apply(request),
                            Err(err) => app.set_message(err, MessageKind::Error),
                        }
                    }
                }
                if ui.add(widgets::secondary_button("Cancel")).clicked() {
                    app.confirm_apply = false;
                }
            });
        });
        if modal {
            app.confirm_apply = false;
        }
    }

    if app.confirm_restore {
        let modal = confirm_modal(ctx, "confirm_restore", |ui| {
            ui.label(theme::heading_text("Restore backup?"));
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "This will restore files from the current tiny-poe2smoother backup.",
                )
                .color(palette::TEXT_MUTED),
            );
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(widgets::danger_button("Restore")).clicked() {
                    app.confirm_restore = false;
                    if is_game_running() {
                        app.show_game_running_dialog = true;
                    } else {
                        app.spawn_restore();
                    }
                }
                if ui.add(widgets::secondary_button("Cancel")).clicked() {
                    app.confirm_restore = false;
                }
            });
        });
        if modal {
            app.confirm_restore = false;
        }
    }

    if app.show_color_editor {
        draw_color_editor(app, ctx);
    }

    if app.show_game_running_dialog {
        let modal = confirm_modal(ctx, "game_running", |ui| {
            ui.label(
                egui::RichText::new("Path of Exile 2 is running")
                    .size(15.0)
                    .family(theme::family_semibold())
                    .color(palette::WARNING),
            );
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Close Path of Exile 2 before applying patches or restoring.")
                    .color(palette::TEXT_MUTED),
            );
            ui.label(
                egui::RichText::new("The game must be fully closed — not minimized to tray.")
                    .color(palette::TEXT_MUTED),
            );
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(widgets::primary_button("OK")).clicked() {
                    app.show_game_running_dialog = false;
                }
            });
        });
        if modal {
            app.show_game_running_dialog = false;
        }
    }
}

const COLOR_ROW_HEIGHT: f32 = 26.0;

/// The color-mods editor: search box over the configured entries plus the
/// stat catalog parsed from the game files, one row per mod with an enabled
/// checkbox, preset swatches, and a custom RGB picker. Wider than
/// `confirm_modal`, hence its own modal.
fn draw_color_editor(app: &mut GuiApp, ctx: &egui::Context) {
    let modal = egui::Modal::new(egui::Id::new("color_mods_editor"))
        .frame(widgets::card().inner_margin(20))
        .backdrop_color(egui::Color32::from_black_alpha(140))
        .show(ctx, |ui| {
            ui.set_width(640.0);
            ui.label(theme::heading_text("Color mods"));
            ui.add_space(2.0);
            ui.label(theme::caption_text(
                "Colorize modifier text wherever it appears in game — waystones, items, tablets.",
            ));
            ui.add_space(8.0);
            ui.add(
                egui::TextEdit::singleline(&mut app.color_search)
                    .hint_text("Search stat id or text…")
                    .desired_width(f32::INFINITY)
                    .margin(egui::Margin::symmetric(10, 8)),
            );
            ui.add_space(6.0);
            if app.catalog_task.is_some() {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().color(palette::ACCENT));
                    ui.label(theme::caption_text("Loading mod catalog from game files…"));
                });
                ui.add_space(4.0);
            } else if let Some(error) = app.catalog_error.clone() {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("Mod catalog unavailable: {error}"))
                            .size(11.5)
                            .color(palette::WARNING),
                    );
                    if small_action(ui, "Retry") {
                        app.catalog_error = None;
                        app.ensure_catalog_loading();
                    }
                });
                ui.add_space(4.0);
            }

            app.refresh_color_filter();
            let row_count = app.color_filter_rows.len();
            egui::ScrollArea::vertical()
                .max_height(380.0)
                .auto_shrink([false, true])
                .show_rows(ui, COLOR_ROW_HEIGHT, row_count, |ui, range| {
                    for i in range {
                        let row = app.color_filter_rows[i];
                        draw_color_mod_list_row(app, ui, row);
                    }
                });

            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(widgets::primary_button("Close")).clicked() {
                    app.show_color_editor = false;
                }
                if ui
                    .add(widgets::secondary_button("Reset to defaults"))
                    .clicked()
                {
                    app.color_mods = default_color_mods();
                    app.color_filter_key = None;
                }
            });
        })
        .should_close();
    if modal {
        app.show_color_editor = false;
    }
}

fn draw_color_mod_list_row(app: &mut GuiApp, ui: &mut egui::Ui, row: ColorRowRef) {
    match row {
        ColorRowRef::Config(idx) => {
            let text = app
                .catalog_text(&app.color_mods[idx].stat_id)
                .map(str::to_string);
            color_row_ui(ui, &mut app.color_mods[idx], text.as_deref());
        }
        ColorRowRef::Catalog(idx) => {
            // Not configured yet: render an ephemeral disabled entry; any
            // interaction (checkbox, swatch, picker) promotes it into the
            // config as enabled. That is the whole "add custom mod" flow.
            let Some(catalog) = &app.stat_catalog else {
                return;
            };
            let catalog_row = &catalog[idx];
            let mut entry = ColorModEntry {
                stat_id: catalog_row.stat_id.clone(),
                color: PRESET_COLORS[0].1,
                enabled: false,
            };
            let text = (!catalog_row.text.is_empty()).then(|| catalog_row.text.clone());
            if color_row_ui(ui, &mut entry, text.as_deref()) {
                entry.enabled = true;
                app.color_mods.push(entry);
            }
        }
    }
}

/// One editor row; returns true when the user changed anything.
fn color_row_ui(ui: &mut egui::Ui, entry: &mut ColorModEntry, text: Option<&str>) -> bool {
    let mut changed = false;
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), COLOR_ROW_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            changed |= ui.checkbox(&mut entry.enabled, "").changed();
            for (name, color) in PRESET_COLORS {
                let selected = entry.color == color;
                let swatch = widgets::color_swatch(
                    ui,
                    egui::Color32::from_rgb(color[0], color[1], color[2]),
                    selected,
                )
                .on_hover_text(name);
                if swatch.clicked() && !selected {
                    entry.color = color;
                    changed = true;
                }
            }
            changed |= ui.color_edit_button_srgb(&mut entry.color).changed();
            ui.add_space(4.0);
            // Show only the human-readable text; fall back to the stat id
            // when the catalog has no text for it (or is still loading).
            // Truncated so long entries never widen the row while scrolling.
            let display = text.unwrap_or(&entry.stat_id);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(display)
                        .size(11.5)
                        .color(if entry.enabled {
                            palette::TEXT
                        } else {
                            palette::TEXT_MUTED
                        }),
                )
                .truncate(),
            )
            .on_hover_text(&entry.stat_id);
        },
    );
    changed
}

/// Shows a modal with shared styling; returns true when it should close
/// (Esc or backdrop click).
fn confirm_modal(ctx: &egui::Context, id: &str, add_contents: impl FnOnce(&mut egui::Ui)) -> bool {
    egui::Modal::new(egui::Id::new(id))
        .frame(widgets::card().inner_margin(20))
        .backdrop_color(egui::Color32::from_black_alpha(140))
        .show(ctx, |ui| {
            ui.set_max_width(380.0);
            add_contents(ui);
        })
        .should_close()
}

fn patch_info(id: PatchId) -> &'static PatchInfo {
    all_patches()
        .iter()
        .find(|patch| patch.id == id)
        .expect("every PatchId has a PatchInfo")
}

/// "atlas-fog" -> "Atlas fog", "mtx-soft" -> "MTX soft".
fn display_name(name: &str) -> String {
    let spaced = name.replace('-', " ");
    let mut chars = spaced.chars();
    let capitalized = match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => spaced,
    };
    capitalized.replace("Mtx", "MTX").replace("mtx", "MTX")
}

fn patch_state_pill(state: PatchState) -> (&'static str, egui::Color32) {
    match state {
        PatchState::Clean => ("Not patched", palette::NEUTRAL),
        PatchState::Patched => ("Patched", palette::SUCCESS),
        PatchState::StaleBackup => ("Obsolete backup", palette::WARNING),
        PatchState::PatchedMissingBackup => ("Backup missing", palette::ERROR),
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

fn small_action(ui: &mut egui::Ui, text: &str) -> bool {
    ui.add(
        egui::Button::new(
            egui::RichText::new(text)
                .size(11.5)
                .color(palette::TEXT_MUTED),
        )
        .fill(egui::Color32::TRANSPARENT)
        .stroke(egui::Stroke::new(1.0, palette::BORDER)),
    )
    .clicked()
}
