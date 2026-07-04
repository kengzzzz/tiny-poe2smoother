use eframe::egui;
use eframe::egui::Color32;

pub mod palette {
    use eframe::egui::Color32;

    // Surfaces (elevation ramp)
    pub const BG_APP: Color32 = Color32::from_rgb(11, 14, 20); // #0B0E14
    pub const BG_CARD: Color32 = Color32::from_rgb(20, 26, 37); // #141A25
    pub const BG_ELEVATED: Color32 = Color32::from_rgb(28, 36, 51); // #1C2433
    pub const BG_ACTIVE: Color32 = Color32::from_rgb(36, 46, 64); // #242E40

    // Strokes
    pub const BORDER: Color32 = Color32::from_rgb(42, 52, 71); // #2A3447
    pub const BORDER_FAINT: Color32 = Color32::from_rgb(30, 38, 53); // #1E2635

    // Accent
    pub const ACCENT: Color32 = Color32::from_rgb(20, 184, 166); // #14B8A6
    pub const ACCENT_HOVER: Color32 = Color32::from_rgb(45, 212, 191); // #2DD4BF
    pub const ACCENT_ACTIVE: Color32 = Color32::from_rgb(13, 148, 136); // #0D9488

    // Text
    pub const TEXT: Color32 = Color32::from_rgb(230, 237, 247); // #E6EDF7
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(148, 163, 184); // #94A3B8
    pub const TEXT_FAINT: Color32 = Color32::from_rgb(100, 112, 131); // #647083
    pub const TEXT_ON_ACCENT: Color32 = Color32::from_rgb(6, 24, 22); // #061816

    // Semantic
    pub const SUCCESS: Color32 = Color32::from_rgb(52, 211, 153); // #34D399
    pub const WARNING: Color32 = Color32::from_rgb(250, 190, 88); // #FABE58
    pub const ERROR: Color32 = Color32::from_rgb(244, 113, 116); // #F47174
    pub const NEUTRAL: Color32 = TEXT_MUTED;
}

pub const FAMILY_MEDIUM: &str = "inter-medium";
pub const FAMILY_SEMIBOLD: &str = "inter-semibold";

pub fn family_medium() -> egui::FontFamily {
    egui::FontFamily::Name(FAMILY_MEDIUM.into())
}

pub fn family_semibold() -> egui::FontFamily {
    egui::FontFamily::Name(FAMILY_SEMIBOLD.into())
}

pub fn install_fonts(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions, FontFamily};
    use std::sync::Arc;

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "inter".into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../assets/fonts/Inter-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        FAMILY_MEDIUM.into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../assets/fonts/Inter-Medium.ttf"
        ))),
    );
    fonts.font_data.insert(
        FAMILY_SEMIBOLD.into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../assets/fonts/Inter-SemiBold.ttf"
        ))),
    );

    // Inter leads the proportional family; egui defaults stay as fallbacks
    // (emoji, symbols) and the default Hack monospace is untouched.
    fonts
        .families
        .get_mut(&FontFamily::Proportional)
        .expect("proportional family exists")
        .insert(0, "inter".into());
    fonts.families.insert(
        FontFamily::Name(FAMILY_MEDIUM.into()),
        vec![FAMILY_MEDIUM.into(), "inter".into()],
    );
    fonts.families.insert(
        FontFamily::Name(FAMILY_SEMIBOLD.into()),
        vec![FAMILY_SEMIBOLD.into(), "inter".into()],
    );
    ctx.set_fonts(fonts);
}

pub fn install_style(ctx: &egui::Context) {
    use palette::*;

    let mut visuals = egui::Visuals::dark();

    visuals.panel_fill = BG_APP;
    visuals.window_fill = BG_CARD;
    visuals.faint_bg_color = BG_ELEVATED;
    visuals.extreme_bg_color = Color32::from_rgb(8, 10, 15);
    visuals.code_bg_color = BG_ELEVATED;
    visuals.window_corner_radius = egui::CornerRadius::same(10);
    visuals.window_stroke = egui::Stroke::new(1.0, BORDER);
    visuals.window_shadow = egui::epaint::Shadow {
        offset: [0, 6],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };

    visuals.hyperlink_color = ACCENT;
    visuals.selection = egui::style::Selection {
        bg_fill: ACCENT.gamma_multiply(0.35),
        stroke: egui::Stroke::new(1.0, ACCENT),
    };
    visuals.slider_trailing_fill = true;
    visuals.weak_text_color = Some(TEXT_FAINT);

    visuals.widgets.noninteractive.bg_fill = BG_CARD;
    visuals.widgets.noninteractive.weak_bg_fill = BG_CARD;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, BORDER_FAINT);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(8);

    visuals.widgets.inactive.bg_fill = BG_ELEVATED;
    visuals.widgets.inactive.weak_bg_fill = BG_ELEVATED;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(8);

    visuals.widgets.hovered.bg_fill = BG_ACTIVE;
    visuals.widgets.hovered.weak_bg_fill = BG_ACTIVE;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.5, TEXT);
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(8);
    visuals.widgets.hovered.expansion = 1.0;

    visuals.widgets.active.bg_fill = ACCENT_ACTIVE;
    visuals.widgets.active.weak_bg_fill = ACCENT_ACTIVE;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.5, Color32::WHITE);
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(8);

    visuals.widgets.open.bg_fill = BG_ELEVATED;
    visuals.widgets.open.weak_bg_fill = BG_ELEVATED;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, BORDER);
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, TEXT);
    visuals.widgets.open.corner_radius = egui::CornerRadius::same(8);

    ctx.set_visuals(visuals);

    let mut style = (*ctx.global_style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(14.0, 7.0);
    style.spacing.icon_width = 16.0;
    style.spacing.icon_width_inner = 9.0;
    style.spacing.slider_width = 160.0;

    use egui::{FontFamily, FontId, TextStyle};
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::new(15.0, family_semibold()));
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(13.5, FontFamily::Proportional));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::new(13.5, family_medium()));
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(11.5, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(12.5, FontFamily::Monospace),
    );
    ctx.set_global_style(style);
}

pub fn title_text(text: &str) -> egui::RichText {
    egui::RichText::new(text)
        .size(22.0)
        .family(family_semibold())
        .color(palette::TEXT)
}

pub fn heading_text(text: &str) -> egui::RichText {
    egui::RichText::new(text)
        .size(15.0)
        .family(family_semibold())
        .color(palette::TEXT)
}

pub fn caption_text(text: &str) -> egui::RichText {
    egui::RichText::new(text)
        .size(11.5)
        .color(palette::TEXT_MUTED)
}
