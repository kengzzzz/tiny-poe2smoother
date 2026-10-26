use eframe::egui;

use super::theme::{self, palette};

pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(palette::BG_CARD)
        .stroke(egui::Stroke::new(1.0, palette::BORDER))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(16, 14))
}

pub fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text)
            .size(13.5)
            .family(theme::family_semibold())
            .color(palette::TEXT_ON_ACCENT),
    )
    .fill(palette::ACCENT)
    .corner_radius(8)
    .min_size(egui::vec2(110.0, 34.0))
}

pub fn secondary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text)
            .size(13.5)
            .family(theme::family_medium())
            .color(palette::TEXT),
    )
    .fill(palette::BG_ELEVATED)
    .stroke(egui::Stroke::new(1.0, palette::BORDER))
    .corner_radius(8)
    .min_size(egui::vec2(110.0, 34.0))
}

pub fn danger_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text)
            .size(13.5)
            .family(theme::family_semibold())
            .color(palette::ERROR),
    )
    .fill(tinted(palette::ERROR, 30))
    .stroke(egui::Stroke::new(1.0, tinted(palette::ERROR, 110)))
    .corner_radius(8)
    .min_size(egui::vec2(110.0, 34.0))
}

pub fn chip(text: &str, active: bool) -> egui::Button<'static> {
    let (fill, stroke_color, text_color) = if active {
        (
            tinted(palette::ACCENT, 26),
            palette::ACCENT,
            palette::ACCENT_HOVER,
        )
    } else {
        (palette::BG_ELEVATED, palette::BORDER, palette::TEXT_MUTED)
    };
    egui::Button::new(egui::RichText::new(text).size(11.5).color(text_color))
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, stroke_color))
        .corner_radius(12)
}

pub fn status_pill(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(tinted(color, 26))
        .stroke(egui::Stroke::new(1.0, tinted(color, 100)))
        .corner_radius(12)
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 3.0, color);
                ui.label(egui::RichText::new(text).size(11.5).color(color));
            });
        });
}

pub fn patch_row(
    ui: &mut egui::Ui,
    selected: &mut bool,
    name: &str,
    description: &str,
) -> egui::Response {
    let inner = ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt(name)
            .sense(egui::Sense::click()),
        |ui| {
            ui.set_min_width(ui.available_width());
            let row_response = ui.response();
            if row_response.contains_pointer() {
                ui.painter().rect_filled(
                    row_response.rect.expand2(egui::vec2(6.0, 3.0)),
                    6.0,
                    palette::BG_ELEVATED,
                );
            }
            ui.horizontal(|ui| {
                let checkbox_changed = ui.checkbox(selected, "").changed();
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    // Selectable labels would swallow clicks meant for the row.
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(name)
                                .size(13.5)
                                .family(theme::family_medium())
                                .color(palette::TEXT),
                        )
                        .selectable(false),
                    );
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(description)
                                .size(11.5)
                                .color(palette::TEXT_MUTED),
                        )
                        .selectable(false),
                    );
                });
                checkbox_changed
            })
            .inner
        },
    );
    let checkbox_changed = inner.inner;
    let response = inner.response;
    if response.clicked() && !checkbox_changed {
        *selected = !*selected;
    }
    response
}

pub fn color_swatch(ui: &mut egui::Ui, color: egui::Color32, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect.shrink(1.0), 4.0, color);
    if selected {
        painter.rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.5, palette::TEXT),
            egui::StrokeKind::Outside,
        );
    } else if response.hovered() {
        painter.rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.0, palette::TEXT_FAINT),
            egui::StrokeKind::Outside,
        );
    }
    response
}

pub fn tinted(color: egui::Color32, alpha: u8) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

pub fn small_action(ui: &mut egui::Ui, text: &str) -> bool {
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

pub fn count_label(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

pub fn nav_item(ui: &mut egui::Ui, title: &str, count: usize, active: bool) -> egui::Response {
    let height = 34.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    let painter = ui.painter();
    if active {
        painter.rect_filled(rect, 8.0, palette::BG_ELEVATED);
        let bar = egui::Rect::from_min_size(
            rect.left_top() + egui::vec2(0.0, 8.0),
            egui::vec2(3.0, height - 16.0),
        );
        painter.rect_filled(bar, 2.0, palette::ACCENT);
    } else if response.hovered() {
        painter.rect_filled(rect, 8.0, tinted(palette::BG_ELEVATED, 140));
    }
    let text_color = if active {
        palette::TEXT
    } else {
        palette::TEXT_MUTED
    };
    painter.text(
        rect.left_center() + egui::vec2(14.0, 0.0),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::new(13.5, theme::family_medium()),
        text_color,
    );
    if count > 0 {
        let badge_text = count.to_string();
        let galley = painter.layout_no_wrap(
            badge_text,
            egui::FontId::new(11.5, theme::family_semibold()),
            palette::TEXT_ON_ACCENT,
        );
        let badge = egui::Rect::from_center_size(
            rect.right_center() - egui::vec2(18.0, 0.0),
            egui::vec2((galley.size().x + 12.0).max(20.0), 18.0),
        );
        painter.rect_filled(badge, 9.0, palette::ACCENT);
        painter.galley(
            badge.center() - galley.size() / 2.0,
            galley,
            palette::TEXT_ON_ACCENT,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn segment(ui: &mut egui::Ui, text: &str, active: bool, enabled: bool) -> egui::Response {
    let (fill, stroke, color) = if active {
        (
            tinted(palette::ACCENT, 34),
            palette::ACCENT,
            palette::ACCENT_HOVER,
        )
    } else {
        (palette::BG_ELEVATED, palette::BORDER, palette::TEXT_MUTED)
    };
    let button = egui::Button::new(
        egui::RichText::new(text)
            .size(12.5)
            .family(theme::family_medium())
            .color(color),
    )
    .fill(fill)
    .stroke(egui::Stroke::new(1.0, stroke))
    .corner_radius(8)
    .min_size(egui::vec2(96.0, 30.0));
    if enabled {
        ui.add(button)
            .on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        ui.add(button.sense(egui::Sense::hover()))
    }
}

pub fn indented<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.add_space(28.0);
        ui.vertical(add_contents).inner
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicking_the_row_title_toggles_it() {
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        let mut selected = false;
        let frame = |events: Vec<egui::Event>, selected: &mut bool| {
            let input = egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 200.0),
                )),
                ..Default::default()
            };
            let mut rect = egui::Rect::NOTHING;
            ctx.run_ui(input, |ui| {
                rect = patch_row(ui, selected, "Camera", "Set a farther default zoom.").rect;
            })
            .drop_without_applying_deltas();
            rect
        };

        let rect = frame(Vec::new(), &mut selected);
        let pos = rect.left_top() + egui::vec2(45.0, 8.0);
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![egui::Event::PointerMoved(pos)], &mut selected);
        frame(vec![button(true)], &mut selected);
        frame(vec![button(false)], &mut selected);
        assert!(selected);
    }
}
