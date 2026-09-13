use super::catalog::{patch_label, PatchChange, PatchId, PatchParams, PatchSet};
use super::color_mods::ColorMatcher;
use super::effect_dependencies::resolve_full_effect_dependencies;
use super::effect_skills::EffectsFilter;
use super::monster_effects::resolve_full_monster_effect_paths;
use super::targeting::{
    effects_targets_path, exact_patch_targets, patch_applies_path, patch_targets_path,
};
use super::transform::{transform, TransformCtx};
use crate::bundle::{BundleFile, BundleIndex, BundleStore};
use anyhow::{anyhow, bail, Result};
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

pub fn compute_patch_set(
    store: &BundleStore,
    index: &mut BundleIndex,
    patches: &[PatchId],
    params: &PatchParams,
) -> Result<PatchSet> {
    crate::timing!("patch_scan_compute");

    let patches = unique_patches(patches);
    let color_matcher = ColorMatcher::new(&params.color_mods);
    // Per-monster exclusions are resolved against the live index, but only
    // when Effects or Particles is selected and Full monster overrides exist — the
    // default path stays free of extra reads and byte-identical.
    let protects_visuals =
        patches.contains(&PatchId::Effects) || patches.contains(&PatchId::Particles);
    let full_monster_paths = if protects_visuals {
        resolve_full_monster_effect_paths(store, index, &params.monster_effects)?
    } else {
        BTreeSet::new()
    };
    let mut effects_filter = EffectsFilter::new(&params.effect_skills, full_monster_paths);
    if protects_visuals {
        if let Some(filter) = effects_filter.as_mut() {
            let dependencies = resolve_full_effect_dependencies(store, index, filter)?;
            filter.protect_paths(dependencies);
        }
    }
    let ctx = TransformCtx {
        zoom: params.zoom,
        color: color_matcher.as_ref(),
        effects: effects_filter.as_ref(),
    };
    let candidates = collect_patch_targets(index, &patches, effects_filter.as_ref())?;
    let candidates = dedup_candidates(candidates);

    crate::timing!("bundle_batch_read");
    let files: Vec<BundleFile> = candidates.iter().map(|(_, file)| *file).collect();
    let mut by_hash = store.read_files_batch(index, &files)?;
    let file_data: Vec<Vec<u8>> = candidates
        .iter()
        .map(|(path, file)| {
            by_hash
                .remove(&file.hash)
                .ok_or_else(|| anyhow!("patch target bytes missing after read: {path}"))
        })
        .collect::<Result<_>>()?;

    crate::timing!("patch_transform");
    let transformed = candidates
        .par_iter()
        .zip(file_data.into_par_iter())
        .map(
            |((path, _), mut bytes)| -> Result<(String, Vec<u8>, bool)> {
                let mut changed = false;
                for &patch in &patches {
                    if patch_applies_path(patch, path) {
                        let after = transform(patch, path, &bytes, ctx)?;
                        if after != bytes {
                            bytes = after;
                            changed = true;
                        }
                    }
                }
                Ok((path.clone(), bytes, changed))
            },
        )
        .collect::<Vec<_>>()
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    build_patch_set_from_transformed(index, &candidates, transformed)
}

