use super::color_mods::{ColorModEntry, default_color_mods};
use super::effect_skills::EffectSkillOverride;
use super::minimap::MinimapStyle;
use super::monster_effects::MonsterEffectOverride;
use crate::bundle::BundleFile;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PatchId {
    Camera,
    Minimap,
    AtlasFog,
    Atmosphere,
    Shadow,
    Light,
    PostProcess,
    BlackScreen,
    Effects,
    Particles,
    Delirium,
    MtxSoft,
    DisableSounds,
    SkillSounds,
    MonsterSounds,
    ColorMods,
    CompactModLines,
    MonsterHpBar,
}

#[derive(Debug, Clone)]
pub struct PatchInfo {
    pub id: PatchId,
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct PatchOption {
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub default: bool,
    /// Widens the patch, so it doesn't count as a required part.
    pub extra: bool,
}

#[derive(Debug, Clone)]
pub struct ProfileInfo {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub patches: &'static [PatchId],
    pub disabled_options: &'static [(PatchId, &'static str)],
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PatchOptions(BTreeMap<String, bool>);

impl PatchOptions {
    pub const fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub fn enabled(&self, patch: PatchId, key: &str) -> bool {
        self.0
            .get(&option_id(patch, key))
            .copied()
            .unwrap_or_else(|| option_default(patch, key))
    }

    pub fn set(&mut self, patch: PatchId, key: &str, enabled: bool) {
        let id = option_id(patch, key);
        if enabled == option_default(patch, key) {
            self.0.remove(&id);
        } else {
            self.0.insert(id, enabled);
        }
    }

    pub fn any_enabled(&self, patch: PatchId) -> bool {
        let mut parts = patch_options(patch)
            .iter()
            .filter(|option| !option.extra)
            .peekable();
        parts.peek().is_none() || parts.any(|option| self.enabled(patch, option.key))
    }
}

fn option_id(patch: PatchId, key: &str) -> String {
    format!("{}.{key}", patch_label(patch))
}

fn option_default(patch: PatchId, key: &str) -> bool {
    patch_options(patch)
        .iter()
        .find(|option| option.key == key)
        .is_some_and(|option| option.default)
}

#[derive(Debug, Clone)]
pub struct PatchParams {
    pub zoom: f64,
    pub minimap: MinimapStyle,
    pub options: PatchOptions,
    pub color_mods: Vec<ColorModEntry>,
    /// Full scopes from the Skills and Others tabs, which also protect their particles.
    /// The name stays for saved preferences.
    pub effect_skills: Vec<EffectSkillOverride>,
    pub monster_effects: Vec<MonsterEffectOverride>,
}

impl Default for PatchParams {
    fn default() -> Self {
        Self {
            zoom: 2.4,
            minimap: MinimapStyle::default(),
            options: PatchOptions::default(),
            color_mods: default_color_mods(),
            effect_skills: Vec::new(),
            monster_effects: Vec::new(),
        }
    }
}

