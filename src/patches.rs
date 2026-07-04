use crate::bundle::{slice_file, BundleFile, BundleIndex, BundleStore};
use anyhow::{anyhow, bail, Context, Result};
use rayon::prelude::*;
use regex::Regex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::LazyLock;

static RAIN_INTENSITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"("rain_intensity":\s*)([^,\r\n}]+)(,?)"#).unwrap());
static CLOUDS_INTENSITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"("clouds_intensity":\s*)([^,\r\n}]+)(,?)"#).unwrap());
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
static PARTICLE_PROTECTED_PREFIXES: &[&str] = &[
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
static EFFECT_PROTECTED_PREFIXES: &[&str] = &[
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
static STARTUP_SCENE_PROTECTED_PREFIXES: &[&str] = &[
    "metadata/terrain/characterselection",
    "metadata/environment/characterselection",
    "metadata/doodads/characterselection",
    "metadata/materials/characterselection",
    "metadata/effects/characterselection",
];

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
    DisableSounds,
    SkillSounds,
    MonsterSounds,
    MtxSoft,
}

#[derive(Debug, Clone)]
pub struct PatchInfo {
    pub id: PatchId,
    pub name: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone)]
pub struct PresetInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub patches: &'static [PatchId],
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
        PatchInfo {
            id: PatchId::DisableSounds,
            name: "disable-sounds",
            description: "Silence sounds by emptying SoundEvents/SoundParams blocks.",
        },
        PatchInfo {
            id: PatchId::SkillSounds,
            name: "skill-sounds",
            description: "Silence skill-effect sounds (empty SoundEvents/SoundParams).",
        },
        PatchInfo {
            id: PatchId::MonsterSounds,
            name: "monster-sounds",
            description: "Silence monster sounds (empty SoundEvents/SoundParams).",
        },
        PatchInfo {
            id: PatchId::MtxSoft,
            name: "mtx-soft",
            description: "Blank microtransaction effect/particle files.",
        },
    ]
}

pub fn all_presets() -> &'static [PresetInfo] {
    &[
        PresetInfo {
            name: "maps-revealed",
            description: "Reveal minimap and Atlas fog.",
            patches: &[PatchId::Minimap, PatchId::AtlasFog],
        },
        PresetInfo {
            name: "performance",
            description: "Balanced visual cleanup for performance.",
            patches: &[
                PatchId::Fog,
                PatchId::Rain,
                PatchId::Clouds,
                PatchId::EnvParticles,
                PatchId::Delirium,
                PatchId::Particles,
                PatchId::Effects,
            ],
        },
        PresetInfo {
            name: "optimal",
            description: "Safe recommended mix of map and environment patches.",
            patches: &[
                PatchId::Minimap,
                PatchId::AtlasFog,
                PatchId::Fog,
                PatchId::Rain,
                PatchId::Clouds,
                PatchId::EnvParticles,
                PatchId::Effects,
            ],
        },
        PresetInfo {
            name: "daylight",
            description: "Remove darkness, fog, shadows, and heavy environment particles.",
            patches: &[
                PatchId::Fog,
                PatchId::Shadow,
                PatchId::Light,
                PatchId::EnvParticles,
                PatchId::Delirium,
            ],
        },
        PresetInfo {
            name: "high-performance",
            description:
                "Aggressive performance preset with effects, particles, sounds, and MTX reduced.",
            patches: &[
                PatchId::Fog,
                PatchId::Rain,
                PatchId::Clouds,
                PatchId::EnvParticles,
                PatchId::Delirium,
                PatchId::Particles,
                PatchId::Effects,
                PatchId::DisableSounds,
                PatchId::MtxSoft,
            ],
        },
        PresetInfo {
            name: "check-all",
            description: "Select every ported patch.",
            patches: &[
                PatchId::Camera,
                PatchId::Minimap,
                PatchId::AtlasFog,
                PatchId::Fog,
                PatchId::Rain,
                PatchId::Clouds,
                PatchId::EnvParticles,
                PatchId::Shadow,
                PatchId::Light,
                PatchId::Delirium,
                PatchId::Particles,
                PatchId::Effects,
                PatchId::DisableSounds,
                PatchId::SkillSounds,
                PatchId::MonsterSounds,
                PatchId::MtxSoft,
            ],
        },
    ]
}