fn collect_patch_targets(
    index: &mut BundleIndex,
    patches: &[PatchId],
    effects: Option<&EffectsFilter>,
) -> Result<Vec<(String, BundleFile)>> {
    let patches = unique_patches(patches);
    // Effects is the one filter-aware patch: Full folders are never read.
    let targets_path = |patch: PatchId, path: &str| match patch {
        PatchId::Effects => effects_targets_path(path, effects),
        PatchId::Particles => {
            patch_targets_path(patch, path)
                && effects.is_none_or(|filter| {
                    filter.level_for(path) != super::effect_skills::EffectLevel::Full
                })
        }
        _ => patch_targets_path(patch, path),
    };
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
            if let Some(file) = index.file_by_path(path).copied() {
                targets
                    .entry(patch)
                    .or_default()
                    .push(((*path).to_string(), file));
            }
        }
    }

    if !broad_patches.is_empty() {
        for entry in index.matching_paths_by(|path| {
            broad_patches.iter().any(|patch| targets_path(*patch, path))
        })? {
            for patch in &broad_patches {
                if targets_path(*patch, &entry.path) {
                    targets
                        .entry(*patch)
                        .or_default()
                        .push((entry.path.clone(), entry.file));
                }
            }
        }
    }

    let mut candidates = Vec::new();
    for &patch in &patches {
        let patch_targets = targets.remove(&patch).unwrap_or_default();
        if patch_targets.is_empty() {
            let full_filter_removed_all_effects =
                matches!(patch, PatchId::Effects | PatchId::Particles)
                    && effects.is_some_and(|f| f.has_full())
                    && index.paths().iter().any(|path| {
                        patch_targets_path(patch, path) && index.file_by_path(path).is_some()
                    });
            if full_filter_removed_all_effects {
                continue;
            }
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

pub(crate) fn unique_patches(patches: &[PatchId]) -> Vec<PatchId> {
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
    index: &BundleIndex,
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
            let bundle_name = index.bundle_name(file.bundle_index)?;
            changes.push(PatchChange {
                path,
                bundle_name: bundle_name.to_string(),
                old_size: file.size as usize,
                new_size: bytes.len(),
            });
            replacements
                .entry(bundle_name.to_string())
                .or_default()
                .push((*file, bytes));
        }
    }

    Ok(PatchSet {
        changes,
        replacements,
    })
}

