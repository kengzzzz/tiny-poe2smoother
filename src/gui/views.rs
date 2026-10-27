use eframe::egui;
use tiny_poe2smoother::app::{AppStatus, PatchState};
use tiny_poe2smoother::install::{InstallLayout, display_path, is_game_running};
use tiny_poe2smoother::patches::{MinimapStyle, PatchId, all_profiles, patch_info, patch_options};

use super::editors;
use super::icon;
use super::theme::{self, palette};
use super::widgets::{self, count_label, small_action};
use crate::{GuiApp, MessageKind};

pub struct Category {
    pub title: &'static str,
    pub description: &'static str,
    pub patches: &'static [PatchId],
}

pub const CATEGORIES: &[Category] = &[
    Category {
        title: "Camera & Map",
        description: "Camera distance, minimap reveal, and the Atlas.",
        patches: &[PatchId::Camera, PatchId::Minimap, PatchId::AtlasFog],
    },
    Category {
        title: "Environment",
        description: "Weather, ambient particles, shadows, and lighting in zones.",
        patches: &[PatchId::Atmosphere, PatchId::Shadow, PatchId::Light],
    },
    Category {
        title: "Screen",
        description: "Full-screen filters drawn over the world.",
        patches: &[PatchId::PostProcess, PatchId::BlackScreen],
    },
    Category {
        title: "Effects",
        description: "Skill, monster, league, and microtransaction visuals.",
        patches: &[
            PatchId::Effects,
            PatchId::Particles,
            PatchId::Delirium,
            PatchId::MtxSoft,
        ],
    },
    Category {
        title: "Audio",
        description: "Effect sounds. Music and voices are not affected.",
        patches: &[
            PatchId::DisableSounds,
            PatchId::SkillSounds,
            PatchId::MonsterSounds,
        ],
    },
    Category {
        title: "Interface",
        description: "Tooltips, item text, and health bars.",
        patches: &[
            PatchId::ColorMods,
            PatchId::CompactModLines,
            PatchId::MonsterHpBar,
        ],
    },
];

pub fn draw(app: &mut GuiApp, ui: &mut egui::Ui) {
    draw_header(app, ui);
    draw_footer(app, ui);
    draw_sidebar(app, ui);

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(palette::BG_APP)
                .inner_margin(egui::Margin::symmetric(24, 18)),
        )
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_enabled_ui(!app.is_busy(), |ui| draw_category(app, ui));
                });
        });

    let ctx = ui.ctx().clone();
    draw_modals(app, &ctx);
}

fn draw_header(app: &mut GuiApp, ui: &mut egui::Ui) {
    egui::Panel::top("header")
        .frame(
            egui::Frame::new()
                .fill(palette::BG_CARD)
                .stroke(egui::Stroke::new(1.0, palette::BORDER_FAINT))
                .inner_margin(egui::Margin::symmetric(20, 14)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
                icon::paint_logo_mark(ui.painter(), rect);
                ui.add_space(2.0);
                ui.label(theme::title_text("tiny-poe2smoother"));
                ui.label(theme::caption_text(concat!("v", env!("CARGO_PKG_VERSION"))));
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| match &app.status {
                        Some(status) => {
                            let (text, color) = patch_state_pill(status.patch_state);
                            widgets::status_pill(ui, text, color);
                        }
                        None => widgets::status_pill(ui, "No install", palette::WARNING),
                    },
                );
            });
            ui.add_space(10.0);
            ui.add_enabled_ui(!app.is_busy(), |ui| {
                draw_install_row(app, ui);
                ui.add_space(12.0);
                draw_profile_bar(app, ui);
            });
        });
}

fn draw_install_row(app: &mut GuiApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let buttons_width = 190.0;
        let response = ui.add(
            egui::TextEdit::singleline(&mut app.game_dir_input)
                .font(egui::TextStyle::Monospace)
                .hint_text("Path of Exile 2 install folder")
                .desired_width(ui.available_width() - buttons_width)
                .margin(egui::Margin::symmetric(10, 7)),
        );
        if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            app.spawn_status();
        }
        if ui.button("Browse…").clicked()
            && let Some(path) = rfd::FileDialog::new().pick_folder()
        {
            app.select_game_dir(path);
        }
        if ui
            .button("Detect")
            .on_hover_text("Find the Steam or standalone install automatically.")
            .clicked()
        {
            app.spawn_autodetect();
        }
    });
    if let Some(status) = &app.status {
        let (text, color) = install_summary(status);
        ui.label(egui::RichText::new(text).size(11.5).color(color));
    }
}

