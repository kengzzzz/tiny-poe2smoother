use super::catalog::{PatchId, PatchOptions};
use super::client_strings::compact_mod_lines;
use super::color_mods::ColorMatcher;
use super::effect_skills::{EffectLevel, EffectsFilter};
use super::minimap::{MinimapStyle, recolor, reveal_floor};
use super::targeting::{
    EFFECT_PROTECTED_PREFIXES, PARTICLE_PROTECTED_PREFIXES, ends_with_path_ci,
    is_startup_scene_protected, normalize_path, patch_applies_path, patch_targets_path,
};
use super::text::{
    decode_utf16, decode_utf16_lossless, empty_named_blocks, encode_utf16_bom, find_matching_brace,
    remove_function_calls, replace_array_property, replace_utf16, strip_client_blocks,
};
use anyhow::{Result, bail};
use regex::Regex;
use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::LazyLock;

static RAIN_INTENSITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"("rain_intensity":\s*)([^,\r\n}]+)(,?)"#).unwrap());
static CLOUDS_INTENSITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"("clouds_intensity":\s*)([^,\r\n}]+)(,?)"#).unwrap());
// Longest types first so float4x4 never half-matches float. Parameters allow
// one nested paren level for macro params.
static HLSL_FUNCTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(float4x4|float3x3|float2x2|float4|float3|float2|float|void|OutPixel)\s+\w+\s*\((?:[^()]|\([^()]*\))*\)",
    )
    .unwrap()
});
static EFFECT_KEEP_BLOCKS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "ClientAnimationController",
        "SoundEvents",
        "BoneGroups",
        "AnimatedRender",
        "SkinMesh",
    ]
    .into_iter()
    .collect()
});
// Only these block bodies are emptied. This reproduces the captured output byte for byte.
static SOUND_EMPTY_BLOCKS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["SoundEvents", "SoundParams"].into_iter().collect());

/// Misspelling a layer name disables it. The client ignores layers it doesn't recognise.
const ATMOSPHERE_LAYERS: &[(&str, &str)] = &[
    ("area", "area"),
    ("fog", "fog"),
    ("screenspace_fog", "screenspace_fog"),
    ("effect_spawner", "effect_spawner"),
    ("bloom", "post_processing"),
];
const LIGHT_LAYERS: &[&str] = &[
    "directional_light",
    "player_light",
    "environment_mapping",
    "global_illumination",
];
const POST_PROCESS_SWITCHES: &[(&str, &str)] = &[
    ("vignette", "vignette_enable"),
    ("depth_of_field", "dof_enable"),
    ("desaturation", "desaturation_enable"),
];
const TOWN_PORTALS_PREFIX: &str = "metadata/effects/microtransactions/town_portals/";
static DEFAULT_OPTIONS: PatchOptions = PatchOptions::new();

#[derive(Clone, Copy)]
pub(super) struct TransformCtx<'a> {
    pub zoom: f64,
    pub minimap: &'a MinimapStyle,
    pub options: &'a PatchOptions,
    pub color: Option<&'a ColorMatcher>,
    /// `None` means every folder is Reduced.
    pub effects: Option<&'a EffectsFilter>,
}

impl Default for TransformCtx<'_> {
    fn default() -> Self {
        Self {
            zoom: 2.4,
            minimap: &MinimapStyle::DEFAULT,
            options: &DEFAULT_OPTIONS,
            color: None,
            effects: None,
        }
    }
}

/// Transformed bytes for one target, or `None` when `patch` doesn't target `path`.
#[doc(hidden)]
pub fn audit_transform(patch: PatchId, path: &str, bytes: &[u8]) -> Option<Result<Vec<u8>>> {
    if !patch_targets_path(patch, path) || !patch_applies_path(patch, path) {
        return None;
    }
    Some(transform(patch, path, bytes, TransformCtx::default()))
}

