use eframe::egui;
use tiny_poe2smoother::patches::{ColorModEntry, EffectLevel, PRESET_COLORS, default_color_mods};

use super::theme::{self, palette};
use super::widgets::{self, count_label, small_action};
use crate::{ColorRowRef, EffectsEditorTab, GuiApp};

const COLOR_ROW_HEIGHT: f32 = 26.0;

pub fn draw_color_editor(app: &mut GuiApp, ctx: &egui::Context) {
    let modal = egui::Modal::new(egui::Id::unique("color_mods_editor"))
        .frame(widgets::card().inner_margin(20))
        .backdrop_color(egui::Color32::from_black_alpha(140))
        .show(ctx, |ui| {
            ui.set_width(640.0);
            ui.label(theme::heading_text("Color mods"));
            ui.add_space(2.0);
            ui.label(theme::caption_text(
                "Color modifier text on waystones, items, and tablets.",
            ));
            ui.add_space(8.0);
            ui.add(
                egui::TextEdit::singleline(&mut app.color_search)
                    .hint_text("Search stat id or text (regex, \"quotes\", !exclude)")
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
            // Not configured yet. Any interaction promotes it into the config as enabled.
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

/// Returns true when the user changed anything.
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
            // Fall back to the stat id while the catalog has no text. Truncate so rows never widen.
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

const EFFECTS_SKILLS_DESCRIPTION: &str =
    "Toggled on strips heavy client effects (default) · off keeps original visuals.";
const EFFECTS_MONSTERS_DESCRIPTION: &str = "Toggled on strips heavy client effects (default) · off keeps original visuals. \
     Shared effects stay original for all monsters using them · unlisted monsters stay reduced.";
const EFFECTS_OTHERS_DESCRIPTION: &str = "Shared or unmapped effects (default reduced) · off keeps original visuals, including \
     referenced particles even with Particles enabled. Kept effects stay original for other users.";

pub fn draw_effects_editor(app: &mut GuiApp, ctx: &egui::Context) {
    let modal = egui::Modal::new(egui::Id::unique("effects_editor"))
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
                    (EffectsEditorTab::Others, "Others"),
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
                EffectsEditorTab::Others => draw_effects_others_tab(app, ui),
            }
        })
        .should_close();
    if modal {
        app.show_effects_editor = false;
    }
}

fn draw_effects_skills_tab(app: &mut GuiApp, ui: &mut egui::Ui) {
    draw_effects_tab_description(ui, EFFECTS_SKILLS_DESCRIPTION);
    ui.add_space(8.0);
    ui.add(
        egui::TextEdit::singleline(&mut app.effects_search)
            .id_salt("effects_skills_search")
            .hint_text("Search skills (regex, \"quotes\", !exclude)")
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

    if app.other_catalog_task.is_some() {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().color(palette::ACCENT));
            ui.label(theme::caption_text(
                "Loading shared effects for cross-tab preservation…",
            ));
        });
        ui.add_space(4.0);
    } else if app.other_catalog.is_none()
        && let Some(error) = app.other_catalog_error.clone()
    {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "Shared effects unavailable: {error}. Reductions that need them stay pending."
                ))
                .size(11.5)
                .color(palette::WARNING),
            );
            if small_action(ui, "Retry") {
                app.other_catalog_error = None;
                app.ensure_other_effect_catalog_loading();
            }
        });
        ui.add_space(4.0);
    }

    app.refresh_effects_filter();
    let row_count = app.effects_filter_rows.len();
    let bulk_folders: Vec<String> = {
        let catalog = app.effect_catalog.as_deref().unwrap_or_default();
        app.effects_filter_rows
            .iter()
            .flat_map(|&idx| catalog[idx].folders.iter().cloned())
            .collect()
    };
    let can_bulk_reduce = app.can_reduce_effect_folders(&bulk_folders);
    draw_effects_list_header(
        ui,
        |ui| {
            ui.label(theme::caption_text(&format!("{row_count} shown")));
        },
        |_| {},
    );
    if !can_bulk_reduce && row_count > 0 {
        ui.label(theme::caption_text(
            "Waiting for the skill and shared lists to keep sibling visuals. Reducing stays pending.",
        ));
    }
    ui.add_space(4.0);
    // Per-tab salt, or the lists would share one scroll offset.
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
            ui.add_enabled_ui(can_bulk_reduce, |ui| {
                if ui.add(widgets::secondary_button("Check all")).clicked() {
                    app.apply_effect_level_to_filtered_rows(EffectLevel::Reduced);
                }
            });
        });
    });
}