fn install_summary(status: &AppStatus) -> (String, egui::Color32) {
    let layout = match status.install_layout {
        InstallLayout::LooseBundles => "Steam install",
        InstallLayout::ContentGgpk => "Standalone install",
    };
    match status.patch_state {
        PatchState::Clean => (
            format!("{layout} · original game files"),
            palette::TEXT_FAINT,
        ),
        PatchState::Patched => (
            format!("{layout} · patched, backup ready to restore"),
            palette::TEXT_FAINT,
        ),
        PatchState::StaleBackup => (
            format!(
                "{layout} · not patched, but an old backup is left over. Apply replaces it and Restore removes it"
            ),
            palette::WARNING,
        ),
        PatchState::PatchedMissingBackup => (
            format!(
                "{layout} · patched but the backup is missing. Verify the game files in Steam or the launcher"
            ),
            palette::ERROR,
        ),
        PatchState::OtherInstallBackup => (
            format!(
                "{layout} · the backup belongs to {}. Restore that install first",
                status
                    .backup_owner
                    .as_deref()
                    .map(display_path)
                    .unwrap_or_else(|| "another install".to_string())
            ),
            palette::WARNING,
        ),
    }
}

fn draw_profile_bar(app: &mut GuiApp, ui: &mut egui::Ui) {
    let size = egui::vec2(ui.available_width(), 30.0);
    ui.allocate_ui_with_layout(
        size,
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(
                egui::RichText::new("Profile")
                    .size(12.0)
                    .family(theme::family_medium())
                    .color(palette::TEXT_FAINT),
            );
            ui.add_space(4.0);
            let active = app.active_profile();
            ui.spacing_mut().item_spacing.x = 6.0;
            for (index, profile) in all_profiles().iter().enumerate() {
                let response = widgets::segment(ui, profile.title, active == Some(index), true)
                    .on_hover_text(profile.description);
                if response.clicked() {
                    app.apply_profile(profile);
                }
            }
            widgets::segment(ui, "Custom", active.is_none(), false)
                .on_hover_text("Your own selection. Pick a profile to replace it.");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if small_action(ui, "Clear all") {
                    app.selected_patches.clear();
                }
            });
        },
    );
}

fn draw_sidebar(app: &mut GuiApp, ui: &mut egui::Ui) {
    egui::Panel::left("categories")
        .resizable(false)
        .exact_size(200.0)
        .frame(
            egui::Frame::new()
                .fill(palette::BG_APP)
                .stroke(egui::Stroke::new(1.0, palette::BORDER_FAINT))
                .inner_margin(egui::Margin::symmetric(12, 18)),
        )
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for (index, category) in CATEGORIES.iter().enumerate() {
                let count = category
                    .patches
                    .iter()
                    .filter(|patch| app.selected_patches.contains(patch))
                    .count();
                if widgets::nav_item(ui, category.title, count, app.active_category == index)
                    .clicked()
                {
                    app.active_category = index;
                }
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                let selected = app.selected_patches.len();
                let noun = if selected == 1 { "patch" } else { "patches" };
                ui.label(theme::caption_text(&format!("{selected} {noun} selected")));
            });
        });
}

fn draw_category(app: &mut GuiApp, ui: &mut egui::Ui) {
    let category = &CATEGORIES[app.active_category.min(CATEGORIES.len() - 1)];
    ui.label(theme::heading_text(category.title));
    ui.label(theme::caption_text(category.description));
    ui.add_space(12.0);
    for &patch in category.patches {
        widgets::card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            draw_patch(app, ui, patch);
        });
        ui.add_space(8.0);
    }
}

fn draw_patch(app: &mut GuiApp, ui: &mut egui::Ui, patch: PatchId) {
    let info = patch_info(patch);
    let mut selected = app.selected_patches.contains(&patch);
    widgets::patch_row(ui, &mut selected, info.title, info.description);
    if selected {
        app.selected_patches.insert(patch);
    } else {
        app.selected_patches.remove(&patch);
        return;
    }

    draw_patch_options(app, ui, patch);
    match patch {
        PatchId::Camera => draw_zoom_row(app, ui),
        PatchId::Minimap => draw_minimap_controls(app, ui),
        PatchId::ColorMods => draw_color_mods_row(app, ui),
        PatchId::Effects => draw_effects_row(app, ui),
        PatchId::Particles if !app.selected_patches.contains(&PatchId::Effects) => {
            draw_effects_row(app, ui)
        }
        _ => {}
    }
}

