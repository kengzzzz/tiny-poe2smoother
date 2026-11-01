//! Listing rows reads index paths only, because it runs on every launch.

use super::other_effects::humanize;
use super::targeting::{
    ends_with_path_ci, is_metadata_effect_ext, is_startup_scene_protected, normalize_path,
    starts_with_path_ci,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};

const MTX_PREFIX: &str = "metadata/effects/microtransactions/";
/// These hold many items, so a row sits one level deeper than `category/name`.
const COLLECTIONS: &[&str] = &[
    "spells",
    "weapon_effects/weapon_effects",
    "weapon_effects/uniques",
    "misc/uniques",
    "bodyarmours/wings",
    "aura/uniques",
];
/// `fx` and `epk` would otherwise split one item into rows that reference each other.
const ASSET_FOLDERS: &[&str] = &[
    "fx",
    "epk",
    "epks",
    "pet",
    "pets",
    "trl",
    "fx_ao",
    "particles",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MtxCatalogEntry {
    pub folder: String,
    pub display: String,
    pub context: String,
    /// Folder names often miss the skill, like Twister in The Taming's row.
    pub file_words: String,
}

#[derive(Default)]
struct RowFiles {
    removable: bool,
    words: BTreeSet<String>,
}

/// Compare keys exactly: `foot_prints` holds only loose files, and a prefix match
/// would also keep `foot_prints/man`.
fn mtx_row_key(relative: &str) -> &str {
    let Some((folders, _)) = relative.rsplit_once('/') else {
        return relative;
    };
    let in_collection = COLLECTIONS.iter().any(|collection| {
        folders
            .strip_prefix(collection)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    });
    let depth = if in_collection { 3 } else { 2 };
    let mut end = 0;
    for (position, segment) in folders.split('/').take(depth).enumerate() {
        if position > 0 && ASSET_FOLDERS.contains(&segment) {
            break;
        }
        end += segment.len() + usize::from(position > 0);
    }
    &relative[..end]
}

fn is_blankable(path: &str) -> bool {
    ends_with_path_ci(path, ".pet")
        || ends_with_path_ci(path, ".epk")
        || ends_with_path_ci(path, ".trl")
}

pub fn build_mtx_catalog(paths: &[String]) -> Vec<MtxCatalogEntry> {
    let mut rows: BTreeMap<String, RowFiles> = BTreeMap::new();
    for path in paths {
        if !starts_with_path_ci(path, MTX_PREFIX)
            || !is_metadata_effect_ext(path)
            || is_startup_scene_protected(path)
        {
            continue;
        }
        let normalized = normalize_path(path);
        let relative = &normalized[MTX_PREFIX.len()..];
        let folder = mtx_row_key(relative);
        let row = rows.entry(folder.to_string()).or_default();
        row.removable |= is_blankable(path);
        row.words
            .extend(file_words(&relative[folder.len()..]).map(str::to_string));
    }
    rows.into_iter()
        .filter(|(_, row)| row.removable)
        .map(|(folder, row)| catalog_entry(folder, &row.words))
        .collect()
}

fn file_words(tail: &str) -> impl Iterator<Item = &str> {
    let stem = tail.rsplit_once('.').map_or(tail, |(stem, _)| stem);
    stem.split(['/', '_', '-', '.', ' ']).filter(|word| {
        !word.is_empty()
            && !ASSET_FOLDERS.contains(word)
            && !word.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn catalog_entry(folder: String, words: &BTreeSet<String>) -> MtxCatalogEntry {
    let (parents, name) = folder.rsplit_once('/').unwrap_or(("", &folder));
    let mut context: Vec<_> = parents
        .split('/')
        .filter(|parent| !parent.is_empty())
        .map(humanize)
        .collect();
    context.dedup();
    MtxCatalogEntry {
        display: humanize(name),
        context: context.join(" · "),
        file_words: words
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        folder,
    }
}

pub(super) fn kept_mtx_paths(paths: &[String], kept: &[String]) -> BTreeSet<String> {
    let kept: HashSet<_> = kept.iter().map(String::as_str).collect();
    paths
        .iter()
        .filter(|path| starts_with_path_ci(path, MTX_PREFIX) && is_metadata_effect_ext(path))
        .map(|path| normalize_path(path))
        .filter(|path| kept.contains(mtx_row_key(&path[MTX_PREFIX.len()..])))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(relative: &[&str]) -> Vec<String> {
        relative
            .iter()
            .map(|path| format!("{MTX_PREFIX}{path}"))
            .collect()
    }

    #[test]
    fn rows_stop_at_the_item_in_every_folder_shape() {
        for (path, row) in [
            (
                "misc/uniques/berekthetaming/fx/fire_addon_01_twister.pet",
                "misc/uniques/berekthetaming",
            ),
            (
                "misc/uniques/berekthetaming/elemental_addon_twister_01.ao",
                "misc/uniques/berekthetaming",
            ),
            (
                "spells/witch/chaoscurse/bolt/fx/impact.pet",
                "spells/witch/chaoscurse",
            ),
            ("spells/fireball/fx/shared.pet", "spells/fireball"),
            (
                "weapon_effects/vampire/epks/a.epk",
                "weapon_effects/vampire",
            ),
            ("weapon_effects/vampire/fx/a.pet", "weapon_effects/vampire"),
            (
                "weapon_effects/weapon_effects/blue/fx/a.pet",
                "weapon_effects/weapon_effects/blue",
            ),
            ("bodyarmours/wings/angel/a.pet", "bodyarmours/wings/angel"),
            (
                "bodyarmours/stygianarmor/fx/a.pet",
                "bodyarmours/stygianarmor",
            ),
            (
                "town_portals/miracleportal/fx/a.pet",
                "town_portals/miracleportal",
            ),
            ("foot_prints/shared.pet", "foot_prints"),
            ("foot_prints/man/left.pet", "foot_prints/man"),
            ("loose.pet", "loose.pet"),
        ] {
            assert_eq!(mtx_row_key(path), row, "path: {path}");
        }
    }

    #[test]
    fn collection_names_only_match_whole_segments() {
        assert_eq!(mtx_row_key("spellsmith/a/b/c.pet"), "spellsmith/a");
        assert_eq!(
            mtx_row_key("misc/uniques_extra/a/b.pet"),
            "misc/uniques_extra"
        );
    }

    #[test]
    fn catalog_lists_rows_with_something_to_remove() {
        let rows = build_mtx_catalog(&[
            format!("{MTX_PREFIX}misc/uniques/berekthetaming/fx/fire.pet"),
            "Metadata/Effects/Microtransactions/Misc/Uniques/BerekTheTaming/fx/cold.pet".into(),
            format!("{MTX_PREFIX}spells/witch/chaoscurse/bolt.trl"),
            format!("{MTX_PREFIX}weapon_effects/weapon_effects/blue/a.epk"),
            format!("{MTX_PREFIX}head/animation_only/rig.ao"),
            format!("{MTX_PREFIX}misc/characterselection/fx/a.pet"),
            "metadata/effects/spells/fireball/impact.pet".into(),
        ]);
        let folders: Vec<_> = rows.iter().map(|row| row.folder.as_str()).collect();
        assert_eq!(
            folders,
            [
                "misc/uniques/berekthetaming",
                "spells/witch/chaoscurse",
                "weapon_effects/weapon_effects/blue"
            ]
        );
        assert_eq!(rows[0].display, "Berekthetaming");
        assert_eq!(rows[0].context, "Misc · Uniques");
        assert_eq!(rows[1].context, "Spells · Witch");
        assert_eq!(rows[2].context, "Weapon Effects");
    }

    #[test]
    fn rows_collect_searchable_words_from_their_file_names() {
        let rows = build_mtx_catalog(&paths(&[
            "misc/uniques/berekthetaming/elemental_addon_twister_01.ao",
            "misc/uniques/berekthetaming/fx/fire_addon_01_twister.pet",
            "misc/uniques/berekthetaming/fx/fire_addon_01_twister.mat",
            "spells/witch/chaoscurse/bolt/fx/impact-big.trl",
        ]));

        assert_eq!(rows[0].file_words, "addon elemental fire twister");
        assert_eq!(rows[1].file_words, "big bolt impact");
    }

    #[test]
    fn keeping_a_row_of_loose_files_leaves_the_rows_below_it_alone() {
        let all = paths(&[
            "foot_prints/shared.pet",
            "foot_prints/man/left.pet",
            "foot_prints/man/rig.ao",
        ]);
        assert_eq!(
            kept_mtx_paths(&all, &["foot_prints".to_string()]),
            BTreeSet::from([format!("{MTX_PREFIX}foot_prints/shared.pet")])
        );
    }

    #[test]
    fn keeping_a_row_covers_its_animation_files_and_ignores_stale_keys() {
        let all = paths(&[
            "misc/uniques/berekthetaming/twister.ao",
            "misc/uniques/berekthetaming/fx/fire.pet",
            "misc/uniques/berekthetaming/fx/fire.mat",
            "misc/uniques/other/fx/fire.pet",
        ]);
        let kept = [
            "misc/uniques/berekthetaming".to_string(),
            "removed/by_a_patch".to_string(),
        ];
        assert_eq!(
            kept_mtx_paths(&all, &kept),
            BTreeSet::from([
                format!("{MTX_PREFIX}misc/uniques/berekthetaming/fx/fire.pet"),
                format!("{MTX_PREFIX}misc/uniques/berekthetaming/twister.ao"),
            ])
        );
    }
}