/// Reducing under an inherited parent expands it first.
fn draw_effect_skill_row(app: &mut GuiApp, ui: &mut egui::Ui, idx: usize) {
    let (folders, display, active_skill_id, action_type, level) = {
        let Some(catalog) = &app.effect_catalog else {
            return;
        };
        let row = &catalog[idx];
        (
            row.folders.clone(),
            row.display.clone(),
            row.active_skill_id.clone(),
            row.action_type.clone(),
            app.effect_level_for_folders(&row.folders),
        )
    };
    let reducible = app.can_reduce_effect_folders(&folders);
    let mixed = level.is_none();
    let mut reduced = level == Some(EffectLevel::Reduced);
    let blocked = level != Some(EffectLevel::Reduced) && !reducible;
    let hover = format!(
        "{}\n{}\n{}",
        active_skill_id,
        action_type,
        folders.join("\n")
    );
    let hover = if blocked {
        format!("{hover}\nWaiting for skill/shared lists to preserve sibling visuals…")
    } else {
        hover
    };
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), COLOR_ROW_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let toggle = ui.add_enabled_ui(!blocked, |ui| {
                ui.add(egui::Checkbox::without_text(&mut reduced).indeterminate(mixed))
            });
            let toggle = toggle.inner;
            if toggle.changed() {
                let new_level = if reduced {
                    EffectLevel::Reduced
                } else {
                    EffectLevel::Full
                };
                app.set_skill_folders_level(&folders, new_level);
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

fn draw_effects_others_tab(app: &mut GuiApp, ui: &mut egui::Ui) {
    draw_effects_tab_description(ui, EFFECTS_OTHERS_DESCRIPTION);
    ui.add_space(8.0);
    ui.add(
        egui::TextEdit::singleline(&mut app.others_search)
            .id_salt("effects_others_search")
            .hint_text("Search shared effects (regex, \"quotes\", !exclude)")
            .desired_width(f32::INFINITY)
            .margin(egui::Margin::symmetric(10, 8)),
    );
    ui.add_space(6.0);
    if app.other_catalog_task.is_some() {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().color(palette::ACCENT));
            ui.label(theme::caption_text(
                "Loading shared effects list from game files…",
            ));
        });
        ui.add_space(4.0);
    } else if let Some(error) = app.other_catalog_error.clone() {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("Shared effects list unavailable: {error}"))
                    .size(11.5)
                    .color(palette::WARNING),
            );
            if small_action(ui, "Retry") {
                app.other_catalog_error = None;
                app.ensure_other_effect_catalog_loading();
            }
        });
        ui.add_space(4.0);
    }

    if app.effect_catalog_task.is_some() {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().color(palette::ACCENT));
            ui.label(theme::caption_text(
                "Loading skill list for cross-tab preservation…",
            ));
        });
        ui.add_space(4.0);
    } else if app.effect_catalog.is_none()
        && let Some(error) = app.effect_catalog_error.clone()
    {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "Skill list unavailable: {error}. Reductions that need it stay pending."
                ))
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

    app.refresh_others_filter();
    let row_count = app.others_filter_rows.len();
    let bulk_folders: Vec<String> = {
        let catalog = app.other_catalog.as_deref().unwrap_or_default();
        app.others_filter_rows
            .iter()
            .map(|&idx| catalog[idx].folder.clone())
            .collect()
    };
    let can_bulk_reduce = app.can_reduce_effect_folders(&bulk_folders);
    draw_effects_list_header(
        ui,
        |ui| {
            ui.label(theme::caption_text(&format!("{row_count} shown")));
        },
        |_| {},
    );
    if !can_bulk_reduce && row_count > 0 {
        ui.label(theme::caption_text(
            "Waiting for the skill and shared lists to keep sibling visuals. Reducing stays pending.",
        ));
    }
    ui.add_space(4.0);
    // Per-tab salt, or the lists would share one scroll offset.
    egui::ScrollArea::vertical()
        .id_salt("effects_others_list")
        .max_height(380.0)
        .auto_shrink([false, false])
        .show_rows(ui, COLOR_ROW_HEIGHT, row_count, |ui, range| {
            for i in range {
                let idx = app.others_filter_rows[i];
                draw_other_effect_row(app, ui, idx);
            }
        });

    ui.add_space(10.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.add(widgets::primary_button("Close")).clicked() {
            app.show_effects_editor = false;
        }
        ui.add_enabled_ui(row_count > 0, |ui| {
            if ui.add(widgets::secondary_button("Uncheck all")).clicked() {
                app.apply_other_effect_level_to_filtered_rows(EffectLevel::Full);
            }
            ui.add_enabled_ui(can_bulk_reduce, |ui| {
                if ui.add(widgets::secondary_button("Check all")).clicked() {
                    app.apply_other_effect_level_to_filtered_rows(EffectLevel::Reduced);
                }
            });
        });
    });
}