fn draw_patch_options(app: &mut GuiApp, ui: &mut egui::Ui, patch: PatchId) {
    let options = patch_options(patch);
    if options.is_empty() {
        return;
    }
    ui.add_space(4.0);
    widgets::indented(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            for option in options {
                let mut enabled = app.options.enabled(patch, option.key);
                let label = egui::RichText::new(option.label)
                    .size(12.5)
                    .color(palette::TEXT);
                if ui
                    .checkbox(&mut enabled, label)
                    .on_hover_text(option.description)
                    .changed()
                {
                    app.options.set(patch, option.key, enabled);
                }
            }
        });
        if !app.options.any_enabled(patch) {
            ui.label(
                egui::RichText::new("Turn on at least one option, or uncheck this patch.")
                    .size(11.5)
                    .color(palette::WARNING),
            );
        }
    });
}

fn draw_zoom_row(app: &mut GuiApp, ui: &mut egui::Ui) {
    ui.add_space(4.0);
    widgets::indented(ui, |ui| {
        ui.horizontal(|ui| {
            control_label(ui, "Zoom");
            ui.add(
                egui::Slider::new(&mut app.zoom, 1.2..=2.4)
                    .step_by(0.1)
                    .show_value(false),
            );
            ui.label(
                egui::RichText::new(format!("{:.1}×", app.zoom))
                    .size(12.5)
                    .monospace()
                    .color(palette::ACCENT),
            );
        });
    });
}

fn draw_minimap_controls(app: &mut GuiApp, ui: &mut egui::Ui) {
    ui.add_space(4.0);
    widgets::indented(ui, |ui| {
        ui.horizontal(|ui| {
            control_label(ui, "Reveal");
            ui.add(
                egui::Slider::new(&mut app.minimap.reveal, 0.0..=1.0)
                    .step_by(0.01)
                    .show_value(false),
            )
            .on_hover_text("How strongly unexplored terrain is drawn. 100% looks fully explored.");
            ui.label(
                egui::RichText::new(format!("{:.0}%", app.minimap.reveal * 100.0))
                    .size(12.5)
                    .monospace()
                    .color(palette::ACCENT),
            );
        });
        ui.horizontal(|ui| {
            control_label(ui, "Floor");
            ui.color_edit_button_rgba_unmultiplied(&mut app.minimap.walkable)
                .on_hover_text("Fill color and opacity of walkable ground.");
            ui.add_space(12.0);
            control_label(ui, "Walls");
            ui.color_edit_button_rgba_unmultiplied(&mut app.minimap.edge)
                .on_hover_text("Color and opacity of walls and edges.");
            ui.add_space(12.0);
            if app.minimap != MinimapStyle::DEFAULT && small_action(ui, "Reset") {
                app.minimap = MinimapStyle::DEFAULT;
            }
        });
    });
}

fn control_label(ui: &mut egui::Ui, text: &str) {
    ui.add_sized(
        [44.0, 18.0],
        egui::Label::new(
            egui::RichText::new(text)
                .size(12.0)
                .color(palette::TEXT_FAINT),
        ),
    );
}

fn draw_color_mods_row(app: &mut GuiApp, ui: &mut egui::Ui) {
    ui.add_space(4.0);
    widgets::indented(ui, |ui| {
        ui.horizontal(|ui| {
            if small_action(ui, "Edit colors…") {
                app.show_color_editor = true;
                app.ensure_catalog_loading();
            }
            let enabled = app.color_mods.iter().filter(|entry| entry.enabled).count();
            ui.label(theme::caption_text(&format!(
                "{} colored",
                count_label(enabled, "mod")
            )));
        });
    });
}