pub(super) fn transform(
    patch: PatchId,
    path: &str,
    bytes: &[u8],
    ctx: TransformCtx<'_>,
) -> Result<Vec<u8>> {
    match patch {
        PatchId::Camera => camera(path, bytes, ctx.zoom),
        PatchId::Minimap => minimap(path, bytes, ctx.minimap),
        PatchId::AtlasFog => atlas_fog(bytes),
        PatchId::ColorMods => match ctx.color {
            Some(matcher) => matcher.colorize(bytes),
            None => Ok(bytes.to_vec()),
        },
        PatchId::Atmosphere => atmosphere(bytes, ctx.options),
        PatchId::Shadow => replace_utf16(
            bytes,
            &[("\"shadows_enabled\": true", "\"shadows_enabled\": false")],
        ),
        PatchId::Light => light(bytes, ctx.options),
        PatchId::PostProcess => post_process(bytes, ctx.options),
        PatchId::Delirium => delirium(bytes),
        PatchId::Particles => {
            if ctx
                .effects
                .is_some_and(|filter| filter.level_for(path) == EffectLevel::Full)
            {
                Ok(bytes.to_vec())
            } else {
                particles(path, bytes)
            }
        }
        PatchId::Effects => effects(path, bytes, ctx.effects),
        PatchId::DisableSounds => strip_sounds(path, bytes),
        PatchId::SkillSounds => strip_sounds(path, bytes),
        PatchId::MonsterSounds => strip_sounds(path, bytes),
        PatchId::MtxSoft => mtx_soft(path, bytes, ctx.options),
        PatchId::CompactModLines => compact_mod_lines(bytes, ctx.options),
        PatchId::MonsterHpBar => monster_hp_bar(bytes),
        PatchId::BlackScreen => black_screen(bytes),
    }
}

fn camera(path: &str, bytes: &[u8], zoom: f64) -> Result<Vec<u8>> {
    let mut text = decode_utf16(bytes)?;
    if path.eq_ignore_ascii_case("metadata/characters/character.ot") {
        let zoom = format!("{zoom:.1}");
        let mut lines: Vec<String> = text.split("\r\n").map(str::to_string).collect();
        if let Some(index) = lines
            .iter()
            .position(|line| line.contains("CreateCameraZoomNode"))
        {
            lines[index] = format!(
                "\ton_initial_position_set = {{CreateCameraZoomNode(5000.0, 5000.0, {zoom});}} "
            );
        } else if let Some(index) = lines.iter().position(|line| line.contains("team = 1")) {
            lines.insert(
                index + 1,
                format!(
                    "\ton_initial_position_set = {{CreateCameraZoomNode(5000.0, 5000.0, {zoom});}} "
                ),
            );
        }
        text = lines.join("\r\n");
        return Ok(encode_utf16_bom(&text));
    }

    let functions = [
        "CreateCameraZoomNode",
        "ClearCameraZoomNodes",
        "CreateCameraLookAtNode",
        "CreateCameraPanNode",
        "ClearCameraPanNode",
        "ClearCameraPanNodes",
        "SetCustomCameraSpeed",
        "RemoveCustomCameraSpeed",
        "FaceCamera",
    ];
    if !functions.iter().any(|func| text.contains(func)) {
        return Ok(bytes.to_vec());
    }
    for func in functions {
        text = remove_function_calls(&text, func);
    }
    Ok(encode_utf16_bom(&text))
}

fn minimap(path: &str, bytes: &[u8], style: &MinimapStyle) -> Result<Vec<u8>> {
    let text = String::from_utf8_lossy(bytes);
    let style = style.clamped();
    let patched = if ends_with_path_ci(path, "minimap_visibility_pixel.hlsl") {
        reveal_floor(&text, &style)
    } else if ends_with_path_ci(path, "minimap_blending_pixel.hlsl") {
        recolor(&text, &style)
    } else {
        text.into_owned()
    };
    Ok(patched.into_bytes())
}

fn atlas_fog(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut text = decode_utf16(bytes)?;
    text = replace_array_property(text, "nodes");
    text = replace_array_property(text, "links");
    Ok(encode_utf16_bom(&text))
}

fn atmosphere(bytes: &[u8], options: &PatchOptions) -> Result<Vec<u8>> {
    let mut text = decode_utf16(bytes)?;
    for (key, layer) in ATMOSPHERE_LAYERS {
        if options.enabled(PatchId::Atmosphere, key) {
            text = disable_layer(&text, layer);
        }
    }
    for (key, regex) in [
        ("rain", &RAIN_INTENSITY_RE),
        ("clouds", &CLOUDS_INTENSITY_RE),
    ] {
        if options.enabled(PatchId::Atmosphere, key) {
            text = regex.replace_all(&text, "${1}0.0${3}").into_owned();
        }
    }
    Ok(encode_utf16_bom(&text))
}

fn light(bytes: &[u8], options: &PatchOptions) -> Result<Vec<u8>> {
    let mut text = decode_utf16(bytes)?;
    for layer in LIGHT_LAYERS {
        if options.enabled(PatchId::Light, layer) {
            text = disable_layer(&text, layer);
        }
    }
    Ok(encode_utf16_bom(&text))
}

fn disable_layer(text: &str, layer: &str) -> String {
    text.replace(&format!("\"{layer}\""), &format!("\"x{}\"", &layer[1..]))
}