/// Reducing under an inherited parent expands it first so siblings stay Full.
fn draw_other_effect_row(app: &mut GuiApp, ui: &mut egui::Ui, idx: usize) {
    let (folder, display, level) = {
        let Some(catalog) = &app.other_catalog else {
            return;
        };
        let row = &catalog[idx];
        (
            row.folder.clone(),
            row.display.clone(),
            app.other_effect_level(&row.folder),
        )
    };
    let reducible = app.can_reduce_effect_folders(std::slice::from_ref(&folder));
    let mut reduced = level == EffectLevel::Reduced;
    let blocked = level == EffectLevel::Full && !reducible;
    let hover = format!("metadata/effects/spells/{folder}");
    let hover = if blocked {
        format!("{hover}\nWaiting for skill/shared lists to preserve sibling visuals…")
    } else {
        hover
    };
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), COLOR_ROW_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let toggle = ui.add_enabled_ui(!blocked, |ui| {
                ui.add(egui::Checkbox::without_text(&mut reduced))
            });
            let toggle = toggle.inner;
            if toggle.changed() {
                app.set_other_effect_level(
                    &folder,
                    if reduced {
                        EffectLevel::Reduced
                    } else {
                        EffectLevel::Full
                    },
                );
            }
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(egui::RichText::new(&display).size(11.5).color(
                    if level == EffectLevel::Reduced {
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

fn draw_effects_monsters_tab(app: &mut GuiApp, ui: &mut egui::Ui) {
    draw_effects_tab_description(ui, EFFECTS_MONSTERS_DESCRIPTION);
    ui.add_space(8.0);
    ui.add(
        egui::TextEdit::singleline(&mut app.monsters_search)
            .id_salt("effects_monsters_search")
            .hint_text("Search monsters (regex, \"quotes\", !exclude)")
            .desired_width(f32::INFINITY)
            .margin(egui::Margin::symmetric(10, 8)),
    );
    ui.add_space(6.0);
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

    let unnamed_rows = app.unnamed_monster_row_count();
    let overridden = app.overridden_unnamed_monster_row_count();
    let show_unnamed = std::cell::Cell::new(app.show_unnamed_monsters);
    let (row_count, ()) = draw_effects_list_header(
        ui,
        |ui| {
            // The right side renders first, so this frame's filter and count see the change.
            app.show_unnamed_monsters = show_unnamed.get();
            app.refresh_monsters_filter();
            let row_count = app.monsters_filter_rows.len();
            ui.label(theme::caption_text(&format!("{row_count} shown")));
            row_count
        },
        |ui| {
            if unnamed_rows > 0 {
                // Own ID space, since the Skills tab puts widgets at the same position.
                let mut show = show_unnamed.get();
                ui.push_id("effects_monsters_show_unnamed", |ui| {
                    ui.checkbox(
                        &mut show,
                        egui::RichText::new(format!("Show unnamed entities ({unnamed_rows})"))
                            .size(11.5),
                    )
                    .on_hover_text(
                        "Internal rows with no in-game name. While hidden, rows keeping \
                         original visuals are still counted here.",
                    );
                });
                if !show && overridden > 0 {
                    if small_action(ui, "Show") {
                        show = true;
                    }
                    ui.label(theme::caption_text(&format!(
                        "{} keeping original visuals hidden",
                        count_label(overridden, "unnamed row")
                    )));
                }
                show_unnamed.set(show);
            }
        },
    );
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

fn draw_effects_tab_description(ui: &mut egui::Ui, text: &str) {
    // Reserve two lines on every tab so the search field doesn't jump.
    let height = 2.0 * ui.text_style_height(&egui::TextStyle::Small);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    )
    .add(egui::Label::new(theme::caption_text(text)).wrap());
}

fn draw_effects_list_header<L, R>(
    ui: &mut egui::Ui,
    add_left: impl FnOnce(&mut egui::Ui) -> L,
    add_right: impl FnOnce(&mut egui::Ui) -> R,
) -> (L, R) {
    // A fixed height keeps the list anchored across tabs.
    const HEIGHT: f32 = 32.0;
    // Right first so a monster-control change updates the count this frame.
    egui::Sides::new()
        .height(HEIGHT)
        .shrink_left()
        .show(ui, add_left, add_right)
}

/// Controls every variant sharing the name. Checking it clears all their overrides.
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
            // Truncate the muted context suffix so the name keeps priority.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monster_context_label_formats_the_rendered_suffix() {
        assert_eq!(
            monster_context_label("Viper Legionnaire"),
            "· Viper Legionnaire"
        );
    }

    /// Every tab's search field sits at the same widget position, so each needs its own salt.
    #[test]
    fn effects_tab_search_fields_persist_state_under_distinct_ids() {
        use egui::widgets::text_edit::TextEditState;

        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        let mut app = GuiApp::default();
        ctx.run_ui(egui::RawInput::default(), |ui| {
            draw_effects_skills_tab(&mut app, ui);
        })
        .drop_without_applying_deltas();
        assert_eq!(ctx.data(|d| d.count::<TextEditState>()), 1);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            draw_effects_others_tab(&mut app, ui);
        })
        .drop_without_applying_deltas();
        assert_eq!(ctx.data(|d| d.count::<TextEditState>()), 2);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            draw_effects_monsters_tab(&mut app, ui);
        })
        .drop_without_applying_deltas();
        assert_eq!(ctx.data(|d| d.count::<TextEditState>()), 3);
    }

    #[test]
    fn effects_tab_descriptions_reserve_the_same_height() {
        fn description_bottom(text: &str) -> f32 {
            let ctx = egui::Context::default();
            theme::install_fonts(&ctx);
            let mut bottom = 0.0;
            ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.set_width(640.0);
                draw_effects_tab_description(ui, text);
                bottom = ui.cursor().top();
            })
            .drop_without_applying_deltas();
            bottom
        }

        assert_eq!(
            description_bottom(EFFECTS_SKILLS_DESCRIPTION),
            description_bottom(EFFECTS_MONSTERS_DESCRIPTION)
        );
        assert_eq!(
            description_bottom(EFFECTS_SKILLS_DESCRIPTION),
            description_bottom(EFFECTS_OTHERS_DESCRIPTION)
        );
    }

    #[test]
    fn effects_list_header_keeps_count_at_the_left_anchor() {
        fn count_left(with_monster_control: bool) -> f32 {
            let ctx = egui::Context::default();
            theme::install_fonts(&ctx);
            theme::install_style(&ctx);
            let mut left = 0.0;
            ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.set_width(640.0);
                let mut show_unnamed = false;
                let response = if with_monster_control {
                    let (count, ()) = draw_effects_list_header(
                        ui,
                        |ui| ui.label(theme::caption_text("539 shown")),
                        |ui| {
                            ui.checkbox(&mut show_unnamed, "Show unnamed entities (427)");
                        },
                    );
                    count
                } else {
                    let (count, ()) = draw_effects_list_header(
                        ui,
                        |ui| ui.label(theme::caption_text("287 shown")),
                        |_| {},
                    );
                    count
                };
                left = response.rect.left();
            })
            .drop_without_applying_deltas();
            left
        }

        assert_eq!(count_left(false), count_left(true));
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

    #[test]
    fn effects_tabs_keep_the_same_vertical_extent_with_monster_controls() {
        fn tab_height(app: &mut GuiApp, draw_tab: fn(&mut GuiApp, &mut egui::Ui)) -> f32 {
            let ctx = egui::Context::default();
            theme::install_fonts(&ctx);
            theme::install_style(&ctx);
            let mut height = 0.0;
            ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.set_width(640.0);
                let top = ui.cursor().top();
                draw_tab(app, ui);
                height = ui.cursor().top() - top;
            })
            .drop_without_applying_deltas();
            height
        }

        let mut skills = GuiApp::default();
        let mut monsters = GuiApp {
            monster_catalog: Some(vec![monster_row(
                "internal helper",
                &["metadata/monsters/internal/helper"],
                false,
            )]),
            ..GuiApp::default()
        };

        assert_eq!(
            tab_height(&mut skills, draw_effects_skills_tab),
            tab_height(&mut monsters, draw_effects_monsters_tab)
        );
        let mut others = GuiApp::default();
        assert_eq!(
            tab_height(&mut skills, draw_effects_skills_tab),
            tab_height(&mut others, draw_effects_others_tab)
        );
    }

    /// Fallback rows stay hidden until the checkbox is on, and the hint counts hidden overrides.
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

        ctx.run_ui(egui::RawInput::default(), |ui| {
            draw_effects_monsters_tab(&mut app, ui);
        })
        .drop_without_applying_deltas();
        assert_eq!(app.monsters_filter_rows, vec![0]);
        assert_eq!(app.overridden_unnamed_monster_row_count(), 1);

        app.show_unnamed_monsters = true;
        ctx.run_ui(egui::RawInput::default(), |ui| {
            draw_effects_monsters_tab(&mut app, ui);
        })
        .drop_without_applying_deltas();
        assert_eq!(app.monsters_filter_rows, vec![0, 1]);
    }

    fn other_row(folder: &str, display: &str) -> crate::OtherEffectRow {
        crate::OtherEffectRow {
            display_lower: display.to_lowercase(),
            search_lower: format!("{display} {folder}").to_lowercase(),
            folder: folder.to_string(),
            display: display.to_string(),
        }
    }

    #[test]
    fn others_tab_keeps_independent_filter_state_from_skills_tab() {
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        let mut app = GuiApp {
            other_catalog: Some(vec![
                other_row("ground_effects/fire", "Ground Fire"),
                other_row("ambient_sparkles.ao", "Ambient Sparkles"),
            ]),
            ..GuiApp::default()
        };

        ctx.run_ui(egui::RawInput::default(), |ui| {
            draw_effects_others_tab(&mut app, ui);
        })
        .drop_without_applying_deltas();
        assert_eq!(app.others_filter_rows, vec![0, 1]);
        assert!(app.effects_filter_rows.is_empty());

        app.others_search = "ambient".to_string();
        ctx.run_ui(egui::RawInput::default(), |ui| {
            draw_effects_others_tab(&mut app, ui);
        })
        .drop_without_applying_deltas();
        assert_eq!(app.others_filter_rows, vec![1]);
        assert!(app.effects_filter_rows.is_empty());
    }
}
