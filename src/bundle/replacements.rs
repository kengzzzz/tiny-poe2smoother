use super::compression::pack_uncompressed_bundle;
use super::index::{BundleFile, BundleIndex};
use super::store::BundleStore;
use crate::atomic_write;
use crate::backup::BackupStore;
use crate::install::InstallLayout;
use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct BundleWriteReport {
    pub touched_paths: Vec<PathBuf>,
}

pub fn apply_bundle_replacements(
    store: &BundleStore,
    index: &mut BundleIndex,
    replacements: &HashMap<String, Vec<(BundleFile, Vec<u8>)>>,
    backup: Option<&BackupStore>,
) -> Result<BundleWriteReport> {
    crate::timing!("apply_replacements_total");
    let mut touched = Vec::new();
    let custom_prefix = match store.layout {
        InstallLayout::LooseBundles => "TinyPoe2Smoother/",
        InstallLayout::ContentGgpk => "TinyPoe2Smoother_",
    };
    let custom_bundle_index = index.create_custom_bundle(custom_prefix)?;
    let custom_bundle_name = index.bundles[custom_bundle_index as usize].name.clone();
    let mut custom_data = Vec::new();

    let mut edits = replacements
        .values()
        .flat_map(|items| items.iter().cloned())
        .collect::<Vec<_>>();
    sort_edits_by_index_order(&mut edits);

    for (file, replacement) in edits {
        let new_offset = custom_data.len();
        custom_data.extend_from_slice(&replacement);
        index.update_file_record(
            file.hash,
            custom_bundle_index,
            u32::try_from(new_offset)?,
            u32::try_from(replacement.len())?,
        )?;
    }
    index.update_bundle_size(custom_bundle_index, u32::try_from(custom_data.len())?)?;

    let out = pack_uncompressed_bundle(&custom_data)?;
    let index_bytes = index.packed_bytes()?;

    if matches!(store.layout, InstallLayout::ContentGgpk) {
        let plan = store.prepare_ggpk_patch(&custom_bundle_name, &out, &index_bytes)?;
        let backup =
            backup.ok_or_else(|| anyhow!("standalone GGPK patch requires a backup store"))?;
        backup.ensure_ggpk_backup_meta(&store.game_dir, plan.backup)?;
        store.apply_ggpk_patch(&plan)?;
        store.remove_legacy_overlay_files()?;
        touched.push(store.content_path.clone());
        return Ok(BundleWriteReport {
            touched_paths: touched,
        });
    }

    let custom_path = store.bundle_path(&custom_bundle_name);
    if let Some(parent) = custom_path.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(&custom_path, &out).with_context(|| {
        format!(
            "failed to write generated bundle at {}",
            custom_path.display()
        )
    })?;

    if let Err(e) = atomic_write(&store.index_path, &index_bytes) {
        let _ = fs::remove_file(&custom_path);
        return Err(e.context("failed to replace index. Generated bundle has been cleaned up"));
    }
    touched.push(custom_path);
    touched.push(store.index_path.clone());
    Ok(BundleWriteReport {
        touched_paths: touched,
    })
}

fn sort_edits_by_index_order(edits: &mut [(BundleFile, Vec<u8>)]) {
    edits.sort_by_key(|(file, _)| file.ordinal);
}

pub fn slice_file(bundle: &[u8], file: &BundleFile) -> Result<Vec<u8>> {
    let start = file.offset as usize;
    let end = start + file.size as usize;
    if end > bundle.len() {
        bail!(
            "file slice exceeds bundle length for bundle index {} (file hash {:#x})",
            file.bundle_index,
            file.hash
        );
    }
    Ok(bundle[start..end].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_edits_sort_by_index_order() {
        let file = |hash, ordinal| BundleFile {
            ordinal,
            ..BundleFile::for_test_with_hash(hash, 1)
        };
        let mut edits = vec![
            (file(30, 2), b"third".to_vec()),
            (file(10, 0), b"first".to_vec()),
            (file(20, 1), b"second".to_vec()),
        ];

        sort_edits_by_index_order(&mut edits);

        let hashes = edits
            .into_iter()
            .map(|(file, _)| file.hash)
            .collect::<Vec<_>>();
        assert_eq!(hashes, vec![10, 20, 30]);
    }
}