pub fn parse_patch(name: &str) -> Option<PatchId> {
    if name.eq_ignore_ascii_case("zero-particles") {
        return Some(PatchId::Particles);
    }
    all_patches()
        .iter()
        .find(|patch| patch.name.eq_ignore_ascii_case(name))
        .map(|patch| patch.id)
}

pub fn parse_preset(name: &str) -> Option<&'static PresetInfo> {
    all_presets()
        .iter()
        .find(|preset| preset.name.eq_ignore_ascii_case(name))
}

pub fn patch_info(id: PatchId) -> Option<&'static PatchInfo> {
    all_patches().iter().find(|patch| patch.id == id)
}

pub fn compute_patch_set(
    store: &BundleStore,
    index: &mut BundleIndex,
    patches: &[PatchId],
    zoom: f64,
) -> Result<PatchSet> {
    crate::timing!("patch_scan_compute");

    let patches = unique_patches(patches);
    let candidates = collect_patch_targets(index, &patches)?;
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
    let transformed = candidates
        .par_iter()
        .map(|(path, _)| -> Result<(String, Vec<u8>, bool)> {
            let mut bytes = file_data
                .get(path)
                .ok_or_else(|| anyhow!("patch target bytes missing after read: {path}"))?
                .clone();
            let mut changed = false;
            for &patch in &patches {
                if patch_applies_path(patch, path) {
                    let after = transform(patch, path, &bytes, zoom)?;
                    if after != bytes {
                        bytes = after;
                        changed = true;
                    }
                }
            }
            Ok((path.clone(), bytes, changed))
        })
        .collect::<Vec<_>>()
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    build_patch_set_from_transformed(&candidates, transformed)
}

fn collect_patch_targets(
    index: &mut BundleIndex,
    patches: &[PatchId],
) -> Result<Vec<(String, BundleFile)>> {
    let patches = unique_patches(patches);
    let mut targets: HashMap<PatchId, Vec<(String, BundleFile)>> = patches
        .iter()
        .copied()
        .map(|patch| (patch, Vec::new()))
        .collect();

    let mut broad_patches = Vec::new();
    for &patch in &patches {
        let exact_targets = exact_patch_targets(patch);
        if exact_targets.is_empty() {
            broad_patches.push(patch);
            continue;
        }
        for path in exact_targets {
            if let Some(file) = index.file_by_path(path).cloned() {
                targets
                    .entry(patch)
                    .or_default()
                    .push(((*path).to_string(), file));
            }
        }
    }

    if !broad_patches.is_empty() {
        for entry in index.matching_paths_by(|path| {
            broad_patches
                .iter()
                .any(|patch| patch_targets_path(*patch, path))
        })? {
            for patch in &broad_patches {
                if patch_targets_path(*patch, &entry.path) {
                    targets
                        .entry(*patch)
                        .or_default()
                        .push((entry.path.clone(), entry.file.clone()));
                }
            }
        }
    }

    let mut candidates = Vec::new();
    for &patch in &patches {
        let patch_targets = targets.remove(&patch).unwrap_or_default();
        if patch_targets.is_empty() {
            bail!(
                "patch '{}' has no matching files in this game version;\n\
                 verify game files or wait for a tiny-poe2smoother update",
                patch_label(patch)
            );
        }
        candidates.extend(patch_targets);
    }
    Ok(candidates)
}

fn unique_patches(patches: &[PatchId]) -> Vec<PatchId> {
    let mut selected = HashSet::new();
    let mut ordered_unique = Vec::new();
    for patch in patches {
        if selected.insert(*patch) {
            ordered_unique.push(*patch);
        }
    }
    ordered_unique
}

