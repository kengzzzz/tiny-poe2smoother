use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// The shader works in linear HDR. The shipped edge color is overbright, which is
/// what makes walls stand out.
const EDGE_GAIN: f32 = 12.0;

/// The minimap shaders double visibility, so a floor of 0.5 already looks fully explored.
const VISIBILITY_GAIN: f32 = 2.0;

/// Near full reveal, rounding makes the border glow flicker.
const EDGE_GLOW_CUTOFF: f32 = 0.95;

const FULLY_REVEALED: &str = "res_color = float4(1.0f, 0.0f, 0.0f, 1.0f);";

static FLOOR_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[ \t]*res_color = max\(res_color, [0-9.]+f\);").unwrap());
static WALKABLE_COLOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"float4 walkable_color = float4\([^;]*\);").unwrap());
static EDGE_LERP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"float4 walkability_map_color = lerp\(walkable_color, float4\([^)]*\), walkable_to_edge_ratio\);",
    )
    .unwrap()
});
static UNEXPLORED_EDGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"float unexplored_edge = (?:0\.0f|saturate\([^\r\n]* \* 4\.0f \* saturate\(2\.0f - 2\.0f \* walkable_dist\)\));",
    )
    .unwrap()
});

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MinimapStyle {
    pub reveal: f32,
    pub walkable: [f32; 4],
    pub edge: [f32; 4],
}

impl MinimapStyle {
    pub const DEFAULT: Self = Self {
        reveal: 0.36,
        walkable: [0.0, 0.0, 0.0, 0.3],
        edge: [1.0, 1.0, 1.0, 0.1],
    };

    pub fn clamped(self) -> Self {
        let clamp = |color: [f32; 4]| color.map(|channel| channel.clamp(0.0, 1.0));
        Self {
            reveal: self.reveal.clamp(0.0, 1.0),
            walkable: clamp(self.walkable),
            edge: clamp(self.edge),
        }
    }
}

impl Default for MinimapStyle {
    fn default() -> Self {
        Self::DEFAULT
    }
}

pub(super) fn reveal_floor(text: &str, style: &MinimapStyle) -> String {
    let line = format!(
        "\tres_color = max(res_color, {}f);",
        hlsl_float(style.reveal / VISIBILITY_GAIN)
    );
    if let Some(existing) = FLOOR_LINE.find(text) {
        return format!(
            "{}{line}{}",
            &text[..existing.start()],
            &text[existing.end()..]
        );
    }
    let mut lines: Vec<&str> = text.split("\r\n").collect();
    if let Some(index) = lines.iter().position(|l| l.contains(FULLY_REVEALED)) {
        lines.insert(index + 1, &line);
        return lines.join("\r\n");
    }
    text.to_string()
}

pub(super) fn recolor(text: &str, style: &MinimapStyle) -> String {
    let [r, g, b, a] = style.walkable;
    let walkable = format!("float4 walkable_color = {};", hlsl_float4([r, g, b, a]));
    let [r, g, b, a] = style.edge;
    let edge = format!(
        "float4 walkability_map_color = lerp(walkable_color, {}, walkable_to_edge_ratio);",
        hlsl_float4([r * EDGE_GAIN, g * EDGE_GAIN, b * EDGE_GAIN, a])
    );
    let glow = unexplored_edge(style.reveal);
    let text = WALKABLE_COLOR.replace(text, regex::NoExpand(&walkable));
    let text = EDGE_LERP.replace(&text, regex::NoExpand(&edge));
    UNEXPLORED_EDGE
        .replace(&text, regex::NoExpand(&glow))
        .into_owned()
}

/// Without this, Reveal near 50% turns all unexplored terrain blue.
fn unexplored_edge(reveal: f32) -> String {
    let Some(scale) = edge_scale(reveal) else {
        return "float unexplored_edge = 0.0f;".to_string();
    };
    let edge = format!(
        "saturate((visibility - {}f) * {}f)",
        hlsl_float(reveal),
        hlsl_float(scale)
    );
    format!(
        "float unexplored_edge = saturate({edge} * (1.0f - {edge}) * 4.0f * saturate(2.0f - 2.0f * walkable_dist));"
    )
}

fn edge_scale(reveal: f32) -> Option<f32> {
    (reveal < EDGE_GLOW_CUTOFF).then(|| 1.0 / (1.0 - reveal))
}

fn hlsl_float4(values: [f32; 4]) -> String {
    let [r, g, b, a] = values.map(hlsl_float);
    format!("float4({r}f, {g}f, {b}f, {a}f)")
}