pub fn all_patches() -> &'static [PatchInfo] {
    &[
        PatchInfo {
            id: PatchId::Camera,
            name: "camera",
            title: "Camera",
            description: "Set a farther default zoom and remove scripted camera zoom and pans.",
        },
        PatchInfo {
            id: PatchId::Minimap,
            name: "minimap",
            title: "Minimap",
            description: "Reveal unexplored minimap terrain and restyle the map overlay.",
        },
        PatchInfo {
            id: PatchId::AtlasFog,
            name: "atlas-fog",
            title: "Atlas fog",
            description: "Remove the Atlas fog of war.",
        },
        PatchInfo {
            id: PatchId::Atmosphere,
            name: "atmosphere",
            title: "Atmosphere",
            description: "Remove fog, weather, and ambient particles from zones.",
        },
        PatchInfo {
            id: PatchId::Shadow,
            name: "shadow",
            title: "Shadows",
            description: "Turn off shadow casting.",
        },
        PatchInfo {
            id: PatchId::Light,
            name: "light",
            title: "Lighting",
            description: "Turn off selected lighting systems.",
        },
        PatchInfo {
            id: PatchId::PostProcess,
            name: "post-process",
            title: "Screen filters",
            description: "Turn off full-screen post-processing filters.",
        },
        PatchInfo {
            id: PatchId::BlackScreen,
            name: "black-screen",
            title: "Black screen",
            description: "Stop drawing the world. UI, item labels, health bars, and the minimap stay.",
        },
        PatchInfo {
            id: PatchId::Effects,
            name: "effects",
            title: "Effects",
            description: "Strip nonessential skill and monster effects.",
        },
        PatchInfo {
            id: PatchId::Particles,
            name: "particles",
            title: "Particles",
            description: "Remove particle effects, including your own skills.",
        },
        PatchInfo {
            id: PatchId::Delirium,
            name: "delirium",
            title: "Delirium",
            description: "Remove the Delirium fog and its props.",
        },
        PatchInfo {
            id: PatchId::MtxSoft,
            name: "mtx-soft",
            title: "MTX effects",
            description: "Remove microtransaction effects and particles.",
        },
        PatchInfo {
            id: PatchId::DisableSounds,
            name: "disable-sounds",
            title: "All effect sounds",
            description: "Silence skill, monster, and environment effect sounds.",
        },
        PatchInfo {
            id: PatchId::SkillSounds,
            name: "skill-sounds",
            title: "Skill sounds",
            description: "Silence skill effect sounds only.",
        },
        PatchInfo {
            id: PatchId::MonsterSounds,
            name: "monster-sounds",
            title: "Monster sounds",
            description: "Silence monster sounds only.",
        },
        PatchInfo {
            id: PatchId::ColorMods,
            name: "color-mods",
            title: "Color mods",
            description: "Color chosen modifier text on waystones, items, and tablets.",
        },
        PatchInfo {
            id: PatchId::CompactModLines,
            name: "compact-mod-lines",
            title: "Compact mod lines",
            description: "Shorten the affix and tier lines on Alt tooltips. English client only.",
        },
        PatchInfo {
            id: PatchId::MonsterHpBar,
            name: "monster-hp-bar",
            title: "Monster health bars",
            description: "Always show monster health bars.",
        },
    ]
}

const fn on(key: &'static str, label: &'static str, description: &'static str) -> PatchOption {
    PatchOption {
        key,
        label,
        description,
        default: true,
        extra: false,
    }
}

const ATMOSPHERE_OPTIONS: &[PatchOption] = &[
    on(
        "fog",
        "Fog",
        "Distance fog that thickens away from the camera.",
    ),
    on(
        "screenspace_fog",
        "Screen fog",
        "Swirling fog layers drawn in front of the camera.",
    ),
    on("rain", "Rain", "Falling rain and its splashes."),
    on("clouds", "Clouds", "Cloud cover moving overhead."),
    on(
        "area",
        "Ground & weather particles",
        "Ground detail and weather that follows the player.",
    ),
    on(
        "effect_spawner",
        "Ambient effects",
        "Embers, souls, and other effects a zone keeps spawning.",
    ),
    on(
        "bloom",
        "Bloom",
        "Glow that bright pixels bleed onto their surroundings.",
    ),
];

const LIGHT_OPTIONS: &[PatchOption] = &[
    on(
        "directional_light",
        "Sun & moon",
        "Directional light and the shadows it casts.",
    ),
    on(
        "player_light",
        "Player light",
        "The glow that follows your character.",
    ),
    on(
        "environment_mapping",
        "Reflections",
        "Cubemap reflections and ambient light from the sky.",
    ),
    on(
        "global_illumination",
        "Global illumination",
        "Ambient occlusion and bounced light.",
    ),
];

const POST_PROCESS_OPTIONS: &[PatchOption] = &[
    on("vignette", "Vignette", "Darkened screen edges."),
    on(
        "depth_of_field",
        "Depth of field",
        "Background and foreground blur.",
    ),
    on(
        "desaturation",
        "Desaturation",
        "The screen washing out when chilled or on low life.",
    ),
];

const COMPACT_MOD_LINES_OPTIONS: &[PatchOption] = &[
    on(
        "affix",
        "Prefix and suffix",
        "\"Prefix Modifier\" becomes \"P\", \"Suffix Modifier\" becomes \"S\".",
    ),
    on(
        "tier",
        "Tier and level",
        "\"(Tier: 2, Lvl: 45)\" becomes \"T2 L45\".",
    ),
];