fn draw_effects_row(app: &mut GuiApp, ui: &mut egui::Ui) {
    if !app.effects_editor_accessible() {
        return;
    }
    // Load eagerly so the caption can count per skill. The error guard stops a failed
    // load from respawning every frame. Monsters load only when their tab opens.
    if app.effect_catalog.is_none()
        && app.effect_catalog_task.is_none()
        && app.effect_catalog_error.is_none()
    {
        app.ensure_effect_catalog_loading();
    }
    if app.other_catalog.is_none()
        && app.other_catalog_task.is_none()
        && app.other_catalog_error.is_none()
    {
        app.ensure_other_effect_catalog_loading();
    }
    ui.add_space(4.0);
    widgets::indented(ui, |ui| {
        ui.horizontal(|ui| {
            if small_action(ui, "Edit effects…") {
                app.show_effects_editor = true;
                app.ensure_active_effects_tab_loading();
            }
            ui.label(theme::caption_text(&effects_caption(app)));
        });
    });
}

fn effects_caption(app: &GuiApp) -> String {
    let skills = app.kept_original_effect_skill_count();
    let monsters = app.kept_original_monster_count();
    // Others overrides share the skills map but count only in Others.
    let others = app
        .other_catalog
        .is_some()
        .then(|| app.kept_original_other_effect_count());
    if skills == 0 && monsters == 0 && others.unwrap_or(0) == 0 {
        return "every effect reduced".to_string();
    }
    let mut parts = vec![
        count_label(skills, "skill"),
        count_label(monsters, "monster"),
    ];
    if let Some(others) = others {
        parts.push(count_label(others, "other"));
    }
    format!("{} kept original", parts.join(" · "))
}

fn draw_footer(app: &mut GuiApp, ui: &mut egui::Ui) {
    egui::Panel::bottom("footer")
        .frame(
            egui::Frame::new()
                .fill(palette::BG_CARD)
                .stroke(egui::Stroke::new(1.0, palette::BORDER_FAINT))
                .inner_margin(egui::Margin::symmetric(20, 12)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let busy = app.is_busy();
                let patched = app
                    .status
                    .as_ref()
                    .is_some_and(|status| status.patch_state.is_currently_patched());
                let can_restore = app
                    .status
                    .as_ref()
                    .is_some_and(|status| status.patch_state.can_restore());

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let loading = app.catalogs_loading();
                    let other_install = app
                        .status
                        .as_ref()
                        .is_some_and(|status| status.patch_state == PatchState::OtherInstallBackup);
                    let apply = ui
                        .add_enabled(
                            !patched && !other_install && !busy && !loading,
                            widgets::primary_button("Apply"),
                        )
                        .on_disabled_hover_text(if patched {
                            "Restore first to apply a different selection."
                        } else if other_install {
                            "Restore the install that owns the backup first."
                        } else if busy {
                            "Working…"
                        } else {
                            "Loading game data…"
                        });
                    if apply.clicked() {
                        match app.patch_request() {
                            Ok(_) => app.confirm_apply = true,
                            Err(err) => app.set_message(err, MessageKind::Error),
                        }
                    }
                    let restore = ui.add_enabled(
                        can_restore && !busy && !loading,
                        widgets::secondary_button("Restore"),
                    );
                    let restore = if can_restore && loading {
                        restore.on_disabled_hover_text("Loading game data…")
                    } else {
                        restore
                    };
                    if restore.clicked() {
                        app.confirm_restore = true;
                    }

                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        if busy {
                            ui.add(egui::Spinner::new().color(palette::ACCENT));
                        } else {
                            let (rect, _) =
                                ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                            ui.painter().circle_filled(
                                rect.center(),
                                4.0,
                                message_color(app.message_kind),
                            );
                        }
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&app.message)
                                    .size(12.5)
                                    .color(message_color(app.message_kind)),
                            )
                            .truncate(),
                        );
                    });
                });
            });
        });
}

fn message_color(kind: MessageKind) -> egui::Color32 {
    match kind {
        MessageKind::Info => palette::TEXT_MUTED,
        MessageKind::Success => palette::SUCCESS,
        MessageKind::Error => palette::ERROR,
    }
}

