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

/// One selectable patch row: checkbox, medium-weight name, muted description.
/// The whole row is clickable and highlights on hover.
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
                    ui.label(
                        egui::RichText::new(name)
                            .size(13.5)
                            .family(theme::family_medium())
                            .color(palette::TEXT),
                    );
                    ui.label(
                        egui::RichText::new(description)
                            .size(11.5)
                            .color(palette::TEXT_MUTED),
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

/// Small clickable color square for the color-mods editor; the currently
/// assigned color gets a bright outline.
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
