use eframe::egui;
use tiny_poe2smoother::app::PatchState;
use tiny_poe2smoother::install::{display_path, is_game_running};
use tiny_poe2smoother::patches::{
    all_patches, all_presets, default_color_mods, ColorModEntry, EffectLevel, PatchId, PatchInfo,
    PRESET_COLORS,
};

use super::icon;
use super::theme::{self, palette};
use super::widgets;
use crate::{ColorRowRef, EffectsEditorTab, GuiApp, MessageKind};

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
            PatchId::BlackScreen,
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

// Column packing chosen to roughly balance column heights (3+3+2 vs 6+5).
const COLUMNS: [&[usize]; 2] = [&[0, 3, 4], &[1, 2]];

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
            if patch_id == PatchId::Effects {
                draw_effects_row(app, ui);
            }
        }
    });
}

/// `1 skill`, `2 skills` — count with a singular/plural noun.
fn count_label(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
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
        ui.label(theme::caption_text(&format!(
            "{} enabled",
            count_label(enabled, "mod")
        )));
    });
}

fn draw_effects_row(app: &mut GuiApp, ui: &mut egui::Ui) {
    if !app.selected_patches.contains(&PatchId::Effects) {
        return;
    }
    // The caption groups per skill only once the catalog is in, so start the
    // load right away instead of waiting for the editor to be opened. The
    // error guard keeps a failed load from respawning the loader every frame;
    // the editor's Retry button clears it for manual retries. The monster
    // catalog reads every monster metadata file, so it only loads once its
    // editor tab is shown.
    if app.effect_catalog.is_none()
        && app.effect_catalog_task.is_none()
        && app.effect_catalog_error.is_none()
    {
        app.ensure_effect_catalog_loading();
    }
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        if small_action(ui, "Edit effects…") {
            app.show_effects_editor = true;
            app.ensure_active_effects_tab_loading();
        }
        let skills = app.kept_original_effect_skill_count();
        let monsters = app.kept_original_monster_count();
        let caption = if skills == 0 && monsters == 0 {
            "all effects reduced".to_string()
        } else {
            format!(
                "{} · {} kept original",
                count_label(skills, "skill"),
                count_label(monsters, "monster")
            )
        };
        ui.label(theme::caption_text(&caption));
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

    if app.show_effects_editor {
        draw_effects_editor(app, ctx);
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
                    .hint_text(
                        "Search stat id or text — regex like in-game search, \"quotes\", !exclude…",
                    )
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

/// The effects editor: one modal, a tab per exception list (skills and
/// monsters). Each tab is a search box over its catalog with one on/off
/// smoothing toggle per row, same layout as the color-mods editor. Tab
/// bodies keep separate search/filter/scroll state, and the bulk buttons
/// only ever touch the active tab's filtered rows.
fn draw_effects_editor(app: &mut GuiApp, ctx: &egui::Context) {
    let modal = egui::Modal::new(egui::Id::new("effects_editor"))
        .frame(widgets::card().inner_margin(20))
        .backdrop_color(egui::Color32::from_black_alpha(140))
        .show(ctx, |ui| {
            ui.set_width(640.0);
            ui.horizontal(|ui| {
                ui.label(theme::heading_text("Effects"));
                ui.add_space(8.0);
                for (tab, label) in [
                    (EffectsEditorTab::Skills, "Skills"),
                    (EffectsEditorTab::Monsters, "Monsters"),
                ] {
                    let active = app.effects_editor_tab == tab;
                    if ui.add(widgets::chip(label, active)).clicked() && !active {
                        app.effects_editor_tab = tab;
                        app.ensure_active_effects_tab_loading();
                    }
                }
            });
            ui.add_space(2.0);
            match app.effects_editor_tab {
                EffectsEditorTab::Skills => draw_effects_skills_tab(app, ui),
                EffectsEditorTab::Monsters => draw_effects_monsters_tab(app, ui),
            }
        })
        .should_close();
    if modal {
        app.show_effects_editor = false;
    }
}

fn draw_effects_skills_tab(app: &mut GuiApp, ui: &mut egui::Ui) {
    ui.label(theme::caption_text(
        "Toggled on strips heavy client effects (default) · off keeps original visuals.",
    ));
    ui.add_space(8.0);
    ui.add(
        egui::TextEdit::singleline(&mut app.effects_search)
            .id_salt("effects_skills_search")
            .hint_text("Search skills — regex like in-game search, \"quotes\", !exclude…")
            .desired_width(f32::INFINITY)
            .margin(egui::Margin::symmetric(10, 8)),
    );
    ui.add_space(6.0);
    if app.effect_catalog_task.is_some() {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().color(palette::ACCENT));
            ui.label(theme::caption_text("Loading skill list from game files…"));
        });
        ui.add_space(4.0);
    } else if let Some(error) = app.effect_catalog_error.clone() {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("Skill list unavailable: {error}"))
                    .size(11.5)
                    .color(palette::WARNING),
            );
            if small_action(ui, "Retry") {
                app.effect_catalog_error = None;
                app.ensure_effect_catalog_loading();
            }
        });
        ui.add_space(4.0);
    }

    app.refresh_effects_filter();
    let row_count = app.effects_filter_rows.len();
    ui.horizontal(|ui| {
        ui.label(theme::caption_text(&format!("{row_count} shown")));
    });
    ui.add_space(4.0);
    // Distinct salt per tab: both lists sit at the same spot in the modal,
    // so without it they would share one persisted scroll offset.
    egui::ScrollArea::vertical()
        .id_salt("effects_skills_list")
        .max_height(380.0)
        .auto_shrink([false, false])
        .show_rows(ui, COLOR_ROW_HEIGHT, row_count, |ui, range| {
            for i in range {
                let idx = app.effects_filter_rows[i];
                draw_effect_skill_row(app, ui, idx);
            }
        });

    ui.add_space(10.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.add(widgets::primary_button("Close")).clicked() {
            app.show_effects_editor = false;
        }
        ui.add_enabled_ui(row_count > 0, |ui| {
            if ui.add(widgets::secondary_button("Uncheck all")).clicked() {
                app.apply_effect_level_to_filtered_rows(EffectLevel::Full);
            }
            if ui.add(widgets::secondary_button("Check all")).clicked() {
                app.apply_effect_level_to_filtered_rows(EffectLevel::Reduced);
            }
        });
    });
}

