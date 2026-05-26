use crate::bundle::{slice_file, BundleFile, BundleIndex, BundleStore};
use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PatchId {
    Camera,
    Minimap,
    AtlasFog,
    Fog,
    Rain,
    Clouds,
    EnvParticles,
    Shadow,
    Light,
    Delirium,
    Particles,
    Effects,
}

#[derive(Debug, Clone)]
pub struct PatchInfo {
    pub id: PatchId,
    pub name: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone)]
pub struct PatchChange {
    pub path: String,
    pub bundle_name: String,
    pub old_size: usize,
    pub new_size: usize,
}

#[derive(Debug, Clone)]
pub struct PatchSet {
    pub changes: Vec<PatchChange>,
    pub replacements: HashMap<String, Vec<(BundleFile, Vec<u8>)>>,
}

pub fn all_patches() -> &'static [PatchInfo] {
    &[
        PatchInfo {
            id: PatchId::Camera,
            name: "camera",
            description: "Adjust camera zoom and remove camera reset calls.",
        },
        PatchInfo {
            id: PatchId::Minimap,
            name: "minimap",
            description: "Reveal more of the minimap by default.",
        },
        PatchInfo {
            id: PatchId::AtlasFog,
            name: "atlas-fog",
            description: "Remove Atlas fog of war graph nodes.",
        },
        PatchInfo {
            id: PatchId::Fog,
            name: "fog",
            description: "Disable environment fog.",
        },
        PatchInfo {
            id: PatchId::Rain,
            name: "rain",
            description: "Set rain intensity to zero.",
        },
        PatchInfo {
            id: PatchId::Clouds,
            name: "clouds",
            description: "Set cloud intensity to zero.",
        },
        PatchInfo {
            id: PatchId::EnvParticles,
            name: "env-particles",
            description: "Disable environment particles and related effects.",
        },
        PatchInfo {
            id: PatchId::Shadow,
            name: "shadow",
            description: "Disable shadows in environment settings.",
        },
        PatchInfo {
            id: PatchId::Light,
            name: "light",
            description: "Disable selected environment lighting systems.",
        },
        PatchInfo {
            id: PatchId::Delirium,
            name: "delirium",
            description: "Disable delirium/affliction environment effects.",
        },
        PatchInfo {
            id: PatchId::Particles,
            name: "particles",
            description: "Blank particle effect files.",
        },
        PatchInfo {
            id: PatchId::Effects,
            name: "effects",
            description: "Strip nonessential client effect blocks.",
        },
    ]
}

pub fn parse_patch(name: &str) -> Option<PatchId> {
    all_patches()
        .iter()
        .find(|patch| patch.name.eq_ignore_ascii_case(name))
        .map(|patch| patch.id)
}

pub fn compute_patch_set(
    store: &BundleStore,
    index: &mut BundleIndex,
    patches: &[PatchId],
    zoom: f64,
) -> Result<PatchSet> {
    crate::timing!("patch_scan_compute");

    let mut candidates = Vec::new();
    for patch in patches {
        let targets = patch_targets(index, *patch)?;
        if targets.is_empty() {
            bail!(
                "patch '{}' has no matching files in this game version;\n\
                 verify game files or wait for a tiny-poe2smoother update",
                patch_label(*patch)
            );
        }
        for path in targets {
            let file = index
                .file_by_path(&path)
                .ok_or_else(|| anyhow!("patch target disappeared from index: {path}"))?
                .clone();
            candidates.push((path, file));
        }
    }
    let candidates = dedup_candidates(candidates);

    crate::timing!("bundle_batch_read");
    let bundle_names: Vec<String> = {
        let mut names: Vec<String> = candidates
            .iter()
            .map(|(_, f)| f.bundle_name.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    };
    let bundles = store.read_bundles_batch(&bundle_names)?;

    crate::timing!("patch_read_slice");
    let mut file_data: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (path, file) in &candidates {
        let bundle_data = bundles.get(&file.bundle_name).with_context(|| {
            format!("bundle loaded but missing from batch: {}", file.bundle_name)
        })?;
        let bytes = slice_file(bundle_data, file)
            .with_context(|| format!("failed to read patch target from bundle: {path}"))?;
        file_data.insert(path.clone(), bytes);
    }

    crate::timing!("patch_transform");
    let mut changed: BTreeMap<String, bool> = BTreeMap::new();
    for (path, _) in &candidates {
        changed.insert(path.clone(), false);
    }
    for patch in patches {
        for (path, _) in &candidates {
            if patch_applies(*patch, path) {
                let before = file_data
                    .get(path)
                    .ok_or_else(|| anyhow!("patch target bytes missing after read: {path}"))?;
                let after = transform(*patch, path, before, zoom)?;
                if &after != before {
                    file_data.insert(path.clone(), after);
                    changed.insert(path.clone(), true);
                }
            }
        }
    }

    build_patch_set_from_changed(&candidates, &mut file_data, &changed)
}

fn build_patch_set_from_changed(
    candidates: &[(String, BundleFile)],
    file_data: &mut BTreeMap<String, Vec<u8>>,
    changed: &BTreeMap<String, bool>,
) -> Result<PatchSet> {
    let mut changes = Vec::new();
    let mut replacements: HashMap<String, Vec<(BundleFile, Vec<u8>)>> = HashMap::new();
    for (path, file) in candidates {
        if *changed.get(path).unwrap_or(&false) {
            let bytes = file_data
                .remove(path)
                .ok_or_else(|| anyhow!("changed patch target bytes missing: {path}"))?;
            changes.push(PatchChange {
                path: path.clone(),
                bundle_name: file.bundle_name.clone(),
                old_size: file.size as usize,
                new_size: bytes.len(),
            });
            replacements
                .entry(file.bundle_name.clone())
                .or_default()
                .push((file.clone(), bytes));
        }
    }

    Ok(PatchSet {
        changes,
        replacements,
    })
}

fn dedup_candidates(candidates: Vec<(String, BundleFile)>) -> Vec<(String, BundleFile)> {
    candidates
        .into_iter()
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .collect()
}

fn patch_label(id: PatchId) -> &'static str {
    all_patches()
        .iter()
        .find(|patch| patch.id == id)
        .map(|patch| patch.name)
        .unwrap_or("unknown")
}