fn post_process(bytes: &[u8], options: &PatchOptions) -> Result<Vec<u8>> {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    for (key, flag) in POST_PROCESS_SWITCHES {
        if !options.enabled(PatchId::PostProcess, key) {
            continue;
        }
        let check = Regex::new(&format!(r"if\s*\(\s*{flag}\s*\)")).expect("valid regex");
        if let Cow::Owned(replaced) = check.replace_all(&text, "if( 0 )") {
            text = replaced;
            continue;
        }
        // BlackScreen stubs leave the flag only in its declaration. Anything else means
        // the shader changed.
        let declaration =
            Regex::new(&format!(r"\b(?:uint|bool)\s+{flag}\s*;")).expect("valid regex");
        if !(declaration.is_match(&text) && text.matches(flag).count() == 1) {
            bail!(
                "postprocessuber.hlsl no longer has the expected {flag} switch.\n\
                 Verify game files or wait for a tiny-poe2smoother update"
            );
        }
    }
    Ok(text.into_bytes())
}

fn delirium(bytes: &[u8]) -> Result<Vec<u8>> {
    let text = decode_utf16(bytes)?;
    let out = if text.contains("Metadata/FmtParent") && !text.contains("AnimatedRender") {
        "version 3\nextends \"Metadata/FmtParent\"".to_string()
    } else if text.contains("Metadata/FmtParent") && text.contains("AnimatedRender") {
        "version 3\nextends \"Metadata/FmtParent\"\n\nclient\n{\n\tAnimatedRender\n\t{\n\t\tcannot_be_disabled = true\n\t}\n}".to_string()
    } else if text.contains("Metadata/Parent") {
        "version 3\nextends \"Metadata/Parent\"\n\nBaseAnimationEvents\n{\n}\n\nAnimationController\n{\n\tmetadata = \"Art/Models/Effects/enviro_effects/weather_attachments/generic_rig/weather_rig.amd\"\n}\n\nclient\n{\n    ClientAnimationController\n    {\n        skeleton = \"Art/Models/Effects/enviro_effects/weather_attachments/generic_rig/weather_rig.ast\"\n    }\n\n    BoneGroups\n    {\n        bone_group = \"box false aux_box1 aux_box2 aux_box3 \"\n    }\n}".to_string()
    } else {
        text
    };
    Ok(encode_utf16_bom(&out))
}

/// `.epk` must be empty because a bare "0" fails with "Unexpected token 0".
/// `.pet` and `.trl` take BOM + "0".
fn blank_effect_data(path: &str) -> Option<Vec<u8>> {
    if ends_with_path_ci(path, ".epk") {
        return Some(encode_utf16_bom(""));
    }
    if ends_with_path_ci(path, ".pet") || ends_with_path_ci(path, ".trl") {
        return Some(encode_utf16_bom("0"));
    }
    None
}

fn particles(path: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    let normalized = normalize_path(path);
    if PARTICLE_PROTECTED_PREFIXES
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
    {
        return Ok(bytes.to_vec());
    }
    Ok(encode_utf16_bom("0"))
}

fn effects(path: &str, bytes: &[u8], filter: Option<&EffectsFilter>) -> Result<Vec<u8>> {
    // Full wins even for candidates collected by other patches, like SkillSounds.
    let level = filter.map_or(EffectLevel::Reduced, |filter| filter.level_for(path));
    if level == EffectLevel::Full {
        return Ok(bytes.to_vec());
    }
    let normalized = normalize_path(path);
    if is_startup_scene_protected(path) {
        return Ok(bytes.to_vec());
    }
    if EFFECT_PROTECTED_PREFIXES
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
    {
        return Ok(bytes.to_vec());
    }
    if !(ends_with_path_ci(path, ".ao") || ends_with_path_ci(path, ".aoc")) {
        return Ok(bytes.to_vec());
    }
    let text = decode_utf16(bytes)?;
    Ok(encode_utf16_bom(&strip_client_blocks(
        &text,
        &EFFECT_KEEP_BLOCKS,
    )))
}

fn strip_sounds(path: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    let normalized = normalize_path(path);
    if is_startup_scene_protected(path) {
        return Ok(bytes.to_vec());
    }
    if EFFECT_PROTECTED_PREFIXES
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
    {
        return Ok(bytes.to_vec());
    }
    let Some(text) = decode_utf16_lossless(bytes)? else {
        return Ok(bytes.to_vec());
    };
    Ok(encode_utf16_bom(&empty_named_blocks(
        &text,
        &SOUND_EMPTY_BLOCKS,
    )))
}