fn build_patch_set_from_transformed(
    candidates: &[(String, BundleFile)],
    transformed: Vec<(String, Vec<u8>, bool)>,
) -> Result<PatchSet> {
    let mut changes = Vec::new();
    let mut replacements: HashMap<String, Vec<(BundleFile, Vec<u8>)>> = HashMap::new();
    for ((candidate_path, file), (path, bytes, changed)) in candidates.iter().zip(transformed) {
        if candidate_path != &path {
            bail!("transformed patch target order mismatch: {candidate_path} != {path}");
        }
        if changed {
            changes.push(PatchChange {
                path,
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

#[cfg(test)]
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

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

fn exact_patch_targets(patch: PatchId) -> &'static [&'static str] {
    match patch {
        PatchId::Minimap => &[
            "shaders/minimap_visibility_pixel.hlsl",
            "shaders/minimap_blending_pixel.hlsl",
        ],
        PatchId::AtlasFog => &["metadata/materials/environment/worldmap/worldmap_fogofwar.fxgraph"],
        _ => &[],
    }
}

fn patch_targets_path(patch: PatchId, path: &str) -> bool {
    match patch {
        PatchId::Camera => {
            starts_with_path_ci(path, "metadata/")
                && (ends_with_path_ci(path, ".ot") || ends_with_path_ci(path, ".otc"))
        }
        PatchId::Minimap | PatchId::AtlasFog => exact_patch_targets(patch)
            .iter()
            .any(|target| eq_path_ci(path, target)),
        PatchId::Fog
        | PatchId::Rain
        | PatchId::Clouds
        | PatchId::EnvParticles
        | PatchId::Shadow
        | PatchId::Light => {
            starts_with_path_ci(path, "metadata/environmentsettings")
                && ends_with_path_ci(path, ".env")
        }
        PatchId::Delirium => {
            starts_with_path_ci(path, "metadata/effects/environment/league_affliction")
                && (ends_with_path_ci(path, ".ao") || ends_with_path_ci(path, ".aoc"))
        }
        PatchId::Particles => {
            starts_with_path_ci(path, "metadata/particles")
                && (ends_with_path_ci(path, ".pet") || ends_with_path_ci(path, ".trl"))
        }
        PatchId::Effects => {
            starts_with_path_ci(path, "metadata/effects/spells")
                && (ends_with_path_ci(path, ".aoc") || ends_with_path_ci(path, ".ao"))
        }
        PatchId::DisableSounds => is_sound_target(path),
        PatchId::SkillSounds => {
            starts_with_path_ci(path, "metadata/effects/spells")
                && !starts_with_path_ci(path, "metadata/effects/spells/monsters_effects")
                && is_metadata_anim_ext(path)
        }
        PatchId::MonsterSounds => {
            (starts_with_path_ci(path, "metadata/effects/spells/monsters_effects")
                && is_metadata_anim_ext(path))
                || (starts_with_path_ci(path, "metadata/monsters") && is_metadata_anim_ext(path))
        }
        PatchId::MtxSoft => {
            starts_with_path_ci(path, "metadata/effects/microtransactions")
                && is_metadata_effect_ext(path)
        }
    }
}

fn patch_applies_path(patch: PatchId, path: &str) -> bool {
    match patch {
        PatchId::Camera => ends_with_path_ci(path, ".ot") || ends_with_path_ci(path, ".otc"),
        PatchId::Minimap => {
            ends_with_path_ci(path, "minimap_visibility_pixel.hlsl")
                || ends_with_path_ci(path, "minimap_blending_pixel.hlsl")
        }
        PatchId::AtlasFog => eq_path_ci(
            path,
            "metadata/materials/environment/worldmap/worldmap_fogofwar.fxgraph",
        ),
        PatchId::Fog
        | PatchId::Rain
        | PatchId::Clouds
        | PatchId::EnvParticles
        | PatchId::Shadow
        | PatchId::Light => {
            starts_with_path_ci(path, "metadata/environmentsettings")
                && ends_with_path_ci(path, ".env")
        }
        PatchId::Delirium => {
            starts_with_path_ci(path, "metadata/effects/environment/league_affliction")
                && (ends_with_path_ci(path, ".ao") || ends_with_path_ci(path, ".aoc"))
        }
        PatchId::Particles => {
            starts_with_path_ci(path, "metadata/particles")
                && (ends_with_path_ci(path, ".pet") || ends_with_path_ci(path, ".trl"))
        }
        PatchId::Effects => {
            starts_with_path_ci(path, "metadata/effects/spells")
                && (ends_with_path_ci(path, ".aoc") || ends_with_path_ci(path, ".ao"))
        }
        PatchId::DisableSounds => is_sound_target(path),
        PatchId::SkillSounds => {
            starts_with_path_ci(path, "metadata/effects/spells")
                && !starts_with_path_ci(path, "metadata/effects/spells/monsters_effects")
                && is_metadata_anim_ext(path)
        }
        PatchId::MonsterSounds => {
            (starts_with_path_ci(path, "metadata/effects/spells/monsters_effects")
                && is_metadata_anim_ext(path))
                || (starts_with_path_ci(path, "metadata/monsters") && is_metadata_anim_ext(path))
        }
        PatchId::MtxSoft => {
            starts_with_path_ci(path, "metadata/effects/microtransactions")
                && is_metadata_effect_ext(path)
        }
    }
}

fn is_metadata_effect_ext(path: &str) -> bool {
    ends_with_path_ci(path, ".ao")
        || ends_with_path_ci(path, ".aoc")
        || ends_with_path_ci(path, ".pet")
        || ends_with_path_ci(path, ".epk")
        || ends_with_path_ci(path, ".trl")
}

/// Animation/script files that can carry `SoundEvents`/`SoundParams` blocks.
/// Sound removal only ever touches these (never `.pet`/`.epk`/`.trl`, which are
/// particle/effect data emptied by other patches).
fn is_metadata_anim_ext(path: &str) -> bool {
    ends_with_path_ci(path, ".ao")
        || ends_with_path_ci(path, ".aoc")
        || ends_with_path_ci(path, ".ot")
        || ends_with_path_ci(path, ".otc")
}

fn is_sound_target(path: &str) -> bool {
    if is_startup_scene_protected(path) {
        return false;
    }
    ((starts_with_path_ci(path, "metadata/effects")
        || starts_with_path_ci(path, "metadata/characters")
        || starts_with_path_ci(path, "metadata/monsters")
        || starts_with_path_ci(path, "metadata/terrain")
        || starts_with_path_ci(path, "metadata/environment"))
        && !starts_with_path_ci(path, "metadata/environmentsettings"))
        && is_metadata_anim_ext(path)
}

fn is_startup_scene_protected(path: &str) -> bool {
    let normalized = normalize_path(path);
    // Character-selection / startup-scene assets appear under several spellings in
    // the real index, confirmed by diffing the game's `_.index.bin`:
    //   - `metadata/terrain/characterselection/...` (concatenated, prefix form)
    //   - `.../gallowscharacterselection/...` and `.../characterselectiongallows/...`
    //     (concatenated, embedded mid-path)
    //   - `metadata/effects/misc/char_selection/...` (underscore form, e.g.
    //     `dexintfour_fxtest.ao`, which previously slipped through and crashed)
    // Match both spellings as substrings so no variant is missed; this is a pure
    // safety net (it can only leave a startup asset untouched, never corrupt one).
    normalized.contains("characterselection")
        || normalized.contains("char_selection")
        || STARTUP_SCENE_PROTECTED_PREFIXES
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
}

fn eq_path_ci(path: &str, pattern: &str) -> bool {
    path.len() == pattern.len()
        && path
            .bytes()
            .zip(pattern.bytes())
            .all(|(a, b)| path_byte_eq(a, b))
}

fn starts_with_path_ci(path: &str, prefix: &str) -> bool {
    path.len() >= prefix.len()
        && path
            .bytes()
            .zip(prefix.bytes())
            .all(|(a, b)| path_byte_eq(a, b))
}

fn ends_with_path_ci(path: &str, suffix: &str) -> bool {
    if path.len() < suffix.len() {
        return false;
    }
    path.as_bytes()[path.len() - suffix.len()..]
        .iter()
        .copied()
        .zip(suffix.bytes())
        .all(|(a, b)| path_byte_eq(a, b))
}

fn path_byte_eq(a: u8, b: u8) -> bool {
    let a = if a == b'\\' {
        b'/'
    } else {
        a.to_ascii_lowercase()
    };
    let b = if b == b'\\' {
        b'/'
    } else {
        b.to_ascii_lowercase()
    };
    a == b
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
    Some(transform(patch, path, bytes, 2.4))
}

fn transform(patch: PatchId, path: &str, bytes: &[u8], zoom: f64) -> Result<Vec<u8>> {
    match patch {
        PatchId::Camera => camera(path, bytes, zoom),
        PatchId::Minimap => minimap(path, bytes),
        PatchId::AtlasFog => atlas_fog(bytes),
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

fn replace_utf16(bytes: &[u8], replacements: &[(&str, &str)]) -> Result<Vec<u8>> {
    let mut text = decode_utf16(bytes)?;
    for (from, to) in replacements {
        text = text.replace(from, to);
    }
    Ok(encode_utf16_bom(&text))
}

fn regex_utf16(bytes: &[u8], regex: &Regex, replacement: &str) -> Result<Vec<u8>> {
    let text = decode_utf16(bytes)?;
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

fn decode_utf16_lossless(bytes: &[u8]) -> Result<Option<String>> {
    if bytes.len() % 2 != 0 {
        return Ok(None);
    }
    match decode_utf16(bytes) {
        Ok(text) => Ok(Some(text)),
        Err(_) => Ok(None),
    }
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

/// Replace the body of every block whose name is in `names` with `{}`, in place,
/// at any nesting depth, leaving all surrounding bytes untouched. Used for sound
/// removal: `SoundEvents { ... }` -> `SoundEvents {}`.
fn empty_named_blocks(data: &str, names: &HashSet<&str>) -> String {
    let mut out = data.to_string();
    let mut pos = 0;
    while let Some((name_start, name, open)) = find_next_sub_block(&out, pos) {
        let Some(close) = find_matching_brace(&out, open) else {
            break;
        };
        // Only collapse blocks that actually carry content; leave already-empty
        // (whitespace-only) bodies verbatim, matching the captured output.
        if names.contains(name.as_str()) && !out[open + 1..close].trim().is_empty() {
            let replacement = format!("{name} {{}}");
            let advance = name_start + replacement.len();
            out.replace_range(name_start..close + 1, &replacement);
            // Resume past the emptied block so we never re-match it (infinite loop).
            pos = advance;
        } else {
            pos = open + 1;
        }
    }
    out
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

    #[test]
    fn duplicate_selected_patches_do_not_create_missing_target_errors() {
        let mut index =
            BundleIndex::for_test_paths(&[("metadata/environmentsettings/test.env", "env", 12)]);

        let candidates = collect_patch_targets(&mut index, &[PatchId::Fog, PatchId::Fog]).unwrap();
        let candidates = dedup_candidates(candidates);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, "metadata/environmentsettings/test.env");
    }

    #[test]
    fn broad_environment_patches_share_one_target_scan_result() {
        let mut index = BundleIndex::for_test_paths(&[
            ("metadata/environmentsettings/test.env", "env", 12),
            ("metadata/environmentsettings/ignored.txt", "env", 12),
        ]);

        let candidates = collect_patch_targets(
            &mut index,
            &[
                PatchId::Fog,
                PatchId::Rain,
                PatchId::Clouds,
                PatchId::EnvParticles,
                PatchId::Shadow,
                PatchId::Light,
            ],
        )
        .unwrap();
        let candidates = dedup_candidates(candidates);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, "metadata/environmentsettings/test.env");
    }

    #[test]
    fn direct_patch_targets_must_match_exact_paths() {
        let mut index = BundleIndex::for_test_paths(&[
            ("shaders/minimap_visibility_pixel.hlsl", "shader", 12),
            ("x/shaders/minimap_blending_pixel.hlsl", "shader", 12),
            (
                "metadata/materials/environment/worldmap/worldmap_fogofwar.fxgraph",
                "atlas",
                12,
            ),
        ]);

        let candidates =
            collect_patch_targets(&mut index, &[PatchId::Minimap, PatchId::AtlasFog]).unwrap();
        let paths = candidates
            .into_iter()
            .map(|(path, _)| path)
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec![
                "shaders/minimap_visibility_pixel.hlsl",
                "metadata/materials/environment/worldmap/worldmap_fogofwar.fxgraph",
            ]
        );
    }

    #[test]
    fn missing_selected_patch_target_still_errors() {
        let mut index =
            BundleIndex::for_test_paths(&[("metadata/environmentsettings/test.env", "env", 12)]);

        let err = collect_patch_targets(&mut index, &[PatchId::Minimap]).unwrap_err();

        assert!(err
            .to_string()
            .contains("patch 'minimap' has no matching files"));
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
            bytes = transform(patch, "metadata/environmentsettings/test.env", &bytes, 2.4).unwrap();
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
    fn capture_driven_patches_discover_expected_path_families() {
        let mut index = BundleIndex::for_test_paths(&[
            (
                "metadata/effects/spells/fireball/fireball.ao",
                "effects",
                12,
            ),
            (
                "metadata/effects/spells/monsters_effects/boss/roar.aoc",
                "monster",
                12,
            ),
            (
                "metadata/effects/microtransactions/portal/portal.pet",
                "mtx",
                12,
            ),
            ("metadata/monsters/foo/bar.ot", "monster", 12),
            // particle data under a skill dir: NOT a sound target.
            (
                "metadata/effects/spells/fireball/fireball.pet",
                "effects",
                12,
            ),
        ]);

        let candidates = collect_patch_targets(
            &mut index,
            &[
                PatchId::SkillSounds,
                PatchId::MonsterSounds,
                PatchId::MtxSoft,
            ],
        )
        .unwrap();
        let paths = dedup_candidates(candidates)
            .into_iter()
            .map(|(path, _)| path)
            .collect::<Vec<_>>();

        assert!(paths.contains(&"metadata/effects/spells/fireball/fireball.ao".to_string()));
        assert!(
            paths.contains(&"metadata/effects/spells/monsters_effects/boss/roar.aoc".to_string())
        );
        assert!(paths.contains(&"metadata/effects/microtransactions/portal/portal.pet".to_string()));
        assert!(paths.contains(&"metadata/monsters/foo/bar.ot".to_string()));
        // sound patches must not grab particle .pet files (that is the particles
        // patch's job); only .ao/.aoc/.ot/.otc are sound targets.
        assert!(!paths.contains(&"metadata/effects/spells/fireball/fireball.pet".to_string()));
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
            2.4,
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
            2.4,
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
            2.4,
        )
        .unwrap();
        assert_eq!(epk, vec![0xff, 0xfe]);
        // .pet/.trl become BOM+"0" (tolerated by the engine).
        for ext in ["pet", "trl"] {
            let out = transform(
                PatchId::MtxSoft,
                &format!("metadata/effects/microtransactions/portal/portal.{ext}"),
                &stuff,
                2.4,
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
            2.4,
        )
        .unwrap();
        assert_eq!(ao, ao_in);
    }

    #[test]
    fn startup_scene_protection_matches_character_selection_anywhere_in_path() {
        let input = encode_utf16_bom("version 3\nclient\n{\n\tSoundEvents\n\t{\n\t\tx = 1\n\t}\n}");
        let out = transform(
            PatchId::DisableSounds,
            "metadata/effects/misc/CharacterSelection/gallows.ao",
            &input,
            2.4,
        )
        .unwrap();

        assert_eq!(out, input);
    }

    #[test]
    fn sound_patch_skips_character_selection_assets() {
        let startup_scene =
            "Metadata/Terrain/CharacterSelection/CharacterSelectionGallows/Gallows_MainBuilding_fx.ao";
        let mut index = BundleIndex::for_test_paths(&[
            (startup_scene, "startup", 12),
            ("metadata/terrain/trees/tree.ao", "terrain", 12),
        ]);

        let candidates = collect_patch_targets(&mut index, &[PatchId::DisableSounds]).unwrap();
        let paths = dedup_candidates(candidates)
            .into_iter()
            .map(|(path, _)| path)
            .collect::<Vec<_>>();

        assert!(!paths.contains(&startup_scene.to_string()));
        assert!(paths.contains(&"metadata/terrain/trees/tree.ao".to_string()));
    }

    #[test]
    fn startup_scene_protection_covers_underscore_char_selection_variant() {
        // Confirmed present in the real game index; previously slipped through the
        // `/characterselection` substring check and crashed ("Skin mesh not found"
        // / "Unexpected token 0") because it has the underscore spelling.
        for path in [
            "metadata/effects/misc/char_selection/dexintfour_fxtest.ao",
            "metadata/effects/misc/char_selection/epk/char_body.epk",
            "metadata/effects/misc/char_selection/fx/char_arm.pet",
            "metadata/particles/enviro_effects/gallowscharacterselection/brazier/fire.pet",
        ] {
            assert!(
                is_startup_scene_protected(path),
                "expected protected: {path}"
            );
        }
        assert!(!is_startup_scene_protected(
            "metadata/effects/spells/fireball/fireball.ao"
        ));
    }
}