#[cfg(test)]
fn build_patch_set_from_changed(
    index: &BundleIndex,
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
            let bundle_name = index.bundle_name(file.bundle_index)?;
            changes.push(PatchChange {
                path: path.clone(),
                bundle_name: bundle_name.to_string(),
                old_size: file.size as usize,
                new_size: bytes.len(),
            });
            replacements
                .entry(bundle_name.to_string())
                .or_default()
                .push((*file, bytes));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_replacements_are_built_once_per_target_path() {
        let index = BundleIndex::for_test_paths(&[(
            "metadata/environmentsettings/test.env",
            "env.bundle.bin",
            6,
        )]);
        let file = *index
            .file_by_path("metadata/environmentsettings/test.env")
            .unwrap();
        let candidates = vec![("metadata/environmentsettings/test.env".to_string(), file)];
        let mut file_data = BTreeMap::from([(
            "metadata/environmentsettings/test.env".to_string(),
            b"changed".to_vec(),
        )]);
        let changed = BTreeMap::from([("metadata/environmentsettings/test.env".to_string(), true)]);

        let patch_set =
            build_patch_set_from_changed(&index, &candidates, &mut file_data, &changed).unwrap();

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
        let file = BundleFile::for_test(6);
        let candidates = dedup_candidates(vec![
            ("metadata/environmentsettings/test.env".to_string(), file),
            ("metadata/environmentsettings/test.env".to_string(), file),
        ]);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, "metadata/environmentsettings/test.env");
    }

    #[test]
    fn duplicate_selected_patches_do_not_create_missing_target_errors() {
        let mut index =
            BundleIndex::for_test_paths(&[("metadata/environmentsettings/test.env", "env", 12)]);

        let candidates =
            collect_patch_targets(&mut index, &[PatchId::Fog, PatchId::Fog], None).unwrap();
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
            None,
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
            collect_patch_targets(&mut index, &[PatchId::Minimap, PatchId::AtlasFog], None)
                .unwrap();
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

        let err = collect_patch_targets(&mut index, &[PatchId::Minimap], None).unwrap_err();

        assert!(err
            .to_string()
            .contains("patch 'minimap' has no matching files"));
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
            None,
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
    fn per_skill_levels_filter_effect_target_collection() {
        use super::super::effect_skills::{EffectLevel, EffectSkillOverride};

        let mut index = BundleIndex::for_test_paths(&[
            (
                "metadata/effects/spells/cold_herald_of_ice/ao/ice_explosion.ao",
                "effects",
                12,
            ),
            (
                "metadata/effects/spells/cold_herald_of_ice/epk/buff.epk",
                "effects",
                12,
            ),
            (
                "metadata/effects/spells/cold_herald_of_ice/fx/burst.pet",
                "effects",
                12,
            ),
            (
                "metadata/effects/spells/fireball/fireball.ao",
                "effects",
                12,
            ),
            ("metadata/effects/spells/arc_02/arc.aoc", "effects", 12),
            ("metadata/effects/spells/arc_02/beam.trl", "effects", 12),
        ]);
        let filter = EffectsFilter::new(
            &[
                EffectSkillOverride {
                    folder: "cold_herald_of_ice".to_string(),
                    level: EffectLevel::Reduced,
                },
                EffectSkillOverride {
                    folder: "fireball".to_string(),
                    level: EffectLevel::Full,
                },
            ],
            BTreeSet::new(),
        )
        .unwrap();

        let candidates =
            collect_patch_targets(&mut index, &[PatchId::Effects], Some(&filter)).unwrap();
        let paths = dedup_candidates(candidates)
            .into_iter()
            .map(|(path, _)| path)
            .collect::<Vec<_>>();

        assert!(paths.contains(
            &"metadata/effects/spells/cold_herald_of_ice/ao/ice_explosion.ao".to_string()
        ));
        assert!(paths.contains(&"metadata/effects/spells/arc_02/arc.aoc".to_string()));
        // Full folders are never even read; unlisted folders keep the
        // .ao/.aoc-only rule.
        assert!(!paths.contains(&"metadata/effects/spells/fireball/fireball.ao".to_string()));
        assert!(
            !paths.contains(&"metadata/effects/spells/cold_herald_of_ice/epk/buff.epk".to_string())
        );
        assert!(
            !paths.contains(&"metadata/effects/spells/cold_herald_of_ice/fx/burst.pet".to_string())
        );
        assert!(!paths.contains(&"metadata/effects/spells/arc_02/beam.trl".to_string()));
    }

    #[test]
    fn per_monster_paths_filter_effect_target_collection() {
        let kept = "metadata/effects/spells/monsters_effects/act3/anchoritemother/idle.ao";
        let mut index = BundleIndex::for_test_paths(&[
            (kept, "effects", 12),
            (
                "metadata/effects/spells/monsters_effects/act3/anchoritemother/death.ao",
                "effects",
                12,
            ),
            (
                "metadata/effects/spells/fireball/fireball.ao",
                "effects",
                12,
            ),
        ]);
        let filter = EffectsFilter::new(&[], BTreeSet::from([kept.to_string()])).unwrap();

        let candidates = collect_patch_targets(
            &mut index,
            &[PatchId::Effects, PatchId::MonsterSounds],
            Some(&filter),
        )
        .unwrap();
        let mut effects_paths = Vec::new();
        let mut all_paths = Vec::new();
        for (path, _) in candidates {
            if effects_targets_path(&path, Some(&filter)) {
                effects_paths.push(path.clone());
            }
            all_paths.push(path);
        }

        // The kept monster path drops out of Effects targeting but still
        // flows to MonsterSounds; sibling and unrelated paths stay targeted.
        assert!(!effects_paths.contains(&kept.to_string()));
        assert!(all_paths.contains(&kept.to_string()));
        assert!(effects_paths.contains(
            &"metadata/effects/spells/monsters_effects/act3/anchoritemother/death.ao".to_string()
        ));
        assert!(effects_paths.contains(&"metadata/effects/spells/fireball/fireball.ao".to_string()));
    }

    #[test]
    fn full_skill_folders_still_flow_to_other_selected_patches() {
        use super::super::effect_skills::{EffectLevel, EffectSkillOverride};

        let mut index = BundleIndex::for_test_paths(&[
            (
                "metadata/effects/spells/fireball/fireball.ao",
                "effects",
                12,
            ),
            ("metadata/effects/spells/arc_02/arc.ao", "effects", 12),
        ]);
        let filter = EffectsFilter::new(
            &[EffectSkillOverride {
                folder: "fireball".to_string(),
                level: EffectLevel::Full,
            }],
            BTreeSet::new(),
        )
        .unwrap();

        let candidates = collect_patch_targets(
            &mut index,
            &[PatchId::Effects, PatchId::SkillSounds],
            Some(&filter),
        )
        .unwrap();
        let paths = dedup_candidates(candidates)
            .into_iter()
            .map(|(path, _)| path)
            .collect::<Vec<_>>();

        // The Full folder's .ao is still collected via SkillSounds; the
        // Effects transform itself passes it through unchanged (covered in
        // transform.rs).
        assert!(paths.contains(&"metadata/effects/spells/fireball/fireball.ao".to_string()));
        assert!(paths.contains(&"metadata/effects/spells/arc_02/arc.ao".to_string()));
    }

    #[test]
    fn all_full_effect_selection_is_a_valid_noop() {
        use super::super::effect_skills::{EffectLevel, EffectSkillOverride};

        let mut index = BundleIndex::for_test_paths(&[(
            "metadata/effects/spells/fireball/fireball.ao",
            "effects",
            12,
        )]);
        let filter = EffectsFilter::new(
            &[EffectSkillOverride {
                folder: "fireball".to_string(),
                level: EffectLevel::Full,
            }],
            BTreeSet::new(),
        )
        .unwrap();

        assert!(
            collect_patch_targets(&mut index, &[PatchId::Effects], Some(&filter))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn full_effect_selection_without_base_targets_reports_game_version_error() {
        use super::super::effect_skills::{EffectLevel, EffectSkillOverride};

        let mut index =
            BundleIndex::for_test_paths(&[("metadata/unrelated/file.ao", "unrelated", 12)]);
        let filter = EffectsFilter::new(
            &[EffectSkillOverride {
                folder: "fireball".to_string(),
                level: EffectLevel::Full,
            }],
            BTreeSet::new(),
        )
        .unwrap();

        let err =
            collect_patch_targets(&mut index, &[PatchId::Effects], Some(&filter)).unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("no matching files in this game version"));
        assert!(!msg.contains("keep original visuals"));
    }

    #[test]
    fn sound_patch_skips_character_selection_assets() {
        let startup_scene =
            "Metadata/Terrain/CharacterSelection/CharacterSelectionGallows/Gallows_MainBuilding_fx.ao";
        let mut index = BundleIndex::for_test_paths(&[
            (startup_scene, "startup", 12),
            ("metadata/terrain/trees/tree.ao", "terrain", 12),
        ]);

        let candidates =
            collect_patch_targets(&mut index, &[PatchId::DisableSounds], None).unwrap();
        let paths = dedup_candidates(candidates)
            .into_iter()
            .map(|(path, _)| path)
            .collect::<Vec<_>>();

        assert!(!paths.contains(&startup_scene.to_string()));
        assert!(paths.contains(&"metadata/terrain/trees/tree.ao".to_string()));
    }
    #[test]
    fn kept_ground_preserves_transitive_particles_while_other_effects_are_reduced() {
        use crate::bundle::pack_uncompressed_bundle;
        use crate::patches::text::encode_utf16_bom;
        use crate::patches::{EffectLevel, EffectSkillOverride};
        let temp = tempfile::tempdir().unwrap();
        let bundle_dir = temp.path().join("Bundles2");
        std::fs::create_dir(&bundle_dir).unwrap();
        let effect = |reference: &str| {
            encode_utf16_bom(&format!(
                "version 3\nclient\n{{\nParticleEffects\n{{\n filename = \"{reference}\"\n}}\n}}"
            ))
        };
        let sources = [
            (
                "metadata/effects/spells/ground_effects_v3/burning/burning_grd.ao",
                effect("Metadata/Effects/Spells/shared/burning.epk"),
            ),
            (
                "metadata/effects/spells/shared/burning.epk",
                b"Metadata/Effects/Spells/shared/child.ao".to_vec(),
            ),
            (
                "metadata/effects/spells/shared/child.ao",
                effect("Metadata/Particles/ground_effects_v3/burning/flames.pet"),
            ),
            (
                "metadata/particles/ground_effects_v3/burning/flames.pet",
                encode_utf16_bom("Metadata/Particles/shared/burning.trl"),
            ),
            (
                "metadata/particles/shared/burning.trl",
                encode_utf16_bom("Metadata/Effects/Spells/shared/burning.epk"),
            ),
            (
                "metadata/particles/unrelated/flames.pet",
                encode_utf16_bom("original particle"),
            ),
            (
                "metadata/effects/spells/cold_arcticarmour/arcticarmor.ao",
                effect("Metadata/Particles/ground_effects_v3/burning/flames.pet"),
            ),
            (
                "metadata/effects/spells/ground_effects_v3/shocked/ground.ao",
                effect("shock.pet"),
            ),
            (
                "metadata/effects/spells/crossbow_oilgrenade/oil.ao",
                effect("oil.pet"),
            ),
        ];
        let names: Vec<_> = (0..sources.len()).map(|i| format!("fixture{i}")).collect();
        for ((_, bytes), name) in sources.iter().zip(&names) {
            std::fs::write(
                bundle_dir.join(format!("{name}.bundle.bin")),
                pack_uncompressed_bundle(bytes).unwrap(),
            )
            .unwrap();
        }
        let entries: Vec<_> = sources
            .iter()
            .zip(&names)
            .map(|((path, bytes), name)| (*path, name.as_str(), bytes.len() as u32))
            .collect();
        let mut index = BundleIndex::for_test_paths(&entries);
        let store = BundleStore::new(temp.path());
        let params = PatchParams {
            effect_skills: vec![
                EffectSkillOverride {
                    folder: "ground_effects_v3/burning".into(),
                    level: EffectLevel::Full,
                },
                EffectSkillOverride {
                    folder: "crossbow_oilgrenade".into(),
                    level: EffectLevel::Full,
                },
            ],
            ..Default::default()
        };
        for patches in [
            vec![PatchId::Effects, PatchId::Particles],
            vec![PatchId::Particles, PatchId::Effects],
        ] {
            let result = compute_patch_set(&store, &mut index, &patches, &params).unwrap();
            let changed: BTreeSet<_> = result.changes.iter().map(|c| c.path.as_str()).collect();
            assert_eq!(
                changed,
                BTreeSet::from([
                    "metadata/particles/unrelated/flames.pet",
                    "metadata/effects/spells/cold_arcticarmour/arcticarmor.ao",
                    "metadata/effects/spells/ground_effects_v3/shocked/ground.ao",
                ])
            );
        }
        // Full protection also holds when Effects is deselected but Particles
        // is enabled, and does not depend on which patch appears first.
        let result = compute_patch_set(&store, &mut index, &[PatchId::Particles], &params).unwrap();
        assert_eq!(result.changes.len(), 1);
        assert_eq!(
            result.changes[0].path,
            "metadata/particles/unrelated/flames.pet"
        );
        // Removing the ground override restores reduction of both the ground
        // effect and its particles; the unrelated Oil Grenade override stays.
        let reduced = PatchParams {
            effect_skills: vec![params.effect_skills[1].clone()],
            ..Default::default()
        };
        let result = compute_patch_set(
            &store,
            &mut index,
            &[PatchId::Effects, PatchId::Particles],
            &reduced,
        )
        .unwrap();
        for path in [sources[0].0, sources[2].0, sources[3].0, sources[4].0] {
            assert!(result.changes.iter().any(|c| c.path == path), "{path}");
        }
    }
}
