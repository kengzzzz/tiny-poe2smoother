mod apply;
mod catalog;
mod color_mods;
mod targeting;
mod text;
mod transform;

pub use apply::compute_patch_set;
pub use catalog::{
    all_patches, all_presets, parse_patch, parse_preset, patch_info, PatchChange, PatchId,
    PatchInfo, PatchParams, PatchSet, PresetInfo,
};
pub use color_mods::{
    default_color_mods, merge_with_defaults, parse_stat_catalog, ColorModEntry, StatCatalogEntry,
    PRESET_COLORS,
};
pub(crate) use text::decode_utf16;
pub use transform::audit_transform;