fn mtx_soft(path: &str, bytes: &[u8], options: &PatchOptions) -> Result<Vec<u8>> {
    if is_startup_scene_protected(path) {
        return Ok(bytes.to_vec());
    }
    if !options.enabled(PatchId::MtxSoft, "town_portals")
        && normalize_path(path).starts_with(TOWN_PORTALS_PREFIX)
    {
        return Ok(bytes.to_vec());
    }
    // The captured soft-mtx output never edits .ao or .aoc files.
    if let Some(blanked) = blank_effect_data(path) {
        return Ok(blanked);
    }
    Ok(bytes.to_vec())
}

/// One base life and ES on the shared monster template keeps HP bars always visible.
fn monster_hp_bar(bytes: &[u8]) -> Result<Vec<u8>> {
    let text = decode_utf16(bytes)?;
    if text.contains("base_maximum_energy_shield = 1") || text.contains("base_maximum_life = 1") {
        return Ok(bytes.to_vec());
    }
    let mut lines: Vec<String> = text.split("\r\n").map(str::to_string).collect();
    let insert_at = lines
        .iter()
        .position(|line| line.contains("item_drop_slots = 1"))
        .map(|index| index + 1)
        .or_else(|| stats_block_body_start(&lines));
    let Some(index) = insert_at else {
        return Ok(bytes.to_vec());
    };
    lines.insert(index, "\tbase_maximum_life = 1".to_string());
    lines.insert(index, "\tbase_maximum_energy_shield = 1".to_string());
    Ok(encode_utf16_bom(&lines.join("\r\n")))
}

/// Stubs every function body so lighting and post-processing output nothing.
/// The UI is drawn afterwards and stays visible.
fn black_screen(bytes: &[u8]) -> Result<Vec<u8>> {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while let Some(found) = HLSL_FUNCTION_RE.captures(&text[pos..]) {
        let signature_end = pos + found.get(0).expect("group 0 always exists").end();
        // Forward declarations are copied through instead of matching the next body.
        let body = function_body_open(&text, signature_end)
            .and_then(|open| find_matching_brace(&text, open).map(|close| (open, close)));
        let Some((open, close)) = body else {
            out.push_str(&text[pos..signature_end]);
            pos = signature_end;
            continue;
        };
        out.push_str(&text[pos..open]);
        out.push_str(hlsl_stub(&found[1]));
        pos = close + 1;
    }
    out.push_str(&text[pos..]);
    Ok(out.into_bytes())
}

fn function_body_open(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut semantic_seen = false;
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => return Some(i),
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i += text[i..].find('\n')?;
            }
            b if b.is_ascii_whitespace() => {}
            b':' if !semantic_seen => semantic_seen = true,
            b if semantic_seen && (b.is_ascii_alphanumeric() || b == b'_') => {}
            _ => return None,
        }
        i += 1;
    }
    None
}

fn hlsl_stub(return_type: &str) -> &'static str {
    match return_type {
        "float" => "{\n\treturn (float)0;\n}",
        "float2" => "{\n\treturn (float2)0;\n}",
        "float3" => "{\n\treturn (float3)0;\n}",
        "float4" => "{\n\treturn (float4)0;\n}",
        "float2x2" => "{\n\treturn (float2x2)0;\n}",
        "float3x3" => "{\n\treturn (float3x3)0;\n}",
        "float4x4" => "{\n\treturn (float4x4)0;\n}",
        // screenspaceshadows.hlsl: report "fully lit" instead of computing.
        "OutPixel" => {
            "{\tOutPixel res;\r\n\tres.shadowmap_data = float4(1.0f, 1.0f, 1.0f, 1.0f);\r\n\treturn res;\n}"
        }
        _ => "{ }",
    }
}

