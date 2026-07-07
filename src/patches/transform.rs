use super::catalog::PatchId;
use super::color_mods::ColorMatcher;
use super::targeting::{
    ends_with_path_ci, is_startup_scene_protected, normalize_path, patch_applies_path,
    patch_targets_path, EFFECT_PROTECTED_PREFIXES, PARTICLE_PROTECTED_PREFIXES,
};
use super::text::{
    decode_utf16, decode_utf16_lossless, empty_named_blocks, encode_utf16_bom, find_matching_brace,
    regex_utf16, remove_function_calls, replace_array_property, replace_utf16, strip_client_blocks,
};
use anyhow::Result;
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

static RAIN_INTENSITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"("rain_intensity":\s*)([^,\r\n}]+)(,?)"#).unwrap());
static CLOUDS_INTENSITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"("clouds_intensity":\s*)([^,\r\n}]+)(,?)"#).unwrap());
// HLSL function signature: return type, name, parameter list. Longest-first
// alternation so `float4x4` never half-matches as `float`; the parameter list
// allows one level of nested parens for macro params like `TEXTURE2D_DECL(tex)`.
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
// Sound removal empties the *bodies* of exactly these blocks in place
// (`SoundEvents { ... }` -> `SoundEvents {}`), leaving every other byte intact.
// Derived by diffing captured output: across 170 changed `.ao`/`.ot`
// files these were the only blocks ever emptied, and emptying them reproduces
// the captured bytes exactly. See docs/capture-findings.md.
static SOUND_EMPTY_BLOCKS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["SoundEvents", "SoundParams"].into_iter().collect());

/// Shared, per-request inputs for parameterized transforms. Built once in
/// `compute_patch_set` and borrowed by every rayon transform task.
#[derive(Clone, Copy)]
pub(super) struct TransformCtx<'a> {
    pub zoom: f64,
    pub color: Option<&'a ColorMatcher>,
}

impl Default for TransformCtx<'_> {
    fn default() -> Self {
        Self {
            zoom: 2.4,
            color: None,
        }
    }
}

/// Whether `patch` would select `path` as a target, and (if so) the bytes it
/// would write for it. Exposed for the `capture-diff` dev tool to verify the
/// capture-driven transforms against reference output. Returns `None`
/// when the patch does not target this path.
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
        PatchId::Minimap => minimap(path, bytes),
        PatchId::AtlasFog => atlas_fog(bytes),
        PatchId::ColorMods => match ctx.color {
            Some(matcher) => matcher.colorize(bytes),
            None => Ok(bytes.to_vec()),
        },
        PatchId::Fog => replace_utf16(bytes, &[("\"fog\"", "\"xog\"")]),
        PatchId::Rain => regex_utf16(bytes, &RAIN_INTENSITY_RE, "${1}0.0${3}"),
        PatchId::Clouds => regex_utf16(bytes, &CLOUDS_INTENSITY_RE, "${1}0.0${3}"),
        PatchId::EnvParticles => env_particles(bytes),
        PatchId::Shadow => replace_utf16(
            bytes,
            &[("\"shadows_enabled\": true", "\"shadows_enabled\": false")],
        ),
        PatchId::Light => replace_utf16(
            bytes,
            &[
                ("\"directional_light\"", "\"xirectional_light\""),
                ("\"player_light\"", "\"xlayer_light\""),
                ("\"environment_mapping\"", "\"xnvironment_mapping\""),
                ("\"global_illumination\"", "\"xlobal_illumination\""),
            ],
        ),
        PatchId::Delirium => delirium(bytes),
        PatchId::Particles => particles(path, bytes),
        PatchId::Effects => effects(path, bytes),
        PatchId::DisableSounds => strip_sounds(path, bytes),
        PatchId::SkillSounds => strip_sounds(path, bytes),
        PatchId::MonsterSounds => strip_sounds(path, bytes),
        PatchId::MtxSoft => mtx_soft(path, bytes),
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

fn minimap(path: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    if path.ends_with("minimap_visibility_pixel.hlsl") {
        if !text.contains("res_color = max(res_color, 0.18f);") {
            let mut lines: Vec<String> = text.split("\r\n").map(str::to_string).collect();
            if let Some(index) = lines
                .iter()
                .position(|line| line.contains("res_color = float4(1.0f, 0.0f, 0.0f, 1.0f);"))
            {
                lines.insert(
                    index + 1,
                    "\tres_color = max(res_color, 0.18f);".to_string(),
                );
                text = lines.join("\r\n");
            }
        }
    } else if path.ends_with("minimap_blending_pixel.hlsl") {
        text = text
            .replace(
                "float4 walkable_color = float4(1.0f, 1.0f, 1.0f, 0.01f);",
                "float4 walkable_color = float4(0.0f, 0.0f, 0.0f, 0.3f);",
            )
            .replace(
                "float4 walkability_map_color = lerp(walkable_color, float4(0.5f, 0.5f, 1.0f, 0.5f), walkable_to_edge_ratio);",
                "float4 walkability_map_color = lerp(walkable_color, float4(12.0f, 12.0f, 12.0f, 0.1f), walkable_to_edge_ratio);",
            );
    }
    Ok(text.into_bytes())
}

fn atlas_fog(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut text = decode_utf16(bytes)?;
    text = replace_array_property(text, "nodes");
    text = replace_array_property(text, "links");
    Ok(encode_utf16_bom(&text))
}

