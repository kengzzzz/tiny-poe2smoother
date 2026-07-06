use super::catalog::{patch_label, PatchChange, PatchId, PatchSet};
use super::targeting::{exact_patch_targets, patch_applies_path, patch_targets_path};
use super::transform::transform;
use crate::bundle::{BundleFile, BundleIndex, BundleStore};
use anyhow::{anyhow, bail, Result};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap, HashSet};

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
    let files: Vec<BundleFile> = candidates.iter().map(|(_, file)| file.clone()).collect();
    let mut by_hash = store.read_files_batch(&files)?;
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
        .map(|((path, _), mut bytes)| -> Result<(String, Vec<u8>, bool)> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
