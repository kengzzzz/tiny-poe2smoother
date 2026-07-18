use eframe::egui;

const SIZE: usize = 64;
const ICON_RGBA: &[u8; SIZE * SIZE * 4] = include_bytes!("../../assets/icons/app-icon-64.rgba");
const TEXTURE_ID: &str = "tiny-poe2smoother.app-icon";

pub fn app_icon() -> egui::IconData {
    egui::IconData {
        rgba: ICON_RGBA.to_vec(),
        width: SIZE as u32,
        height: SIZE as u32,
    }
}

fn color_image() -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied([SIZE, SIZE], ICON_RGBA)
}

fn texture(ctx: &egui::Context) -> egui::TextureHandle {
    let id = egui::Id::new(TEXTURE_ID);
    if let Some(texture) = ctx.data(|data| data.get_temp::<egui::TextureHandle>(id)) {
        return texture;
    }

    let texture = ctx.load_texture(TEXTURE_ID, color_image(), egui::TextureOptions::LINEAR);
    ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
    texture
}

/// Draws the same embedded asset as [`app_icon`], so the header logo and the
/// window icon stay consistent.
pub fn paint_logo_mark(painter: &egui::Painter, rect: egui::Rect) {
    let texture = texture(painter.ctx());
    painter.image(
        texture.id(),
        rect,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_expected_dimensions_and_transparent_corners() {
        let icon = app_icon();
        assert_eq!(icon.width, 64);
        assert_eq!(icon.height, 64);
        assert_eq!(icon.rgba.len(), 64 * 64 * 4);

        for (x, y) in [(0u32, 0u32), (63, 0), (0, 63), (63, 63)] {
            let alpha = icon.rgba[((y * 64 + x) * 4 + 3) as usize];
            assert_eq!(alpha, 0, "corner ({x},{y}) should be transparent");
        }

        let center_alpha = icon.rgba[((32 * 64 + 32) * 4 + 3) as usize];
        assert_eq!(center_alpha, 255);
    }
}