/// One skill row: smoothing toggle plus the skill name. A row can control
/// multiple effect folders for skills whose visuals are split across
/// buff/explosion/etc. folders. Toggling on removes all overrides.
fn draw_effect_skill_row(app: &mut GuiApp, ui: &mut egui::Ui, idx: usize) {
    let Some(catalog) = &app.effect_catalog else {
        return;
    };
    let row = &catalog[idx];
    let folders = row.folders.clone();
    let display = row.display.clone();
    let level = app.effect_level_for_folders(&folders);
    let mixed = level.is_none();
    let mut reduced = level == Some(EffectLevel::Reduced);
    let hover = format!(
        "{}\n{}\n{}",
        row.active_skill_id,
        row.action_type,
        folders.join("\n")
    );
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), COLOR_ROW_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let toggle = ui.add(egui::Checkbox::without_text(&mut reduced).indeterminate(mixed));
            if toggle.changed() {
                let new_level = if reduced {
                    EffectLevel::Reduced
                } else {
                    EffectLevel::Full
                };
                for folder in &folders {
                    if new_level == EffectLevel::Reduced {
                        app.effect_overrides.remove(folder);
                    } else {
                        app.effect_overrides.insert(folder.clone(), new_level);
                    }
                }
            }
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(egui::RichText::new(&display).size(11.5).color(
                    if level == Some(EffectLevel::Reduced) {
                        palette::TEXT_MUTED
                    } else {
                        palette::TEXT
                    },
                ))
                .truncate(),
            )
            .on_hover_text(&hover);
            toggle.on_hover_text(hover);
        },
    );
}