fn stats_block_body_start(lines: &[String]) -> Option<usize> {
    let stats = lines.iter().position(|line| line.trim() == "Stats")?;
    let brace = lines[stats + 1..]
        .iter()
        .position(|line| line.trim() == "{")?;
    Some(stats + 1 + brace + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_removes_function_calls() {
        let input = encode_utf16_bom("foo = 1;\ncontroller.CreateCameraPanNode(1, (2));\nbar = 2;");
        let out = String::from_utf16(
            &camera("metadata/x.ot", &input, 2.4)
                .unwrap()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(!out.contains("CreateCameraPanNode"));
        assert!(out.contains("bar = 2"));
    }

    const ENV: &str = r#"{
"fog": { "density": 1.0 },
"screenspace_fog": {},
"area": {},
"effect_spawner": {},
"post_processing": {},
"directional_light": {},
"player_light": {},
"environment_mapping": {},
"global_illumination": {},
"rain_intensity": 1.0,
"clouds_intensity": 0.5,
"shadows_enabled": true
}"#;

    fn legacy_env_particles(text: &str) -> String {
        let mut text = text.to_string();
        for (from, to) in [
            ("\"area\"", "\"xrea\""),
            ("\"fog\"", "\"xog\""),
            ("\"screenspace_fog\"", "\"xcreenspace_fog\""),
            ("\"effect_spawner\"", "\"xffect_spawner\""),
            ("\"post_processing\"", "\"xost_processing\""),
        ] {
            text = text.replace(from, to);
        }
        text = RAIN_INTENSITY_RE
            .replace_all(&text, "${1}0.0${3}")
            .into_owned();
        CLOUDS_INTENSITY_RE
            .replace_all(&text, "${1}0.0${3}")
            .into_owned()
    }

    fn atmosphere_with(layers: &[&str]) -> String {
        let mut options = PatchOptions::new();
        for option in super::super::catalog::patch_options(PatchId::Atmosphere) {
            options.set(
                PatchId::Atmosphere,
                option.key,
                layers.contains(&option.key),
            );
        }
        let ctx = TransformCtx {
            options: &options,
            ..TransformCtx::default()
        };
        let out = transform(
            PatchId::Atmosphere,
            "metadata/environmentsettings/test.env",
            &encode_utf16_bom(ENV),
            ctx,
        )
        .unwrap();
        decode_utf16(&out).unwrap()
    }

    #[test]
    fn atmosphere_with_every_layer_matches_legacy_env_particles() {
        let out = transform(
            PatchId::Atmosphere,
            "metadata/environmentsettings/test.env",
            &encode_utf16_bom(ENV),
            TransformCtx::default(),
        )
        .unwrap();
        assert_eq!(decode_utf16(&out).unwrap(), legacy_env_particles(ENV));
    }

    #[test]
    fn atmosphere_single_layers_match_legacy_weather_patches() {
        assert_eq!(atmosphere_with(&["fog"]), ENV.replace("\"fog\"", "\"xog\""));
        assert_eq!(
            atmosphere_with(&["rain"]),
            ENV.replace("\"rain_intensity\": 1.0", "\"rain_intensity\": 0.0")
        );
        assert_eq!(
            atmosphere_with(&["clouds"]),
            ENV.replace("\"clouds_intensity\": 0.5", "\"clouds_intensity\": 0.0")
        );
        assert_eq!(
            atmosphere_with(&["bloom"]),
            ENV.replace("\"post_processing\"", "\"xost_processing\"")
        );
    }

    #[test]
    fn light_disables_only_enabled_layers() {
        let mut options = PatchOptions::new();
        options.set(PatchId::Light, "player_light", false);
        let ctx = TransformCtx {
            options: &options,
            ..TransformCtx::default()
        };
        let out = transform(
            PatchId::Light,
            "metadata/environmentsettings/test.env",
            &encode_utf16_bom(ENV),
            ctx,
        )
        .unwrap();
        let text = decode_utf16(&out).unwrap();
        assert!(text.contains("\"xirectional_light\""));
        assert!(text.contains("\"player_light\""));
        assert!(text.contains("\"xnvironment_mapping\""));
        assert!(text.contains("\"xlobal_illumination\""));
    }

    const UBER: &str = "uint dof_enable;\r\nuint desaturation_enable;\r\nuint vignette_enable;\r\nfloat4 main() {\r\n\tif ( dof_enable )\r\n\t{ blur(); }\r\n\tif( desaturation_enable )\r\n\t{ grey(); }\r\n\tif( vignette_enable )\r\n\t{ edge(); }\r\n\treturn c;\r\n}\r\n";

    #[test]
    fn post_process_switches_off_selected_filters() {
        let mut options = PatchOptions::new();
        options.set(PatchId::PostProcess, "desaturation", false);
        let ctx = TransformCtx {
            options: &options,
            ..TransformCtx::default()
        };
        let out = transform(
            PatchId::PostProcess,
            "shaders/postprocessuber.hlsl",
            UBER.as_bytes(),
            ctx,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("if( 0 )\r\n\t{ blur(); }"));
        assert!(text.contains("if( desaturation_enable )"));
        assert!(text.contains("if( 0 )\r\n\t{ edge(); }"));
    }

    #[test]
    fn post_process_matches_reformatted_conditions_and_rejects_missing_switches() {
        let path = "shaders/postprocessuber.hlsl";
        let ctx = TransformCtx::default();
        let spaced = UBER.replace("if( vignette_enable )", "if (vignette_enable)");
        let out = transform(PatchId::PostProcess, path, spaced.as_bytes(), ctx).unwrap();
        assert!(
            !String::from_utf8(out)
                .unwrap()
                .contains("(vignette_enable)")
        );

        let changed = UBER.replace("if( vignette_enable )", "if( vignette_enable && x )");
        assert!(transform(PatchId::PostProcess, path, changed.as_bytes(), ctx).is_err());

        let removed = UBER.replace("vignette_enable", "edge_enable");
        assert!(transform(PatchId::PostProcess, path, removed.as_bytes(), ctx).is_err());
    }

    #[test]
    fn post_process_and_black_screen_compose_in_either_order() {
        let path = "shaders/postprocessuber.hlsl";
        let ctx = TransformCtx::default();
        let black = transform(PatchId::BlackScreen, path, UBER.as_bytes(), ctx).unwrap();
        let black_then_post = transform(PatchId::PostProcess, path, &black, ctx).unwrap();
        assert_eq!(black_then_post, black);

        let post = transform(PatchId::PostProcess, path, UBER.as_bytes(), ctx).unwrap();
        let post_then_black = transform(PatchId::BlackScreen, path, &post, ctx).unwrap();
        assert_eq!(post_then_black, black);
    }

    #[test]
    fn minimap_default_style_matches_the_fixed_patch() {
        let blending = "\tfloat4 walkable_color = float4(1.0f, 1.0f, 1.0f, 0.01f);\r\n";
        let out = transform(
            PatchId::Minimap,
            "shaders/minimap_blending_pixel.hlsl",
            blending.as_bytes(),
            TransformCtx::default(),
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\tfloat4 walkable_color = float4(0.0f, 0.0f, 0.0f, 0.3f);\r\n"
        );
    }

    #[test]
    fn protected_particle_and_effect_paths_are_unchanged() {
        let particle_input = encode_utf16_bom("keep particle");
        let particle = particles(
            "metadata/particles/monster_effects/league_legion/rewardsystem/foo.pet",
            &particle_input,
        )
        .unwrap();
        assert_eq!(particle, particle_input);

        let effect_input = encode_utf16_bom("client\n{\n  ParticleEffects\n  {\n  }\n}");
        let effect = effects(
            "metadata/effects/spells/monsters_effects/atlasofworldsbosses/foo.aoc",
            &effect_input,
            None,
        )
        .unwrap();
        assert_eq!(effect, effect_input);
    }

    #[test]
    fn effect_skill_levels_control_strip_behavior() {
        use super::super::effect_skills::EffectSkillOverride;

        let filter = EffectsFilter::new(
            &[EffectSkillOverride {
                folder: "fireball".to_string(),
                level: EffectLevel::Full,
            }],
            Default::default(),
        )
        .unwrap();
        let filter = Some(&filter);

        // Unlisted folders strip .ao/.aoc exactly like the default Reduced level.
        let anim = encode_utf16_bom(
            "client\r\n{\r\n\tParticleEffects\r\n\t{\r\n\t}\r\n\tSoundEvents\r\n\t{\r\n\t}\r\n}",
        );
        let herald_ao = "metadata/effects/spells/cold_herald_of_ice/ao/ice_explosion.ao";
        let reduced_ao = effects(herald_ao, &anim, filter).unwrap();
        assert_eq!(reduced_ao, effects(herald_ao, &anim, None).unwrap());
        assert_ne!(reduced_ao, anim);

        // Full also passes .ao candidates collected by SkillSounds.
        assert_eq!(
            effects(
                "metadata/effects/spells/fireball/fireball.ao",
                &anim,
                filter
            )
            .unwrap(),
            anim
        );

        // Without a filter (or at Reduced) non-anim files are never touched.
        let data = encode_utf16_bom("some effect data");
        assert_eq!(
            effects(
                "metadata/effects/spells/fireball/fx/impact.pet",
                &data,
                None
            )
            .unwrap(),
            data
        );
        assert_eq!(
            effects(
                "metadata/effects/spells/monsters_effects/league_ultimatum/mechanics/fx/arena_limit.pet",
                &data,
                filter
            )
            .unwrap(),
            data
        );
    }

    #[test]
    fn sound_transform_reproduces_captured_output_byte_for_byte() {
        // Real shape: the SoundEvents body holds JSON with its own braces.
        let before = "version 3\r\nextends \"Metadata/Parent\"\r\n\r\nclient\r\n{\r\n\t\
ParticleEffects\r\n\t{\r\n\t\tanimations = '[\n\t\t\t{\n\t\t\t\t\"name\": \"once\"\n\t\t\t}\n\t\t]'\r\n\t}\r\n\t\r\n\t\
SoundEvents\r\n\t{\r\n\t\tanimations = '[\n\t\t\t{\n\t\t\t\t\"type\": \"SoundEventType\",\n\t\t\t\t\"filename\": \"Audio/x\"\n\t\t\t}\n\t\t]'\r\n\t}\r\n}";
        let after = "version 3\r\nextends \"Metadata/Parent\"\r\n\r\nclient\r\n{\r\n\t\
ParticleEffects\r\n\t{\r\n\t\tanimations = '[\n\t\t\t{\n\t\t\t\t\"name\": \"once\"\n\t\t\t}\n\t\t]'\r\n\t}\r\n\t\r\n\t\
SoundEvents {}\r\n}";

        let out = transform(
            PatchId::SkillSounds,
            "metadata/effects/spells/absolution_blast/aoe_explosion_01.ao",
            &encode_utf16_bom(before),
            TransformCtx::default(),
        )
        .unwrap();

        assert_eq!(out, encode_utf16_bom(after));
        // ParticleEffects (which also references external files) is untouched.
        assert!(decode_utf16(&out).unwrap().contains("\"name\": \"once\""));
    }

    #[test]
    fn sound_transform_empties_sound_blocks_and_preserves_structure() {
        let input = encode_utf16_bom(
            "version 3\nclient\n{\n\tSoundEvents\n\t{\n\t\tanimations = 'aaa'\n\t}\n\tSoundParams\n\t{\n\t\tanimations = 'bbb'\n\t}\n\tAnimatedRender\n\t{\n\t\tkeep = true\n\t}\n}",
        );
        let out = transform(
            PatchId::DisableSounds,
            "metadata/effects/spells/fireball/fireball.ao",
            &input,
            TransformCtx::default(),
        )
        .unwrap();
        let text = decode_utf16(&out).unwrap();

        // Sound blocks emptied in place (name kept, body gone).
        assert!(text.contains("SoundEvents {}"));
        assert!(text.contains("SoundParams {}"));
        assert!(!text.contains("aaa"));
        assert!(!text.contains("bbb"));
        // Non-sound structure preserved verbatim, braces balanced.
        assert!(text.contains("keep = true"));
        assert_eq!(text.matches('{').count(), text.matches('}').count());
    }

    #[test]
    fn mtx_soft_empties_effect_data_files_and_leaves_anim_files_alone() {
        let stuff = encode_utf16_bom("stuff");
        // .epk must become empty (a bare "0" would crash the parser).
        let epk = transform(
            PatchId::MtxSoft,
            "metadata/effects/microtransactions/wings/angel/angel.epk",
            &stuff,
            TransformCtx::default(),
        )
        .unwrap();
        assert_eq!(epk, vec![0xff, 0xfe]);
        // .pet/.trl become BOM+"0" (tolerated by the engine).
        for ext in ["pet", "trl"] {
            let out = transform(
                PatchId::MtxSoft,
                &format!("metadata/effects/microtransactions/wings/angel/angel.{ext}"),
                &stuff,
                TransformCtx::default(),
            )
            .unwrap();
            assert_eq!(out, encode_utf16_bom("0"));
        }
        // Animation files are left untouched.
        let ao_in = encode_utf16_bom("version 3\nclient\n{\n}");
        let ao = transform(
            PatchId::MtxSoft,
            "metadata/effects/microtransactions/wings/angel/angel.ao",
            &ao_in,
            TransformCtx::default(),
        )
        .unwrap();
        assert_eq!(ao, ao_in);
    }

    #[test]
    fn mtx_soft_keeps_town_portals_unless_the_option_is_on() {
        let stuff = encode_utf16_bom("stuff");
        let path = "Metadata/Effects/Microtransactions/Town_Portals/miseryportal/fx/open.pet";
        let kept = transform(PatchId::MtxSoft, path, &stuff, TransformCtx::default()).unwrap();
        assert_eq!(kept, stuff);

        let mut options = PatchOptions::new();
        options.set(PatchId::MtxSoft, "town_portals", true);
        let ctx = TransformCtx {
            options: &options,
            ..TransformCtx::default()
        };
        let stripped = transform(PatchId::MtxSoft, path, &stuff, ctx).unwrap();
        assert_eq!(stripped, encode_utf16_bom("0"));
    }

    #[test]
    fn monster_hp_bar_inserts_stats_after_drop_slots_anchor() {
        let input = encode_utf16_bom(
            "version 2\r\nextends \"Metadata/Base\"\r\n\r\nStats\r\n{\r\n\titem_drop_slots = 1\r\n\tset_monster_scale = 100\r\n}",
        );
        let out = transform(
            PatchId::MonsterHpBar,
            "metadata/monsters/monster.ot",
            &input,
            TransformCtx::default(),
        )
        .unwrap();

        let expected = encode_utf16_bom(
            "version 2\r\nextends \"Metadata/Base\"\r\n\r\nStats\r\n{\r\n\titem_drop_slots = 1\r\n\tbase_maximum_energy_shield = 1\r\n\tbase_maximum_life = 1\r\n\tset_monster_scale = 100\r\n}",
        );
        assert_eq!(out, expected);

        // Re-applying to already-patched bytes changes nothing.
        let again = transform(
            PatchId::MonsterHpBar,
            "metadata/monsters/monster.ot",
            &out,
            TransformCtx::default(),
        )
        .unwrap();
        assert_eq!(again, out);
    }

    #[test]
    fn monster_hp_bar_falls_back_to_stats_block_when_anchor_is_missing() {
        let input =
            encode_utf16_bom("version 2\r\n\r\nStats\r\n{\r\n\tset_monster_scale = 100\r\n}");
        let out = transform(
            PatchId::MonsterHpBar,
            "metadata/monsters/monster.ot",
            &input,
            TransformCtx::default(),
        )
        .unwrap();
        let text = decode_utf16(&out).unwrap();
        assert!(text.contains(
            "Stats\r\n{\r\n\tbase_maximum_energy_shield = 1\r\n\tbase_maximum_life = 1\r\n\tset_monster_scale = 100"
        ));

        // No anchor and no Stats block: leave the file untouched.
        let bare = encode_utf16_bom("version 2\r\nextends \"Metadata/Base\"");
        let untouched = transform(
            PatchId::MonsterHpBar,
            "metadata/monsters/monster.ot",
            &bare,
            TransformCtx::default(),
        )
        .unwrap();
        assert_eq!(untouched, bare);
    }

    #[test]
    fn black_screen_stubs_function_bodies_by_return_type() {
        // Shapes from real PoE2 shaders: nested braces, macro params, and a comment
        // before the body.
        let input = b"#include \"include/common.hlsl\"\n\nfloat3 ComputeLight(float3 normal, float3 dir)\n{\n\tif (dot(normal, dir) > 0.0f)\n\t{\n\t\treturn normal * dir;\n\t}\n\treturn (float3)0.5f;\n}\n\nfloat4 SampleEnv(uniform TEXTURECUBE_DECL(spec_tex), float3 dir)\n{\n\treturn SAMPLE_TEXCUBELOD(spec_tex, Sampler, float4(dir, 0.0f));\n}\n\nfloat SmithTerm(float alpha, float VdotN) // * VdotN * LdotN\n{\n\treturn alpha * VdotN;\n}\n\nvoid Accumulate(inout float4 total)\n{\n\ttotal += 1.0f;\n}\n";
        let out = transform(
            PatchId::BlackScreen,
            "shaders/include/lighting.hlsl",
            input,
            TransformCtx::default(),
        )
        .unwrap();

        let expected = "#include \"include/common.hlsl\"\n\nfloat3 ComputeLight(float3 normal, float3 dir)\n{\n\treturn (float3)0;\n}\n\nfloat4 SampleEnv(uniform TEXTURECUBE_DECL(spec_tex), float3 dir)\n{\n\treturn (float4)0;\n}\n\nfloat SmithTerm(float alpha, float VdotN) // * VdotN * LdotN\n{\n\treturn (float)0;\n}\n\nvoid Accumulate(inout float4 total)\n{ }\n";
        assert_eq!(String::from_utf8(out.clone()).unwrap(), expected);

        // Re-applying to already-stubbed bytes changes nothing.
        let again = transform(
            PatchId::BlackScreen,
            "shaders/include/lighting.hlsl",
            &out,
            TransformCtx::default(),
        )
        .unwrap();
        assert_eq!(again, out);
    }

    #[test]
    fn black_screen_stubs_outpixel_shader_and_skips_forward_declarations() {
        let input = b"float ShadowTerm(float3 pos);\n\nOutPixel main(InPixel input) : SV_Target\n{\n\tOutPixel res;\n\tres.shadowmap_data = ComputeShadows(input);\n\treturn res;\n}\n";
        let out = transform(
            PatchId::BlackScreen,
            "shaders/screenspaceshadows.hlsl",
            input,
            TransformCtx::default(),
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();

        // A forward declaration must not swallow the next function body.
        assert!(text.starts_with("float ShadowTerm(float3 pos);\n"));
        assert!(text.contains(
            "{\tOutPixel res;\r\n\tres.shadowmap_data = float4(1.0f, 1.0f, 1.0f, 1.0f);\r\n\treturn res;\n}"
        ));
        assert!(!text.contains("ComputeShadows"));
    }

    #[test]
    fn startup_scene_protection_matches_character_selection_anywhere_in_path() {
        let input = encode_utf16_bom("version 3\nclient\n{\n\tSoundEvents\n\t{\n\t\tx = 1\n\t}\n}");
        let out = transform(
            PatchId::DisableSounds,
            "metadata/effects/misc/CharacterSelection/gallows.ao",
            &input,
            TransformCtx::default(),
        )
        .unwrap();

        assert_eq!(out, input);
    }
}