fn patch_targets(index: &mut BundleIndex, patch: PatchId) -> Result<Vec<String>> {
    Ok(match patch {
        PatchId::Camera => {
            let mut paths: Vec<_> = index
                .matching_paths("metadata/", &[".ot", ".otc"])?
                .into_iter()
                .filter(|entry| {
                    !entry
                        .path
                        .eq_ignore_ascii_case("metadata/characters/character.ot")
                })
                .map(|entry| entry.path)
                .collect();
            if index
                .file_by_path("metadata/characters/character.ot")
                .is_some()
            {
                paths.push("metadata/characters/character.ot".to_string());
            }
            paths
        }
        PatchId::Minimap => [
            "shaders/minimap_visibility_pixel.hlsl",
            "shaders/minimap_blending_pixel.hlsl",
        ]
        .into_iter()
        .filter(|path| index.file_by_path(path).is_some())
        .map(str::to_string)
        .collect(),
        PatchId::AtlasFog => ["metadata/materials/environment/worldmap/worldmap_fogofwar.fxgraph"]
            .into_iter()
            .filter(|path| index.file_by_path(path).is_some())
            .map(str::to_string)
            .collect(),
        PatchId::Fog
        | PatchId::Rain
        | PatchId::Clouds
        | PatchId::EnvParticles
        | PatchId::Shadow
        | PatchId::Light => index
            .matching_paths("metadata/environmentsettings", &[".env"])?
            .into_iter()
            .map(|entry| entry.path)
            .collect(),
        PatchId::Delirium => index
            .matching_paths(
                "metadata/effects/environment/league_affliction",
                &[".ao", ".aoc"],
            )?
            .into_iter()
            .map(|entry| entry.path)
            .collect(),
        PatchId::Particles => index
            .matching_paths("metadata/particles", &[".pet", ".trl"])?
            .into_iter()
            .map(|entry| entry.path)
            .collect(),
        PatchId::Effects => index
            .matching_paths("metadata/effects/spells", &[".aoc", ".ao"])?
            .into_iter()
            .map(|entry| entry.path)
            .collect(),
    })
}

fn patch_applies(patch: PatchId, path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    match patch {
        PatchId::Camera => normalized.ends_with(".ot") || normalized.ends_with(".otc"),
        PatchId::Minimap => {
            normalized.ends_with("minimap_visibility_pixel.hlsl")
                || normalized.ends_with("minimap_blending_pixel.hlsl")
        }
        PatchId::AtlasFog => normalized.contains("worldmap_fogofwar.fxgraph"),
        PatchId::Fog
        | PatchId::Rain
        | PatchId::Clouds
        | PatchId::EnvParticles
        | PatchId::Shadow
        | PatchId::Light => {
            normalized.starts_with("metadata/environmentsettings") && normalized.ends_with(".env")
        }
        PatchId::Delirium => {
            normalized.starts_with("metadata/effects/environment/league_affliction")
                && (normalized.ends_with(".ao") || normalized.ends_with(".aoc"))
        }
        PatchId::Particles => {
            normalized.starts_with("metadata/particles")
                && (normalized.ends_with(".pet") || normalized.ends_with(".trl"))
        }
        PatchId::Effects => {
            normalized.starts_with("metadata/effects/spells")
                && (normalized.ends_with(".aoc") || normalized.ends_with(".ao"))
        }
    }
}

