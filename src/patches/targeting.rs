use super::catalog::PatchId;
use super::effect_skills::{EffectLevel, EffectsFilter};

pub(super) static PARTICLE_PROTECTED_PREFIXES: &[&str] = &[
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
pub(super) static EFFECT_PROTECTED_PREFIXES: &[&str] = &[
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

pub(super) fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

pub(super) fn exact_patch_targets(patch: PatchId) -> &'static [&'static str] {
    match patch {
        PatchId::Minimap => &[
            "shaders/minimap_visibility_pixel.hlsl",
            "shaders/minimap_blending_pixel.hlsl",
        ],
        PatchId::AtlasFog => &["metadata/materials/environment/worldmap/worldmap_fogofwar.fxgraph"],
        PatchId::MonsterHpBar => &["metadata/monsters/monster.ot"],
        // stubs out; killing lighting + the post chain renders the world black
        // while the UI (drawn after post-processing) stays visible.
        PatchId::BlackScreen => &[
            "shaders/postprocessuber.hlsl",
            "shaders/bloomcutoff.hlsl",
            "shaders/bloomgather.hlsl",
            "shaders/blur.hlsl",
            "shaders/copytexture.hlsl",
            "shaders/depthawareblur.hlsl",
            "shaders/gaussianblur.hlsl",
            "shaders/postprocessoutput.hlsl",
            "shaders/screenspaceshadows.hlsl",
            "shaders/include/lighting.hlsl",
            "shaders/include/projection.hlsl",
        ],
        _ => &[],
    }
}

pub(super) fn patch_targets_path(patch: PatchId, path: &str) -> bool {
    match patch {
        PatchId::Camera => {
            starts_with_path_ci(path, "metadata/")
                && (ends_with_path_ci(path, ".ot") || ends_with_path_ci(path, ".otc"))
        }
        PatchId::Minimap | PatchId::AtlasFog | PatchId::MonsterHpBar | PatchId::BlackScreen => {
            exact_patch_targets(patch)
                .iter()
                .any(|target| eq_path_ci(path, target))
        }
        PatchId::ColorMods => is_stat_description_target(path),
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
        PatchId::Effects => effects_targets_path(path, None),
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

/// Filter-aware target predicate for `Effects`. Without a filter this is the
/// long-standing rule (`.ao`/`.aoc` under spells). With one, `Full` folders
/// drop out entirely.
pub(super) fn effects_targets_path(path: &str, filter: Option<&EffectsFilter>) -> bool {
    if !starts_with_path_ci(path, "metadata/effects/spells") {
        return false;
    }
    let is_anim = ends_with_path_ci(path, ".aoc") || ends_with_path_ci(path, ".ao");
    let Some(filter) = filter else {
        return is_anim;
    };
    match filter.level_for(path) {
        EffectLevel::Full => false,
        EffectLevel::Reduced => is_anim,
    }
}

pub(super) fn patch_applies_path(patch: PatchId, path: &str) -> bool {
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
        PatchId::ColorMods => is_stat_description_target(path),
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
        PatchId::Effects => effects_targets_path(path, None),
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
        PatchId::MonsterHpBar => eq_path_ci(path, "metadata/monsters/monster.ot"),
        PatchId::BlackScreen => exact_patch_targets(PatchId::BlackScreen)
            .iter()
            .any(|target| eq_path_ci(path, target)),
    }
}

fn is_stat_description_target(path: &str) -> bool {
    starts_with_path_ci(path, "data/statdescriptions") && ends_with_path_ci(path, ".csd")
}

pub(super) fn is_metadata_effect_ext(path: &str) -> bool {
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

pub(super) fn is_startup_scene_protected(path: &str) -> bool {
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

pub(super) fn starts_with_path_ci(path: &str, prefix: &str) -> bool {
    path.len() >= prefix.len()
        && path
            .bytes()
            .zip(prefix.bytes())
            .all(|(a, b)| path_byte_eq(a, b))
}

pub(super) fn ends_with_path_ci(path: &str, suffix: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn black_screen_targets_only_the_post_processing_shaders() {
        for path in [
            "shaders/postprocessuber.hlsl",
            "Shaders/PostProcessOutput.hlsl",
            "shaders\\include\\lighting.hlsl",
            "shaders/include/projection.hlsl",
        ] {
            assert!(patch_targets_path(PatchId::BlackScreen, path));
            assert!(patch_applies_path(PatchId::BlackScreen, path));
        }
        for path in [
            // No overlap with the minimap patch's shaders.
            "shaders/minimap_visibility_pixel.hlsl",
            "shaders/minimap_blending_pixel.hlsl",
            "shaders/lighting.hlsl",
            "shaders/include/lighting.hlsl.bak",
        ] {
            assert!(!patch_targets_path(PatchId::BlackScreen, path));
            assert!(!patch_applies_path(PatchId::BlackScreen, path));
        }
    }

    #[test]
    fn effects_targeting_without_filter_matches_the_long_standing_rule() {
        for path in [
            "metadata/effects/spells/fireball/fireball.ao",
            "Metadata\\Effects\\Spells\\Arc_02\\beam.aoc",
        ] {
            assert!(effects_targets_path(path, None), "path: {path}");
            assert!(patch_targets_path(PatchId::Effects, path));
        }
        for path in [
            "metadata/effects/spells/fireball/fx/impact.pet",
            "metadata/effects/spells/cold_herald_of_ice/epk/unarmed_buff.epk",
            "metadata/effects/spells/fireball/trail.trl",
            "metadata/particles/enviro/foo.ao",
        ] {
            assert!(!effects_targets_path(path, None), "path: {path}");
            assert!(!patch_targets_path(PatchId::Effects, path));
        }
        assert!(!patch_applies_path(
            PatchId::Effects,
            "metadata/effects/spells/fireball/fx/impact.pet"
        ));
    }

    #[test]
    fn effects_targeting_with_filter_honors_per_skill_levels() {
        use super::super::effect_skills::EffectSkillOverride;

        let filter = EffectsFilter::new(
            &[EffectSkillOverride {
                folder: "cold_herald_of_ice".to_string(),
                level: EffectLevel::Full,
            }],
            Default::default(),
        )
        .unwrap();
        let filter = Some(&filter);

        // Full folders drop out entirely.
        assert!(!effects_targets_path(
            "metadata/effects/spells/cold_herald_of_ice/ao/ice_explosion.ao",
            filter
        ));
        assert!(effects_targets_path(
            "metadata/effects/spells/arc_02/arc.aoc",
            filter
        ));
        assert!(!effects_targets_path(
            "metadata/effects/spells/arc_02/fx/beam.trl",
            filter
        ));
        // Unlisted folders keep the .ao/.aoc-only rule.
        assert!(effects_targets_path(
            "metadata/effects/spells/fireball/fireball.ao",
            filter
        ));
        assert!(!effects_targets_path(
            "metadata/effects/spells/fireball/fx/impact.pet",
            filter
        ));
    }

    #[test]
    fn monster_hp_bar_targets_only_the_monster_template() {
        for path in [
            "metadata/monsters/monster.ot",
            "Metadata/Monsters/Monster.ot",
            "metadata\\monsters\\monster.ot",
        ] {
            assert!(patch_targets_path(PatchId::MonsterHpBar, path));
            assert!(patch_applies_path(PatchId::MonsterHpBar, path));
        }
        for path in [
            "metadata/monsters/monster.otc",
            "metadata/monsters/bosses/monster.ot",
            "metadata/characters/character.ot",
        ] {
            assert!(!patch_targets_path(PatchId::MonsterHpBar, path));
            assert!(!patch_applies_path(PatchId::MonsterHpBar, path));
        }
    }
}
