use super::color_mods::{default_color_mods, ColorModEntry};
use crate::bundle::BundleFile;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PatchId {
    Camera,
    Minimap,
    AtlasFog,
    ColorMods,
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
    MonsterHpBar,
    BlackScreen,
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

/// Per-request knobs for parameterized patches (camera zoom level, color-mod
/// assignments). Everything else is fully determined by the patch id.
#[derive(Debug, Clone)]
pub struct PatchParams {
    pub zoom: f64,
    pub color_mods: Vec<ColorModEntry>,
}

impl Default for PatchParams {
    fn default() -> Self {
        Self {
            zoom: 2.4,
            color_mods: default_color_mods(),
        }
    }
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
            id: PatchId::ColorMods,
            name: "color-mods",
            description: "Colorize modifier text on items.",
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
        PatchInfo {
            id: PatchId::MonsterHpBar,
            name: "monster-hp-bar",
            description: "Always show monster HP bars.",
        },
        PatchInfo {
            id: PatchId::BlackScreen,
            name: "black-screen",
            description: "Black out world rendering.",
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
            name: "black-screen",
            description:
                "Black world rendering plus heavy effect removal.",
            patches: &[
                PatchId::BlackScreen,
                PatchId::Fog,
                PatchId::Rain,
                PatchId::Clouds,
                PatchId::EnvParticles,
                PatchId::Shadow,
                PatchId::Light,
                PatchId::Delirium,
                PatchId::Particles,
                PatchId::Effects,
            ],
        },
        PresetInfo {
            name: "check-all",
            description: "Select every ported patch except black-screen.",
            patches: &[
                PatchId::Camera,
                PatchId::Minimap,
                PatchId::AtlasFog,
                PatchId::ColorMods,
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
                PatchId::MonsterHpBar,
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

pub(super) fn patch_label(id: PatchId) -> &'static str {
    all_patches()
        .iter()
        .find(|patch| patch.id == id)
        .map(|patch| patch.name)
        .unwrap_or("unknown")
}