/// The monsters tab: search box over the monsters mapped from the game
/// files, one row per monster name with an on/off smoothing toggle. Same
/// layout as the skills tab.
fn draw_effects_monsters_tab(app: &mut GuiApp, ui: &mut egui::Ui) {
    ui.label(theme::caption_text(
        "Toggled on strips heavy client effects (default) · off keeps original visuals. \
         Shared effects stay original for all monsters using them · unlisted monsters stay reduced.",
    ));
    ui.add_space(8.0);
    ui.add(
        egui::TextEdit::singleline(&mut app.monsters_search)
            .id_salt("effects_monsters_search")
            .hint_text("Search monsters — regex like in-game search, \"quotes\", !exclude…")
            .desired_width(f32::INFINITY)
            .margin(egui::Margin::symmetric(10, 8)),
    );
    ui.add_space(6.0);
    let unnamed_rows = app.unnamed_monster_row_count();
    if unnamed_rows > 0 {
        // Distinct ID space: the Skills tab renders widgets at the same
        // position and egui would otherwise share state across tabs.
        ui.push_id("effects_monsters_show_unnamed", |ui| {
            ui.checkbox(
                &mut app.show_unnamed_monsters,
                egui::RichText::new(format!("Show unnamed entities ({unnamed_rows})")).size(11.5),
            )
            .on_hover_text(
                "Internal rows with no in-game name. While hidden, rows keeping \
                 original visuals are flagged below — never dropped silently.",
            );
        });
        if !app.show_unnamed_monsters {
            let overridden = app.overridden_unnamed_monster_row_count();
            if overridden > 0 {
                ui.horizontal(|ui| {
                    ui.label(theme::caption_text(&format!(
                        "{} keeping original visuals hidden",
                        count_label(overridden, "unnamed row")
                    )));
                    if small_action(ui, "Show") {
                        app.show_unnamed_monsters = true;
                    }
                });
            }
        }
        ui.add_space(6.0);
    }
    if app.monster_catalog_task.is_some() {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().color(palette::ACCENT));
            ui.label(theme::caption_text(
                "Loading monster list from game files… (first open takes a moment)",
            ));
        });
        ui.add_space(4.0);
    } else if let Some(error) = app.monster_catalog_error.clone() {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("Monster list unavailable: {error}"))
                    .size(11.5)
                    .color(palette::WARNING),
            );
            if small_action(ui, "Retry") {
                app.monster_catalog_error = None;
                app.ensure_monster_catalog_loading();
            }
        });
        ui.add_space(4.0);
    }

    app.refresh_monsters_filter();
    let row_count = app.monsters_filter_rows.len();
    ui.horizontal(|ui| {
        ui.label(theme::caption_text(&format!("{row_count} shown")));
    });
    ui.add_space(4.0);
    egui::ScrollArea::vertical()
        .id_salt("effects_monsters_list")
        .max_height(380.0)
        .auto_shrink([false, false])
        .show_rows(ui, COLOR_ROW_HEIGHT, row_count, |ui, range| {
            for i in range {
                let idx = app.monsters_filter_rows[i];
                draw_monster_row(app, ui, idx);
            }
        });

    ui.add_space(10.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.add(widgets::primary_button("Close")).clicked() {
            app.show_effects_editor = false;
        }
        ui.add_enabled_ui(row_count > 0, |ui| {
            if ui.add(widgets::secondary_button("Uncheck all")).clicked() {
                app.apply_monster_level_to_filtered_rows(EffectLevel::Full);
            }
            if ui.add(widgets::secondary_button("Check all")).clicked() {
                app.apply_monster_level_to_filtered_rows(EffectLevel::Reduced);
            }
        });
    });
}

/// One monster row: smoothing toggle plus the monster name. A row controls
/// every variant sharing the display name (runemarked etc.); toggling on
/// removes all their overrides.
fn draw_monster_row(app: &mut GuiApp, ui: &mut egui::Ui, idx: usize) {
    let Some(catalog) = &app.monster_catalog else {
        return;
    };
    let row = &catalog[idx];
    let keys = row.monster_keys.clone();
    let display = row.display.clone();
    let context = row.context.clone();
    let level = app.monster_level_for_keys(&keys);
    let mixed = level.is_none();
    let mut reduced = level == Some(EffectLevel::Reduced);
    let hover = hover_key_list(&keys);
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), COLOR_ROW_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let toggle = ui.add(egui::Checkbox::without_text(&mut reduced).indeterminate(mixed));
            if toggle.changed() {
                let new_level = if reduced {
                    EffectLevel::Reduced
                } else {
                    EffectLevel::Full
                };
                for key in &keys {
                    if new_level == EffectLevel::Reduced {
                        app.monster_overrides.remove(key);
                    } else {
                        app.monster_overrides.insert(key.clone(), new_level);
                    }
                }
            }
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(egui::RichText::new(&display).size(11.5).color(
                    if level == Some(EffectLevel::Reduced) {
                        palette::TEXT_MUTED
                    } else {
                        palette::TEXT
                    },
                ))
                .truncate(),
            )
            .on_hover_text(&hover);
            // Fallback rows carry a muted "where does this belong" suffix;
            // truncation keeps the row single-line, the name has priority.
            if let Some(context) = &context {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(monster_context_label(context))
                            .size(11.5)
                            .color(palette::TEXT_MUTED),
                    )
                    .truncate(),
                )
                .on_hover_text(&hover);
            }
            toggle.on_hover_text(hover);
        },
    );
}

