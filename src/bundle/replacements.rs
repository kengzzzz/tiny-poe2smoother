use super::compression::pack_uncompressed_bundle;
use super::index::{BundleFile, BundleIndex};
use super::store::BundleStore;
use crate::backup::BackupStore;
use crate::install::InstallLayout;
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

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
    let mut generated_bundle_paths: Vec<PathBuf> = Vec::new();
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
    let file_order = index.file_order_map();
    sort_edits_by_index_order(&mut edits, &file_order);

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
    if let Err(e) = atomic_write(&custom_path, &out) {
        // Clean up any previously written generated bundles
        for p in &generated_bundle_paths {
            let _ = fs::remove_file(p);
        }
        return Err(e.context(format!(
            "failed to write generated bundle at {}",
            custom_path.display()
        )));
    }
    generated_bundle_paths.push(custom_path.clone());
    touched.push(custom_path);

    if let Err(e) = atomic_write(&store.index_path, &index_bytes) {
        // Index replacement failed; remove the orphaned generated bundle
        for p in &generated_bundle_paths {
            let _ = fs::remove_file(p);
        }
        return Err(e.context("failed to replace index; generated bundle has been cleaned up"));
    }
    touched.push(store.index_path.clone());
    Ok(BundleWriteReport {
        touched_paths: touched,
    })
}

fn sort_edits_by_index_order(
    edits: &mut [(BundleFile, Vec<u8>)],
    file_order: &HashMap<u64, usize>,
) {
    edits.sort_by_key(|(file, _)| file_order.get(&file.hash).copied().unwrap_or(usize::MAX));
}

pub fn slice_file(bundle: &[u8], file: &BundleFile) -> Result<Vec<u8>> {
    let start = file.offset as usize;
    let end = start + file.size as usize;
    if end > bundle.len() {
        bail!("file slice exceeds bundle length for {}", file.bundle_name);
    }
    Ok(bundle[start..end].to_vec())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|err| anyhow!("failed to replace {}: {}", path.display(), err.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_edits_sort_by_index_order_map() {
        let mut edits = vec![
            (
                BundleFile::for_test_with_hash(30, "a", 1),
                b"third".to_vec(),
            ),
            (
                BundleFile::for_test_with_hash(10, "a", 1),
                b"first".to_vec(),
            ),
            (
                BundleFile::for_test_with_hash(20, "a", 1),
                b"second".to_vec(),
            ),
        ];
        let order = HashMap::from([(10, 0), (20, 1), (30, 2)]);

        sort_edits_by_index_order(&mut edits, &order);

        let hashes = edits
            .into_iter()
            .map(|(file, _)| file.hash)
            .collect::<Vec<_>>();
        assert_eq!(hashes, vec![10, 20, 30]);
    }
}
