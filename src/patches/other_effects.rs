//! Shared ground visuals and spell-effect folders without a skill owner.

use super::effect_skills::{
    effect_skill_folder, is_shared_effect_folder, EffectSkillCatalogEntry, SPELLS_PREFIX,
};
use super::targeting::{ends_with_path_ci, normalize_path};
use std::collections::BTreeSet;

/// One independently controlled scope under `metadata/effects/spells/`.
/// Normally a directory; a loose file directly under spells uses its filename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherEffectCatalogEntry {
    pub folder: String,
    pub display: String,
}

pub fn build_other_effect_catalog(
    paths: &[String],
    skills: &[EffectSkillCatalogEntry],
) -> Vec<OtherEffectCatalogEntry> {
    let owned: BTreeSet<_> = skills
        .iter()
        .flat_map(|row| row.folders.iter().cloned())
        .collect();
    let mut folders = BTreeSet::new();
    for path in paths {
        if !(ends_with_path_ci(path, ".ao") || ends_with_path_ci(path, ".aoc")) {
            continue;
        }
        let normalized = normalize_path(path);
        let Some(relative) = normalized.strip_prefix(SPELLS_PREFIX) else {
            continue;
        };
        // Monster-specific assets remain in the Monsters tab. Shared assets
        // referenced by monsters live outside this reserved subtree.
        if relative.starts_with("monsters_effects/") {
            continue;
        }
        if relative
            .match_indices('/')
            .any(|(end, _)| owned.contains(&relative[..end]))
        {
            continue;
        }
        let segments: Vec<_> = relative.split('/').collect();
        let loose_shared_file = (is_shared_effect_folder(relative) || segments[0] == "supports")
            && segments.len() == 2
            || segments.starts_with(&["supports", "runicsupports"]) && segments.len() == 3;
        let folder = if loose_shared_file {
            relative.to_string()
        } else {
            effect_skill_folder(&normalized).unwrap_or_else(|| relative.to_string())
        };
        folders.insert(folder);
    }
    let mut rows: Vec<_> = folders
        .into_iter()
        .map(|folder| {
            let display = display_folder(&folder);
            OtherEffectCatalogEntry { folder, display }
        })
        .collect();
    rows.sort_by(|a, b| {
        a.display
            .cmp(&b.display)
            .then_with(|| a.folder.cmp(&b.folder))
    });
    rows
}

fn display_folder(folder: &str) -> String {
    let (group, child) = folder.split_once('/').unwrap_or((folder, ""));
    match group {
        "ground_effects" | "ground_effects_v2" | "ground_effects_v3" if !child.is_empty() => {
            let generation = match group {
                "ground_effects_v2" => " (v2)",
                "ground_effects_v3" => " (v3)",
                _ => "",
            };
            format!("{} ground{}", humanize(child), generation)
        }
        _ => humanize(folder),
    }
}

fn humanize(value: &str) -> String {
    value
        .split(['_', '/', '\\'])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            let first = chars.next().unwrap();
            first.to_uppercase().chain(chars).collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn others_separates_ground_types_and_excludes_skill_and_monster_scopes() {
        let paths: Vec<_> = [
            "Metadata/Effects/Spells/ground_effects_v3/burning/burning_grd.ao",
            "metadata/effects/spells/ground_effects_v3/burning/fade_in_burst.ao",
            "metadata/effects/spells/ground_effects_v3/shocked/shock.ao",
            "metadata/effects/spells/new_shared/fx/effect.aoc",
            "metadata/effects/spells/loose.ao",
            "metadata/effects/spells/ground_effects_v3/helper.ao",
            "metadata/effects/spells/supports/helper.ao",
            "metadata/effects/spells/supports/runicsupports/helper.ao",
            "metadata/effects/spells/cold_arcticarmour/arcticarmor.ao",
            "metadata/effects/spells/monsters_effects/boss/attack.ao",
            "metadata/effects/spells/particles_only/burst.pet",
            "metadata/effects/spells/supports/runicsupports/known/rig.ao",
            "metadata/effects/spells/supports/runicsupports/unknown/rig.ao",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let skills = vec![EffectSkillCatalogEntry {
            active_skill_id: "arctic".into(),
            display: "Arctic Armour".into(),
            action_type: "Arctic".into(),
            folders: vec![
                "cold_arcticarmour".into(),
                "supports/runicsupports/known".into(),
            ],
        }];
        let rows = build_other_effect_catalog(&paths, &skills);
        let folders: BTreeSet<_> = rows.iter().map(|r| r.folder.as_str()).collect();
        assert_eq!(
            folders,
            BTreeSet::from([
                "ground_effects_v3/burning",
                "ground_effects_v3/shocked",
                "new_shared",
                "loose.ao",
                "ground_effects_v3/helper.ao",
                "supports/helper.ao",
                "supports/runicsupports/helper.ao",
                "supports/runicsupports/unknown"
            ])
        );
        assert_eq!(
            rows.iter()
                .find(|r| r.folder == "ground_effects_v3/burning")
                .unwrap()
                .display,
            "Burning ground (v3)"
        );
    }
}