fn monster_context_label(context: &str) -> String {
    format!("· {context}")
}

/// Variant keys for a row hover, truncated so huge groups stay readable.
fn hover_key_list(keys: &[String]) -> String {
    const MAX_HOVER_KEYS: usize = 12;
    let mut hover = keys
        .iter()
        .take(MAX_HOVER_KEYS)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    if keys.len() > MAX_HOVER_KEYS {
        hover.push_str(&format!("\n… {} more", keys.len() - MAX_HOVER_KEYS));
    }
    hover
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_label_pluralizes() {
        assert_eq!(count_label(0, "skill"), "0 skills");
        assert_eq!(count_label(1, "skill"), "1 skill");
        assert_eq!(count_label(2, "monster"), "2 monsters");
    }

    #[test]
    fn monster_context_label_formats_the_rendered_suffix() {
        assert_eq!(
            monster_context_label("Viper Legionnaire"),
            "· Viper Legionnaire"
        );
    }

    /// Both tab search fields sit at the same auto-widget position in the
    /// modal, so without distinct salts egui persists one shared
    /// `TextEditState` (cursor, selection, undo history) across tabs.
    #[test]
    fn effects_tab_search_fields_persist_state_under_distinct_ids() {
        use egui::widgets::text_edit::TextEditState;

        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        let mut app = GuiApp::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            draw_effects_skills_tab(&mut app, ui);
        });
        assert_eq!(ctx.data(|d| d.count::<TextEditState>()), 1);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            draw_effects_monsters_tab(&mut app, ui);
        });
        // A second state entry proves the two fields have distinct widget
        // IDs; a shared ID would overwrite the first entry in place.
        assert_eq!(ctx.data(|d| d.count::<TextEditState>()), 2);
    }

    fn monster_row(display: &str, keys: &[&str], named: bool) -> crate::MonsterCatalogRow {
        let monster_keys: Vec<String> = keys.iter().map(|key| key.to_string()).collect();
        crate::MonsterCatalogRow {
            display_lower: display.to_lowercase(),
            search_lower: format!("{} {}", display.to_lowercase(), monster_keys.join(" ")),
            display: display.to_string(),
            monster_keys,
            context: None,
            named,
        }
    }

    /// The rendered monsters tab default-hides fallback rows; ticking
    /// "Show unnamed entities" (state flip, as the checkbox does) reveals
    /// them, and an overridden hidden row is flagged by the hint count
    /// rather than silently dropped.
    #[test]
    fn monsters_tab_hides_unnamed_rows_until_checkbox_enabled() {
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        let mut app = GuiApp::default();
        let wolf = "metadata/monsters/baron/phase2/baronphase2wolf";
        let mut fallback = monster_row("baronphase 2 wolf", &[wolf], false);
        fallback.context = Some("Count Geonor / Geonor, the Putrid Wolf".to_string());
        app.monster_catalog = Some(vec![
            monster_row("Bog Hulk", &["metadata/monsters/boghulk/boghulk"], true),
            fallback,
        ]);
        app.monster_overrides
            .insert(wolf.to_string(), EffectLevel::Full);

        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            draw_effects_monsters_tab(&mut app, ui);
        });
        assert_eq!(app.monsters_filter_rows, vec![0]);
        assert_eq!(app.overridden_unnamed_monster_row_count(), 1);

        app.show_unnamed_monsters = true;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            draw_effects_monsters_tab(&mut app, ui);
        });
        assert_eq!(app.monsters_filter_rows, vec![0, 1]);
    }
}
