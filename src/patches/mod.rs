mod apply;
mod catalog;
mod client_strings;
mod color_mods;
#[doc(hidden)]
pub mod datc64;
mod effect_dependencies;
mod effect_skills;
mod minimap;
mod monster_effects;
mod other_effects;
mod targeting;
mod text;
mod transform;

pub use apply::compute_patch_set;
pub use catalog::{
    PatchChange, PatchId, PatchInfo, PatchOption, PatchOptions, PatchParams, PatchSet, ProfileInfo,
    all_patches, all_profiles, parse_patch, parse_saved_patches, patch_info, patch_options,
};
pub use color_mods::{
    ColorModEntry, PRESET_COLORS, StatCatalogEntry, default_color_mods, display_stat_text,
    merge_with_defaults, parse_stat_catalog,
};
pub use effect_skills::{
    ACTIONTYPES_DATC64_PATH, ACTIVESKILLS_DATC64_PATH, EffectLevel, EffectSkillCatalogEntry,
    EffectSkillOverride, ITEM_VISUAL_EFFECT_DATC64_PATH, build_effect_skill_catalog,
    effect_skill_folders,
};
pub use minimap::MinimapStyle;
pub use monster_effects::{
    MonsterEffectCatalogEntry, MonsterEffectOverride, build_monster_effect_catalog,
};
pub use other_effects::{OtherEffectCatalogEntry, build_other_effect_catalog};
pub(crate) use text::decode_utf16;
pub use transform::audit_transform;