/// Always keeps a decimal point because HLSL rejects `1f`.
fn hlsl_float(value: f32) -> String {
    let text = format!("{value:.4}");
    let trimmed = text.trim_end_matches('0');
    if trimmed.ends_with('.') {
        format!("{trimmed}0")
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VISIBILITY: &str = "void main()\r\n{\r\n\tif (revealed)\r\n\t\tres_color = float4(1.0f, 0.0f, 0.0f, 1.0f);\r\n\treturn res_color;\r\n}";
    const BLENDING: &str = "\tfloat4 walkable_color = float4(1.0f, 1.0f, 1.0f, 0.01f);\r\n\tfloat4 walkability_map_color = lerp(walkable_color, float4(0.5f, 0.5f, 1.0f, 0.5f), walkable_to_edge_ratio);\r\n\tfloat unexplored_edge = saturate(visibility * (1.0f - visibility) * 4.0f * saturate(2.0f - 2.0f * walkable_dist));\r\n";

    fn shader_glow(reveal: f32, visibility: f32) -> f32 {
        let Some(scale) = edge_scale(reveal) else {
            return 0.0;
        };
        let offset: f32 = hlsl_float(reveal).parse().unwrap();
        let scale: f32 = hlsl_float(scale).parse().unwrap();
        let edge = ((visibility - offset) * scale).clamp(0.0, 1.0);
        (edge * (1.0 - edge) * 4.0).clamp(0.0, 1.0).powf(25.0)
    }

    #[test]
    fn default_style_reproduces_the_fixed_patch_lines() {
        let style = MinimapStyle::default();
        assert_eq!(
            reveal_floor(VISIBILITY, &style),
            "void main()\r\n{\r\n\tif (revealed)\r\n\t\tres_color = float4(1.0f, 0.0f, 0.0f, 1.0f);\r\n\tres_color = max(res_color, 0.18f);\r\n\treturn res_color;\r\n}"
        );
        assert_eq!(
            recolor(BLENDING, &style),
            "\tfloat4 walkable_color = float4(0.0f, 0.0f, 0.0f, 0.3f);\r\n\tfloat4 walkability_map_color = lerp(walkable_color, float4(12.0f, 12.0f, 12.0f, 0.1f), walkable_to_edge_ratio);\r\n\tfloat unexplored_edge = saturate(saturate((visibility - 0.36f) * 1.5625f) * (1.0f - saturate((visibility - 0.36f) * 1.5625f)) * 4.0f * saturate(2.0f - 2.0f * walkable_dist));\r\n"
        );
    }

    #[test]
    fn reapplying_replaces_instead_of_stacking() {
        let first = reveal_floor(
            VISIBILITY,
            &MinimapStyle {
                reveal: 0.5,
                ..MinimapStyle::DEFAULT
            },
        );
        assert!(first.contains("max(res_color, 0.25f)"));
        let second = reveal_floor(&first, &MinimapStyle::DEFAULT);
        assert_eq!(second.matches("max(res_color").count(), 1);
        assert!(second.contains("max(res_color, 0.18f)"));

        let custom = MinimapStyle {
            walkable: [0.25, 0.5, 1.0, 0.75],
            edge: [0.5, 0.0, 1.0, 1.0],
            ..MinimapStyle::DEFAULT
        };
        let recolored = recolor(&recolor(BLENDING, &MinimapStyle::DEFAULT), &custom);
        assert!(recolored.contains("float4 walkable_color = float4(0.25f, 0.5f, 1.0f, 0.75f);"));
        assert!(recolored.contains(
            "lerp(walkable_color, float4(6.0f, 0.0f, 12.0f, 1.0f), walkable_to_edge_ratio)"
        ));
        assert_eq!(recolored.matches("float unexplored_edge").count(), 1);
        assert!(recolored.contains("(visibility - 0.36f) * 1.5625f"));

        let full = MinimapStyle {
            reveal: 1.0,
            ..MinimapStyle::DEFAULT
        };
        let no_glow = recolor(&recolored, &full);
        assert!(no_glow.contains("\tfloat unexplored_edge = 0.0f;\r\n"));
        let back = recolor(&no_glow, &MinimapStyle::DEFAULT);
        assert_eq!(back, recolor(BLENDING, &MinimapStyle::DEFAULT));
    }

    #[test]
    fn border_glow_stays_off_unexplored_and_explored_terrain() {
        for step in 0..=100 {
            let reveal = step as f32 / 100.0;
            let stored: f32 = hlsl_float(reveal / VISIBILITY_GAIN).parse().unwrap();
            let floor = stored * VISIBILITY_GAIN;
            for visibility in [floor - 1.0 / 255.0, floor, floor + 1.0 / 255.0] {
                let glow = shader_glow(reveal, visibility.clamp(0.0, 1.0));
                assert!(
                    glow < 0.01,
                    "reveal {reveal} visibility {visibility} glow {glow}"
                );
            }
            let explored = shader_glow(reveal, 1.0);
            assert!(explored < 1e-6, "reveal {reveal} explored glow {explored}");
            if reveal < EDGE_GLOW_CUTOFF {
                let border = shader_glow(reveal, f32::midpoint(1.0, reveal));
                assert!(border > 0.99, "reveal {reveal} border glow {border}");
            }
        }
    }

    #[test]
    fn full_reveal_writes_the_floor_the_game_shows_as_explored() {
        let style = MinimapStyle {
            reveal: 1.0,
            ..MinimapStyle::DEFAULT
        };
        assert!(reveal_floor(VISIBILITY, &style).contains("max(res_color, 0.5f)"));
    }

    #[test]
    fn missing_anchor_leaves_text_unchanged() {
        let text = "float4 main() { return 0; }";
        assert_eq!(reveal_floor(text, &MinimapStyle::DEFAULT), text);
        assert_eq!(recolor(text, &MinimapStyle::DEFAULT), text);
    }

    #[test]
    fn hlsl_floats_always_carry_a_decimal_point() {
        assert_eq!(hlsl_float(1.0), "1.0");
        assert_eq!(hlsl_float(0.0), "0.0");
        assert_eq!(hlsl_float(0.1 + 0.17), "0.27");
        assert_eq!(hlsl_float(12.0), "12.0");
    }

    #[test]
    fn clamped_limits_every_channel() {
        let style = MinimapStyle {
            reveal: 2.0,
            walkable: [-1.0, 0.5, 3.0, 1.0],
            edge: [1.0; 4],
        }
        .clamped();
        assert_eq!(style.reveal, 1.0);
        assert_eq!(style.walkable, [0.0, 0.5, 1.0, 1.0]);
    }
}