fn transform(patch: PatchId, path: &str, bytes: &[u8], zoom: f64) -> Result<Vec<u8>> {
    match patch {
        PatchId::Camera => camera(path, bytes, zoom),
        PatchId::Minimap => minimap(path, bytes),
        PatchId::AtlasFog => atlas_fog(bytes),
        PatchId::Fog => replace_utf16(bytes, &[("\"fog\"", "\"xog\"")]),
        PatchId::Rain => regex_utf16(
            bytes,
            r#"("rain_intensity":\s*)([^,\r\n}]+)(,?)"#,
            "${1}0.0${3}",
        ),
        PatchId::Clouds => regex_utf16(
            bytes,
            r#"("clouds_intensity":\s*)([^,\r\n}]+)(,?)"#,
            "${1}0.0${3}",
        ),
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
    let rain = Regex::new(r#"("rain_intensity":\s*)([^,\r\n}]+)(,?)"#)?;
    let clouds = Regex::new(r#"("clouds_intensity":\s*)([^,\r\n}]+)(,?)"#)?;
    text = rain.replace_all(&text, "${1}0.0${3}").into_owned();
    text = clouds.replace_all(&text, "${1}0.0${3}").into_owned();
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
    let protected = [
        "metadata/particles/monster_effects/league_legion/rewardsystem",
        "metadata/particles/monster_effects/league_legion/endgame",
        "metadata/particles/monster_effects/league_delve/general",
        "metadata/particles/monster_effects/atlasexiles/adjudicator",
        "metadata/particles/monster_effects/atlasexiles/adjudicatormonsters",
        "metadata/particles/enviro_effects/act3/blood_temple",
        "metadata/particles/ground_effects_v2/smoke_blind_chimera",
        "metadata/particles/monster_effects/atlasofworldsbosses/chimera",
        "metadata/particles/monster_effects/atlasexiles/orion",
    ];
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    if protected
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
    {
        return Ok(bytes.to_vec());
    }
    Ok(encode_utf16_bom("0"))
}

fn effects(path: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    let protected = [
        "metadata/effects/spells/monsters_effects/league_expedition/dynamic_marker",
        "metadata/effects/spells/monsters_effects/atlasofworldsbosses",
        "metadata/effects/spells/monsters_effects/league_azmeri/guiding_light",
        "metadata/effects/spells/monsters_effects/league_azmeri/monster_fx",
        "metadata/effects/spells/monsters_effects/league_azmeri/resources/affecting_area",
        "metadata/effects/spells/monsters_effects/league_azmeri/resources/feature_room_dust",
        "metadata/effects/spells/monsters_effects/league_azmeri/resources/guiding_light",
        "metadata/effects/spells/monsters_effects/league_azmeri/resources/wisp_doodads",
        "metadata/effects/spells/monsters_effects/league_legion/rewardsystem",
        "metadata/effects/spells/monsters_effects/league_blight/rewardsystem",
        "metadata/effects/spells/monsters_effects/league_archnemesis",
        "metadata/effects/spells/monsters_effects/league_ritual/cold_ritual",
        "metadata/effects/spells/monsters_effects/league_ultimatum/mechanics/fx/arena_limit.pet",
        "metadata/effects/spells/monsters_effects/league_sanctum",
        "metadata/effects/spells/monsters_effects/league_hellscape/mechanics",
        "metadata/effects/spells/monsters_effects/atlasofworldsbosses/maven",
        "metadata/effects/spells/monsters_effects/atlasexiles/adjudicator",
        "metadata/effects/spells/ground_effects/chimera_smoke",
        "metadata/effects/spells/ground_effects/evil",
        "metadata/effects/spells/ground_effects_v2/smoke_blind_chimera",
        "metadata/effects/spells/monsters_effects/atlasofworldsbosses/chimera",
        "metadata/effects/spells/monsters_effects/atlasexiles/orion",
        "metadata/effects/spells/monsters_effects/prophecy_league",
        "metadata/effects/spells/ground_effects/caustic",
        "metadata/effects/spells/ground_effects_v2/caustic_arrow_ground",
        "metadata/effects/spells/ground_effects_v2/desecrated",
        "metadata/effects/spells/ground_effects_v2/desecrated_maligaro",
        "metadata/effects/spells/ground_effects_v2/desecrated_red",
        "metadata/effects/spells/ground_effects_v3/caustic",
    ];
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    if protected
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
    {
        return Ok(bytes.to_vec());
    }
    let keep: HashSet<&str> = [
        "ClientAnimationController",
        "SoundEvents",
        "BoneGroups",
        "AnimatedRender",
        "SkinMesh",
    ]
    .into_iter()
    .collect();
    let text = decode_utf16(bytes)?;
    Ok(encode_utf16_bom(&strip_client_blocks(&text, &keep)))
}

fn replace_utf16(bytes: &[u8], replacements: &[(&str, &str)]) -> Result<Vec<u8>> {
    let mut text = decode_utf16(bytes)?;
    for (from, to) in replacements {
        text = text.replace(from, to);
    }
    Ok(encode_utf16_bom(&text))
}

fn regex_utf16(bytes: &[u8], pattern: &str, replacement: &str) -> Result<Vec<u8>> {
    let text = decode_utf16(bytes)?;
    let regex = Regex::new(pattern)?;
    Ok(encode_utf16_bom(&regex.replace_all(&text, replacement)))
}

fn decode_utf16(bytes: &[u8]) -> Result<String> {
    if bytes.len() % 2 != 0 {
        bail!("UTF-16 file has odd byte length");
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    let text = String::from_utf16(&units)?;
    Ok(text.trim_start_matches('\u{feff}').to_string())
}

fn encode_utf16_bom(text: &str) -> Vec<u8> {
    let mut out = vec![0xff, 0xfe];
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

fn replace_array_property(mut data: String, property: &str) -> String {
    let pattern = format!("\"{property}\":");
    let Some(index) = data.find(&pattern) else {
        return data;
    };
    let Some(bracket_start_rel) = data[index..].find('[') else {
        return data;
    };
    let bracket_start = index + bracket_start_rel;
    let mut depth = 1;
    let mut end = bracket_start + 1;
    let chars: Vec<char> = data.chars().collect();
    while end < chars.len() && depth > 0 {
        match chars[end] {
            '[' => depth += 1,
            ']' => depth -= 1,
            _ => {}
        }
        end += 1;
    }
    if depth == 0 {
        if let Some(comma_rel) = data[end - 1..].find(',') {
            let comma = end - 1 + comma_rel;
            if comma < end + 5 {
                data.replace_range(index..=comma, &format!("\"{property}\": [],"));
            }
        }
    }
    data
}

fn remove_function_calls(data: &str, func: &str) -> String {
    let mut data = data.to_string();
    let mut pos = 0;
    while let Some(found) = data[pos..].find(func) {
        let found = pos + found;
        let mut start = found;
        while start > 0 {
            let ch = data.as_bytes()[start - 1] as char;
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
                start -= 1;
            } else {
                break;
            }
        }
        let mut paren = found + func.len();
        while paren < data.len() && data.as_bytes()[paren].is_ascii_whitespace() {
            paren += 1;
        }
        if paren >= data.len() || data.as_bytes()[paren] != b'(' {
            pos = found + 1;
            continue;
        }
        let mut depth = 1;
        let mut end = paren + 1;
        while end < data.len() && depth > 0 {
            match data.as_bytes()[end] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            end += 1;
        }
        while end < data.len() && data.as_bytes()[end].is_ascii_whitespace() {
            end += 1;
        }
        if depth == 0 && end < data.len() && data.as_bytes()[end] == b';' {
            data.replace_range(start..end + 1, "");
            pos = start;
        } else {
            pos = found + 1;
        }
    }
    data
}

fn skip_syntax(text: &str, i: usize) -> usize {
    let bytes = text.as_bytes();
    match bytes[i] {
        b'"' => {
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == b'\\' && j + 1 < bytes.len() {
                    j += 2;
                    continue;
                }
                if bytes[j] == b'"' {
                    return j + 1;
                }
                j += 1;
            }
            bytes.len()
        }
        b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => text[i + 2..]
            .find('\n')
            .map_or(bytes.len(), |p| i + 2 + p + 1),
        b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => text[i + 2..]
            .find("*/")
            .map_or(bytes.len(), |p| i + 2 + p + 2),
        b'[' => {
            let mut depth = 1;
            let mut j = i + 1;
            while j < bytes.len() && depth > 0 {
                if bytes[j] == b'"'
                    || (bytes[j] == b'/'
                        && j + 1 < bytes.len()
                        && (bytes[j + 1] == b'/' || bytes[j + 1] == b'*'))
                {
                    j = skip_syntax(text, j);
                    continue;
                }
                if bytes[j] == b'[' {
                    depth += 1;
                } else if bytes[j] == b']' {
                    depth -= 1;
                }
                j += 1;
            }
            j
        }
        _ => i + 1,
    }
}

fn find_matching_brace(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 1;
    let mut i = open + 1;
    while i < bytes.len() {
        if bytes[i] == b'"'
            || bytes[i] == b'['
            || (bytes[i] == b'/'
                && i + 1 < bytes.len()
                && (bytes[i + 1] == b'/' || bytes[i + 1] == b'*'))
        {
            i = skip_syntax(text, i);
            continue;
        }
        if bytes[i] == b'{' {
            depth += 1;
        } else if bytes[i] == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn find_next_sub_block(text: &str, from: usize) -> Option<(usize, String, usize)> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'"'
            || bytes[i] == b'['
            || (bytes[i] == b'/'
                && i + 1 < bytes.len()
                && (bytes[i + 1] == b'/' || bytes[i + 1] == b'*'))
        {
            i = skip_syntax(text, i);
            continue;
        }
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let name_end = i;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'{' {
                return Some((start, text[start..name_end].to_string(), i));
            }
            continue;
        }
        i += 1;
    }
    None
}

fn find_top_level_block(text: &str, name: &str) -> Option<usize> {
    let mut pos = 0;
    while let Some((_, block_name, open)) = find_next_sub_block(text, pos) {
        if block_name == name {
            return Some(open);
        }
        let close = find_matching_brace(text, open)?;
        pos = close + 1;
    }
    None
}

fn strip_client_blocks(data: &str, keep: &HashSet<&str>) -> String {
    let Some(client_open) = find_top_level_block(data, "client") else {
        return data.to_string();
    };
    let Some(client_close) = find_matching_brace(data, client_open) else {
        return data.to_string();
    };
    let body = &data[client_open + 1..client_close];
    let mut result = String::new();
    let mut pos = 0;
    while let Some((name_start, name, open)) = find_next_sub_block(body, pos) {
        let Some(close) = find_matching_brace(body, open) else {
            result.push_str(&body[pos..]);
            break;
        };
        result.push_str(&body[pos..name_start]);
        if keep.contains(name.as_str()) {
            result.push_str(&body[name_start..close + 1]);
        }
        pos = close + 1;
    }
    result.push_str(&body[pos..]);
    format!(
        "{}{}{}",
        &data[..client_open + 1],
        result,
        &data[client_close..]
    )
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
    fn effects_keeps_selected_client_blocks() {
        let input =
            "version 3\nclient\n{\n  AnimatedRender\n  {\n  }\n  ParticleEffects\n  {\n  }\n}";
        let keep = ["AnimatedRender"].into_iter().collect();
        let out = strip_client_blocks(input, &keep);
        assert!(out.contains("AnimatedRender"));
        assert!(!out.contains("ParticleEffects"));
    }

    #[test]
    fn changed_replacements_are_built_once_per_target_path() {
        let file = BundleFile::for_test("env.bundle.bin", 6);
        let candidates = vec![(
            "metadata/environmentsettings/test.env".to_string(),
            file.clone(),
        )];
        let mut file_data = BTreeMap::from([(
            "metadata/environmentsettings/test.env".to_string(),
            b"changed".to_vec(),
        )]);
        let changed = BTreeMap::from([("metadata/environmentsettings/test.env".to_string(), true)]);

        let patch_set =
            build_patch_set_from_changed(&candidates, &mut file_data, &changed).unwrap();

        assert_eq!(patch_set.changes.len(), 1);
        assert_eq!(
            patch_set
                .replacements
                .get("env.bundle.bin")
                .map(|entries| entries.len()),
            Some(1)
        );
    }

    #[test]
    fn duplicate_patch_candidates_collapse_to_one_target() {
        let file = BundleFile::for_test("env.bundle.bin", 6);
        let candidates = dedup_candidates(vec![
            (
                "metadata/environmentsettings/test.env".to_string(),
                file.clone(),
            ),
            (
                "metadata/environmentsettings/test.env".to_string(),
                file.clone(),
            ),
        ]);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, "metadata/environmentsettings/test.env");
    }
}