const MTX_OPTIONS: &[PatchOption] = &[PatchOption {
    key: "town_portals",
    label: "Include town portals",
    description: "Also strip portal MTX. Off keeps portals visible.",
    default: false,
    extra: true,
}];

pub fn patch_options(id: PatchId) -> &'static [PatchOption] {
    match id {
        PatchId::Atmosphere => ATMOSPHERE_OPTIONS,
        PatchId::Light => LIGHT_OPTIONS,
        PatchId::PostProcess => POST_PROCESS_OPTIONS,
        PatchId::CompactModLines => COMPACT_MOD_LINES_OPTIONS,
        PatchId::MtxSoft => MTX_OPTIONS,
        _ => &[],
    }
}

pub fn all_profiles() -> &'static [ProfileInfo] {
    const CLARITY: &[PatchId] = &[
        PatchId::Minimap,
        PatchId::AtlasFog,
        PatchId::MonsterHpBar,
        PatchId::Atmosphere,
        PatchId::PostProcess,
    ];
    const BALANCED: &[PatchId] = &[
        PatchId::Minimap,
        PatchId::AtlasFog,
        PatchId::MonsterHpBar,
        PatchId::Atmosphere,
        PatchId::PostProcess,
        PatchId::Delirium,
        PatchId::Effects,
        PatchId::MtxSoft,
    ];
    const PERFORMANCE: &[PatchId] = &[
        PatchId::Minimap,
        PatchId::AtlasFog,
        PatchId::MonsterHpBar,
        PatchId::Atmosphere,
        PatchId::PostProcess,
        PatchId::Delirium,
        PatchId::Effects,
        PatchId::MtxSoft,
        PatchId::Particles,
        PatchId::Shadow,
        PatchId::Light,
    ];
    &[
        ProfileInfo {
            name: "clarity",
            title: "Clarity",
            description: "Revealed maps and clear sightlines. Skill effects stay untouched.",
            patches: CLARITY,
            disabled_options: &[
                (PatchId::Atmosphere, "area"),
                (PatchId::Atmosphere, "effect_spawner"),
                (PatchId::Atmosphere, "bloom"),
            ],
        },
        ProfileInfo {
            name: "balanced",
            title: "Balanced",
            description: "Clarity plus less effect clutter: ambient particles, Delirium, nonessential effects, and MTX.",
            patches: BALANCED,
            disabled_options: &[],
        },
        ProfileInfo {
            name: "performance",
            title: "Performance",
            description: "Balanced plus no particles, shadows, or lighting systems.",
            patches: PERFORMANCE,
            disabled_options: &[],
        },
    ]
}

impl ProfileInfo {
    pub fn options(&self) -> PatchOptions {
        let mut options = PatchOptions::new();
        for (patch, key) in self.disabled_options {
            options.set(*patch, key, false);
        }
        options
    }

    pub fn matches(&self, selected: &HashSet<PatchId>, options: &PatchOptions) -> bool {
        if selected.len() != self.patches.len()
            || !self.patches.iter().all(|patch| selected.contains(patch))
        {
            return false;
        }
        let expected = self.options();
        self.patches.iter().all(|&patch| {
            patch_options(patch).iter().all(|option| {
                options.enabled(patch, option.key) == expected.enabled(patch, option.key)
            })
        })
    }
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

/// Also migrates the removed fog, rain, clouds, and env-particles patches.
pub fn parse_saved_patches(names: &[String], options: &mut PatchOptions) -> HashSet<PatchId> {
    let mut selected = HashSet::new();
    let mut legacy_layers = BTreeSet::new();
    let mut legacy_all_layers = false;
    for name in names {
        match name.to_ascii_lowercase().as_str() {
            "env-particles" => legacy_all_layers = true,
            layer @ ("fog" | "rain" | "clouds") => {
                legacy_layers.insert(layer.to_string());
            }
            other => selected.extend(parse_patch(other)),
        }
    }
    if legacy_all_layers || !legacy_layers.is_empty() {
        selected.insert(PatchId::Atmosphere);
        for option in patch_options(PatchId::Atmosphere) {
            let enabled = legacy_all_layers || legacy_layers.contains(option.key);
            options.set(PatchId::Atmosphere, option.key, enabled);
        }
    }
    selected
}

pub fn patch_info(id: PatchId) -> &'static PatchInfo {
    all_patches()
        .iter()
        .find(|patch| patch.id == id)
        .expect("every PatchId has a PatchInfo")
}