fn draw_modals(app: &mut GuiApp, ctx: &egui::Context) {
    if app.confirm_apply {
        let close = confirm_modal(ctx, "confirm_apply", |ui| {
            ui.label(theme::heading_text("Apply patches?"));
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "Path of Exile 2 game files will be modified. A backup is created first.",
                )
                .color(palette::TEXT_MUTED),
            );
            ui.add_space(6.0);
            ui.label(theme::caption_text(&selected_titles(app)));
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
        if close {
            app.confirm_apply = false;
        }
    }

    if app.confirm_restore {
        let backup = app
            .status
            .as_ref()
            .map(|status| display_path(&status.backup_path));
        let close = confirm_modal(ctx, "confirm_restore", |ui| {
            ui.label(theme::heading_text("Restore original files?"));
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Every patched file is restored from the backup.")
                    .color(palette::TEXT_MUTED),
            );
            if let Some(backup) = &backup {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(backup)
                        .size(11.5)
                        .monospace()
                        .color(palette::TEXT_FAINT),
                );
            }
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
        if close {
            app.confirm_restore = false;
        }
    }

    if app.show_color_editor {
        editors::draw_color_editor(app, ctx);
    }

    if app.show_effects_editor {
        editors::draw_effects_editor(app, ctx);
    }

    if app.show_game_running_dialog {
        let close = confirm_modal(ctx, "game_running", |ui| {
            ui.label(
                egui::RichText::new("Path of Exile 2 is running")
                    .size(15.0)
                    .family(theme::family_semibold())
                    .color(palette::WARNING),
            );
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "Close the game completely, not just minimized to the tray, then try again.",
                )
                .color(palette::TEXT_MUTED),
            );
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(widgets::primary_button("OK")).clicked() {
                    app.show_game_running_dialog = false;
                }
            });
        });
        if close {
            app.show_game_running_dialog = false;
        }
    }
}

fn selected_titles(app: &GuiApp) -> String {
    CATEGORIES
        .iter()
        .flat_map(|category| category.patches)
        .filter(|patch| app.selected_patches.contains(patch))
        .map(|patch| patch_info(*patch).title)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Returns true when the modal should close (Esc or backdrop click).
fn confirm_modal(ctx: &egui::Context, id: &str, add_contents: impl FnOnce(&mut egui::Ui)) -> bool {
    egui::Modal::new(egui::Id::unique(id))
        .frame(widgets::card().inner_margin(20))
        .backdrop_color(egui::Color32::from_black_alpha(140))
        .show(ctx, |ui| {
            ui.set_max_width(400.0);
            add_contents(ui);
        })
        .should_close()
}

fn patch_state_pill(state: PatchState) -> (&'static str, egui::Color32) {
    match state {
        PatchState::Clean => ("Not patched", palette::NEUTRAL),
        PatchState::Patched => ("Patched", palette::SUCCESS),
        PatchState::StaleBackup => ("Old backup", palette::WARNING),
        PatchState::PatchedMissingBackup => ("Backup missing", palette::ERROR),
        PatchState::OtherInstallBackup => ("Other install", palette::WARNING),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn count_label_pluralizes() {
        assert_eq!(count_label(0, "skill"), "0 skills");
        assert_eq!(count_label(1, "skill"), "1 skill");
        assert_eq!(count_label(2, "monster"), "2 monsters");
        assert_eq!(count_label(1, "other"), "1 other");
        assert_eq!(count_label(2, "other"), "2 others");
    }

    #[test]
    fn every_patch_appears_in_exactly_one_category() {
        let listed: Vec<PatchId> = CATEGORIES
            .iter()
            .flat_map(|category| category.patches.iter().copied())
            .collect();
        let unique: HashSet<PatchId> = listed.iter().copied().collect();
        assert_eq!(listed.len(), unique.len());
        assert_eq!(
            unique.len(),
            tiny_poe2smoother::patches::all_patches().len()
        );
    }

    /// Particles alone still needs the editor because Full scopes protect particles.
    #[test]
    fn effects_row_stays_reachable_for_particles_only() {
        let mut app = GuiApp::default();
        app.selected_patches.clear();
        assert!(!app.effects_editor_accessible());

        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            draw_effects_row(&mut app, ui);
        })
        .drop_without_applying_deltas();
        assert!(app.effect_catalog_task.is_none());
        assert!(app.other_catalog_task.is_none());

        app.selected_patches.insert(PatchId::Particles);
        assert!(app.effects_editor_accessible());
        ctx.run_ui(egui::RawInput::default(), |ui| {
            draw_patch(&mut app, ui, PatchId::Particles);
        })
        .drop_without_applying_deltas();
        assert!(app.effect_catalog_task.is_some());
        assert!(app.other_catalog_task.is_some());
    }
}
