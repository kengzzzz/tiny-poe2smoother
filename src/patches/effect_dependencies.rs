//! Preserve the visual references of effects kept Full, including particles
//! outside the selected folder. Only actual index entries are followed.

use super::effect_skills::{EffectLevel, EffectsFilter};
use super::targeting::normalize_path;
use crate::bundle::{BundleIndex, BundleStore};
use anyhow::{Context, Result};
use regex::Regex;
use std::collections::BTreeSet;
use std::sync::LazyLock;

static REF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)metadata[/\\][^"'\s\)\};,\x00]+"#).unwrap());

fn scannable(path: &str) -> bool {
    path.starts_with("metadata/")
        && [".ao", ".aoc", ".epk", ".pet", ".trl"]
            .iter()
            .any(|ext| path.ends_with(ext))
}

fn references(bytes: &[u8]) -> BTreeSet<String> {
    // References occur in UTF-16 effect text and ASCII/UTF-8 packs. Scan both
    // views rather than guessing encoding from a binary pack's first bytes.
    let utf16 = String::from_utf16_lossy(
        &bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect::<Vec<_>>(),
    );
    let ascii = String::from_utf8_lossy(bytes);
    [&utf16, ascii.as_ref()]
        .into_iter()
        .flat_map(|text| REF_RE.find_iter(text))
        .map(|m| normalize_path(m.as_str()))
        .filter(|path| scannable(path))
        .collect()
}

pub(super) fn resolve_full_effect_dependencies(
    store: &BundleStore,
    index: &mut BundleIndex,
    filter: &EffectsFilter,
) -> Result<BTreeSet<String>> {
    index.ensure_paths_built()?;
    let mut pending: BTreeSet<_> = index
        .paths()
        .iter()
        .map(|path| normalize_path(path))
        .filter(|path| scannable(path) && filter.level_for(path) == EffectLevel::Full)
        .collect();
    let mut visited = BTreeSet::new();
    let mut kept = BTreeSet::new();
    while !pending.is_empty() {
        // Bound each batch's working set. The visited set handles cycles;
        // there is no depth cutoff that silently drops a kept visual's child.
        let mut candidates = Vec::new();
        for _ in 0..256 {
            let Some(path) = pending.pop_first() else {
                break;
            };
            if !visited.insert(path.clone()) {
                continue;
            }
            if let Some(file) = index.file_by_path(&path).copied() {
                candidates.push((path, file));
            }
        }
        let files: Vec<_> = candidates.iter().map(|(_, file)| *file).collect();
        let mut data = store
            .read_files_batch(index, &files)
            .context("read dependencies of kept effects")?;
        for (path, file) in candidates {
            kept.insert(path);
            let bytes = data
                .remove(&file.hash)
                .expect("batch returned each requested file");
            for reference in references(&bytes) {
                if !visited.contains(&reference) && index.file_by_path(&reference).is_some() {
                    pending.insert(reference);
                }
            }
        }
    }
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patches::text::encode_utf16_bom;

    #[test]
    fn reference_scan_handles_utf16_packs_and_path_separators() {
        let text = r#"filename = "Metadata\Particles\ground_effects_v3\burning\flames.pet"
            attached = 'Metadata/Effects/Spells/shared/pack.epk'
            texture = "Art/texture.dds" missing = "Metadata/Parent""#;
        let expected = BTreeSet::from([
            "metadata/particles/ground_effects_v3/burning/flames.pet".into(),
            "metadata/effects/spells/shared/pack.epk".into(),
        ]);
        assert_eq!(references(text.as_bytes()), expected);
        assert_eq!(references(&encode_utf16_bom(text)), expected);
    }
}