pub(super) fn patch_label(id: PatchId) -> &'static str {
    patch_info(id).name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn every_patch_id_has_info_and_unique_name() {
        let names: HashSet<_> = all_patches().iter().map(|patch| patch.name).collect();
        assert_eq!(names.len(), all_patches().len());
        for profile in all_profiles() {
            for patch in profile.patches {
                patch_info(*patch);
            }
        }
    }

    #[test]
    fn options_store_only_non_default_values() {
        let mut options = PatchOptions::new();
        options.set(PatchId::Atmosphere, "fog", true);
        options.set(PatchId::MtxSoft, "town_portals", false);
        assert_eq!(options, PatchOptions::new());

        options.set(PatchId::Atmosphere, "bloom", false);
        assert!(!options.enabled(PatchId::Atmosphere, "bloom"));
        assert!(options.enabled(PatchId::Atmosphere, "fog"));
        assert!(!options.enabled(PatchId::MtxSoft, "town_portals"));

        options.set(PatchId::Atmosphere, "bloom", true);
        assert_eq!(options, PatchOptions::new());
    }

    #[test]
    fn any_enabled_is_false_only_when_every_option_is_off() {
        let mut options = PatchOptions::new();
        assert!(options.any_enabled(PatchId::Camera));
        for option in patch_options(PatchId::PostProcess) {
            options.set(PatchId::PostProcess, option.key, false);
        }
        assert!(!options.any_enabled(PatchId::PostProcess));
        assert!(options.any_enabled(PatchId::Light));
        assert!(options.any_enabled(PatchId::MtxSoft));
    }

    #[test]
    fn profile_matches_its_own_selection_and_options_only() {
        let clarity = &all_profiles()[0];
        let selected: HashSet<_> = clarity.patches.iter().copied().collect();
        let options = clarity.options();
        assert!(clarity.matches(&selected, &options));
        assert!(!all_profiles()[1].matches(&selected, &options));

        let mut edited = options.clone();
        edited.set(PatchId::Atmosphere, "bloom", true);
        assert!(!clarity.matches(&selected, &edited));

        let mut extra = selected.clone();
        extra.insert(PatchId::Camera);
        assert!(!clarity.matches(&extra, &options));
    }

    #[test]
    fn options_of_unselected_patches_do_not_affect_profile_match() {
        let balanced = &all_profiles()[1];
        let selected: HashSet<_> = balanced.patches.iter().copied().collect();
        let mut options = balanced.options();
        options.set(PatchId::Light, "player_light", false);
        assert!(balanced.matches(&selected, &options));
    }

    #[test]
    fn saved_env_particles_migrates_to_atmosphere_with_every_layer() {
        let mut options = PatchOptions::new();
        let selected = parse_saved_patches(
            &names(&["env-particles", "fog", "minimap", "unknown"]),
            &mut options,
        );
        assert_eq!(
            selected,
            HashSet::from([PatchId::Atmosphere, PatchId::Minimap])
        );
        for option in patch_options(PatchId::Atmosphere) {
            assert!(options.enabled(PatchId::Atmosphere, option.key));
        }
    }

    #[test]
    fn saved_weather_patches_migrate_to_matching_layers_only() {
        let mut options = PatchOptions::new();
        let selected = parse_saved_patches(&names(&["fog", "rain"]), &mut options);
        assert_eq!(selected, HashSet::from([PatchId::Atmosphere]));
        let enabled: Vec<_> = patch_options(PatchId::Atmosphere)
            .iter()
            .filter(|option| options.enabled(PatchId::Atmosphere, option.key))
            .map(|option| option.key)
            .collect();
        assert_eq!(enabled, ["fog", "rain"]);
    }

    #[test]
    fn saved_names_keep_current_patches_and_aliases() {
        let mut options = PatchOptions::new();
        let selected = parse_saved_patches(
            &names(&["Camera", "zero-particles", "mtx-soft"]),
            &mut options,
        );
        assert_eq!(
            selected,
            HashSet::from([PatchId::Camera, PatchId::Particles, PatchId::MtxSoft])
        );
        assert_eq!(options, PatchOptions::new());
    }
}