fn env_particles(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut text = decode_utf16(bytes)?;
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
    text = CLOUDS_INTENSITY_RE
        .replace_all(&text, "${1}0.0${3}")
        .into_owned();
    Ok(encode_utf16_bom(&text))
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

fn effects(path: &str, bytes: &[u8]) -> Result<Vec<u8>> {
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
    // Ground-truth rule (see SOUND_EMPTY_BLOCKS): empty the SoundEvents/SoundParams
    // block bodies in place and leave everything else untouched. This matches the
    // captured output byte-for-byte and, unlike the old line-deletion approach,
    // never corrupts file structure.
    Ok(encode_utf16_bom(&empty_named_blocks(
        &text,
        &SOUND_EMPTY_BLOCKS,
    )))
}

fn mtx_soft(path: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    if is_startup_scene_protected(path) {
        return Ok(bytes.to_vec());
    }
    // Ground-truth rule (see docs/capture-findings.md): empty microtransaction
    // effect/particle data files. `.epk` MUST become empty
    // (a bare "0" makes the parser throw "Unexpected token 0"); `.pet`/`.trl`
    // become BOM+"0" (a value the engine tolerates). Animation files
    // (`.ao`/`.aoc`) are left untouched; the capture shows they are never
    // rewritten for the soft-mtx option.
    if ends_with_path_ci(path, ".epk") {
        return Ok(encode_utf16_bom(""));
    }
    if ends_with_path_ci(path, ".pet") || ends_with_path_ci(path, ".trl") {
        return Ok(encode_utf16_bom("0"));
    }
    Ok(bytes.to_vec())
}

/// Gives every monster 1 base energy shield/life via the shared
/// `metadata/monsters/monster.ot` template, which makes the engine render HP
/// bars permanently instead of only after the monster takes damage
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

/// Stubs every HLSL function body in the targeted post-processing/lighting
/// shaders to `return (type)0;`, so lighting and the post-process chain
/// output nothing and the world renders black, while the UI — drawn after
/// post-processing — stays intact.
fn black_screen(bytes: &[u8]) -> Result<Vec<u8>> {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while let Some(found) = HLSL_FUNCTION_RE.captures(&text[pos..]) {
        let signature_end = pos + found.get(0).expect("group 0 always exists").end();
        // Only accept a body directly following the signature (allowing a
        // `: SEMANTIC` return annotation); anything else — e.g. a forward
        // declaration ending in `;` — is copied through untouched rather than
        // grabbing the next `{` anywhere.
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

/// Index of the `{` opening a function body that directly follows a signature
/// ending at `from`, allowing only whitespace, `//` line comments, and one
/// `: SEMANTIC` return annotation in between.
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

/// Replacement bodies byte-for-byte.
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

/// Index of the first line inside the `Stats { ... }` block body, if present.
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
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(!out.contains("CreateCameraPanNode"));
        assert!(out.contains("bar = 2"));
    }

    #[test]
    fn multiple_transforms_apply_in_selected_order_for_one_target() {
        let input = encode_utf16_bom(
            r#""fog"
"rain_intensity": 1.0,
"clouds_intensity": 1.0,
"#,
        );
        let mut bytes = input;
        for patch in [PatchId::Fog, PatchId::Rain, PatchId::Clouds] {
            bytes = transform(
                patch,
                "metadata/environmentsettings/test.env",
                &bytes,
                TransformCtx::default(),
            )
            .unwrap();
        }
        let text = decode_utf16(&bytes).unwrap();

        assert!(text.contains(r#""xog""#));
        assert!(text.contains(r#""rain_intensity": 0.0,"#));
        assert!(text.contains(r#""clouds_intensity": 0.0,"#));
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
        )
        .unwrap();
        assert_eq!(effect, effect_input);
    }

    #[test]
    fn sound_transform_reproduces_captured_output_byte_for_byte() {
        // Real captured input/output shape (absolution_blast/aoe_explosion_01.ao):
        // the SoundEvents body holds an `animations = '[ ... ]'` array whose JSON
        // contains braces, so emptying must rely on array-aware brace matching.
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
            "metadata/effects/microtransactions/portal/portal.epk",
            &stuff,
            TransformCtx::default(),
        )
        .unwrap();
        assert_eq!(epk, vec![0xff, 0xfe]);
        // .pet/.trl become BOM+"0" (tolerated by the engine).
        for ext in ["pet", "trl"] {
            let out = transform(
                PatchId::MtxSoft,
                &format!("metadata/effects/microtransactions/portal/portal.{ext}"),
                &stuff,
                TransformCtx::default(),
            )
            .unwrap();
            assert_eq!(out, encode_utf16_bom("0"));
        }
        // Animation files are left untouched (capture shows soft-mtx never edits them).
        let ao_in = encode_utf16_bom("version 3\nclient\n{\n}");
        let ao = transform(
            PatchId::MtxSoft,
            "metadata/effects/microtransactions/portal/portal.ao",
            &ao_in,
            TransformCtx::default(),
        )
        .unwrap();
        assert_eq!(ao, ao_in);
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
        // Shapes taken from the real PoE2 shader files: nested braces in a
        // body, macro parens in a parameter list, and a trailing line comment
        // between the signature and the body.
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

        // The forward declaration is untouched; its `;` must not make the
        // stubber swallow the next function's body.
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
