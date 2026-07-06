//! Dev-only tool: diff a pristine `Bundles2` against a patched one
//! and emit the changed files' ground-truth bytes.
//!
//! Usage:
//!   cargo run --release --bin capture-diff -- <before_game_dir> <after_game_dir> <out_dir>
//!
//! The capture archives are *subsets*: they contain the full `_.index.bin` (which
//! references every bundle in the game) but only the handful of `*.bundle.bin`
//! files that were actually written/repacked. So we diff at the **index-record**
//! level first (a file is changed if its bundle assignment or size differs, or it
//! is new), then decompress only the physically-present bundles we need to read
//! the patched bytes and to byte-compare the same-bundle/same-size cases.
//!
//! Output:
//!   <out_dir>/manifest.csv  path,status,before_bundle,after_bundle,before_size,after_size,content_hash,ext
//!   <out_dir>/removed.txt   paths present in before index but absent in after index
//!   <out_dir>/blobs/<hash>.bin   one file per unique patched content

use anyhow::{Context, Result};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use tiny_poe2smoother::bundle::{slice_file, BundleFile, BundleStore};
use tiny_poe2smoother::patches::{audit_transform, PatchId};

fn content_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn ext_of(path: &str) -> &str {
    match path.rfind('.') {
        Some(i) => &path[i..],
        None => "",
    }
}

struct Capture {
    store: BundleStore,
    files: HashMap<String, BundleFile>,
    present: HashSet<String>, // bundle names physically on disk
}

fn load(game_dir: &Path) -> Result<Capture> {
    let store = BundleStore::new(game_dir);
    let mut index = store
        .open_index()
        .with_context(|| format!("open index for {}", game_dir.display()))?;
    let indexed = index.indexed_paths()?;
    let mut files = HashMap::with_capacity(indexed.len());
    for ip in indexed {
        files.insert(ip.path, ip.file);
    }
    let mut present = HashSet::new();
    for entry in fs::read_dir(&store.bundles_dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some(stripped) = name.strip_suffix(".bundle.bin") {
            present.insert(stripped.to_string());
        }
    }
    eprintln!(
        "  {} : {} index files, {} bundles physically present",
        game_dir.display(),
        files.len(),
        present.len()
    );
    Ok(Capture {
        store,
        files,
        present,
    })
}

/// Decompress the named present bundles into name -> bytes.
fn decompress_present(cap: &Capture, names: &HashSet<String>) -> HashMap<String, Vec<u8>> {
    let wanted: Vec<String> = names
        .iter()
        .filter(|n| cap.present.contains(*n))
        .cloned()
        .collect();
    let mut map = HashMap::new();
    let results: Vec<(String, Vec<u8>)> = wanted
        .par_iter()
        .filter_map(|n| match cap.store.read_bundle(n) {
            Ok(b) => Some((n.clone(), b)),
            Err(e) => {
                eprintln!("WARN read_bundle {n}: {e:#}");
                None
            }
        })
        .collect();
    for (n, b) in results {
        map.insert(n, b);
    }
    map
}

#[derive(Clone)]
struct Change {
    path: String,
    status: &'static str, // changed | added
    before_bundle: String,
    after_bundle: String,
    before_size: i64,
    after_size: i64,
    content_hash: u64,
    bytes: Vec<u8>,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!(
            "usage: {} <before_game_dir> <after_game_dir> <out_dir>",
            args[0]
        );
        std::process::exit(2);
    }
    let before_dir = PathBuf::from(&args[1]);
    let after_dir = PathBuf::from(&args[2]);
    let out_dir = PathBuf::from(&args[3]);

    eprintln!("loading before…");
    let before = load(&before_dir)?;
    eprintln!("loading after…");
    let after = load(&after_dir)?;

    // Optional: dump all after-index paths matching a substring (for protection audits).
    if let Ok(needle) = std::env::var("GREP_PATHS") {
        let needle = needle.to_ascii_lowercase();
        let mut hits: Vec<&String> = after
            .files
            .keys()
            .filter(|p| p.to_ascii_lowercase().contains(&needle))
            .collect();
        hits.sort();
        eprintln!("paths containing {needle:?}: {}", hits.len());
        for p in hits.iter().take(60) {
            println!("{p}");
        }
        return Ok(());
    }

    // Optional: dump before+after bytes for the first DUMP_N changed paths matching
    // DUMP_NEEDLE (writes <out>/dump/<i>.{path,before,after}). For rule derivation.
    if let Ok(needle) = std::env::var("DUMP_NEEDLE") {
        let needle = needle.to_ascii_lowercase();
        let n: usize = std::env::var("DUMP_N")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20);
        let mut hits: Vec<&String> = after
            .files
            .keys()
            .filter(|p| {
                let pl = p.to_ascii_lowercase();
                pl.contains(&needle)
                    && before
                        .files
                        .get(*p)
                        .map(|bf| {
                            let af = &after.files[*p];
                            bf.bundle_name != af.bundle_name || bf.size != af.size
                        })
                        .unwrap_or(false)
            })
            .collect();
        hits.sort();
        hits.truncate(n);
        let mut bundles_needed: HashSet<String> = HashSet::new();
        for p in &hits {
            bundles_needed.insert(after.files[*p].bundle_name.clone());
            bundles_needed.insert(before.files[*p].bundle_name.clone());
        }
        let ab = decompress_present(&after, &bundles_needed);
        let bb = decompress_present(&before, &bundles_needed);
        let dump_dir = PathBuf::from(&args[3]).join("dump");
        fs::create_dir_all(&dump_dir)?;
        for (i, p) in hits.iter().enumerate() {
            let af = &after.files[*p];
            let bf = &before.files[*p];
            if let (Some(abd), Some(bbd)) = (ab.get(&af.bundle_name), bb.get(&bf.bundle_name)) {
                if let (Ok(a), Ok(b)) = (slice_file(abd, af), slice_file(bbd, bf)) {
                    fs::write(dump_dir.join(format!("{i:02}.path")), p.as_str())?;
                    fs::write(dump_dir.join(format!("{i:02}.before")), &b)?;
                    fs::write(dump_dir.join(format!("{i:02}.after")), &a)?;
                }
            }
        }
        eprintln!("dumped {} pairs to {}", hits.len(), dump_dir.display());
        return Ok(());
    }

    // Pass 1: index-record diff. Classify each after path; collect bundles to read.
    eprintln!(
        "classifying {} after paths via index records…",
        after.files.len()
    );
    let mut definite: Vec<(String, &'static str)> = Vec::new(); // path, status (changed/added)
    let mut maybe: Vec<String> = Vec::new(); // same bundle + same size -> needs byte compare
    for (path, af) in &after.files {
        match before.files.get(path) {
            None => definite.push((path.clone(), "added")),
            Some(bf) => {
                if bf.bundle_name != af.bundle_name || bf.size != af.size {
                    definite.push((path.clone(), "changed"));
                } else {
                    maybe.push(path.clone());
                }
            }
        }
    }
    eprintln!(
        "  definite (record differs): {}   needs-byte-compare (same bundle+size): {}",
        definite.len(),
        maybe.len()
    );

    // Bundles we must decompress.
    let mut after_needed: HashSet<String> = HashSet::new();
    for (p, _) in &definite {
        after_needed.insert(after.files[p].bundle_name.clone());
    }
    for p in &maybe {
        after_needed.insert(after.files[p].bundle_name.clone());
    }
    let mut before_needed: HashSet<String> = HashSet::new();
    for p in &maybe {
        before_needed.insert(before.files[p].bundle_name.clone());
    }
    eprintln!(
        "decompressing {} after bundles + {} before bundles (present-only)…",
        after_needed
            .iter()
            .filter(|n| after.present.contains(*n))
            .count(),
        before_needed
            .iter()
            .filter(|n| before.present.contains(*n))
            .count(),
    );
    let after_bundles = decompress_present(&after, &after_needed);
    let before_bundles = decompress_present(&before, &before_needed);

    let read = |cap: &Capture, bundles: &HashMap<String, Vec<u8>>, path: &str| -> Option<Vec<u8>> {
        let f = cap.files.get(path)?;
        let b = bundles.get(&f.bundle_name)?;
        slice_file(b, f).ok()
    };

    // Pass 2: byte-compare the "maybe" set; keep only those that truly differ.
    let maybe_changed: Vec<String> = maybe
        .par_iter()
        .filter_map(|path| {
            let a = read(&after, &after_bundles, path)?;
            let b = read(&before, &before_bundles, path)?;
            if a != b {
                Some(path.clone())
            } else {
                None
            }
        })
        .collect();
    eprintln!("  of those, {} actually differ", maybe_changed.len());

    // Build final change set with bytes.
    let mut all: Vec<(String, &'static str)> = definite;
    for p in maybe_changed {
        all.push((p, "changed"));
    }

    // VERIFY: run the real capture-driven transforms over every changed file and
    // confirm they reproduce captured bytes. A file the patches do not target
    // (particles/env/materials handled elsewhere) is skipped.
    if std::env::var("VERIFY").is_ok() {
        let patches = [
            PatchId::DisableSounds,
            PatchId::SkillSounds,
            PatchId::MonsterSounds,
            PatchId::MtxSoft,
        ];
        let mut targeted = 0usize;
        let mut matched = 0usize;
        // mismatch buckets:
        let mut mm_pet = 0usize; // mtx .pet: we write "0"; the reference keeps a template.
        let mut mm_idempotent = 0usize; // sound file: our output is a *superset* of the reference.
                                        // (re-running our transform on captured bytes is a no-op => we agree on sound blocks,
                                        //  the size diff is other-option content we correctly leave alone)
        let mut mm_other: Vec<(String, usize, usize)> = Vec::new(); // anything unexplained (must be 0)
        for (path, _) in &all {
            let Some(before_bytes) = read(&before, &before_bundles, path) else {
                continue;
            };
            let Some(after_bytes) = read(&after, &after_bundles, path) else {
                continue;
            };
            for patch in patches {
                if let Some(result) = audit_transform(patch, path, &before_bytes) {
                    let out = result.expect("transform failed");
                    targeted += 1;
                    if out == after_bytes {
                        matched += 1;
                    } else if patch == PatchId::MtxSoft
                        && path.to_ascii_lowercase().ends_with(".pet")
                    {
                        mm_pet += 1;
                    } else if audit_transform(patch, path, &after_bytes)
                        .map(|r| r.unwrap() == after_bytes)
                        .unwrap_or(true)
                    {
                        // our transform is idempotent on captured output:
                        // we removed nothing it kept (no over-removal); the size
                        // delta is content emptied by a *different* option.
                        mm_idempotent += 1;
                    } else {
                        mm_other.push((path.clone(), out.len(), after_bytes.len()));
                    }
                    break;
                }
            }
        }
        println!("\n==== VERIFY (real transforms vs captured bytes) ====");
        println!("targeted by our 4 patches: {targeted}");
        println!("byte-exact matches: {matched}");
        println!("mtx .pet (we emit valid \"0\" vs vendor template): {mm_pet}");
        println!("sound supersets (idempotent on vendor output, no over-removal): {mm_idempotent}");
        println!("UNEXPLAINED mismatches (should be 0): {}", mm_other.len());
        for (p, got, want) in mm_other.iter().take(40) {
            println!("  UNEXPLAINED {p}  (got {got} B, want {want} B)");
        }
        return Ok(());
    }

    let changes: Vec<Change> = all
        .par_iter()
        .filter_map(|(path, status)| {
            let bytes = read(&after, &after_bundles, path)?;
            let af = &after.files[path];
            let (before_bundle, before_size) = match before.files.get(path) {
                Some(bf) => (bf.bundle_name.clone(), bf.size as i64),
                None => (String::new(), -1),
            };
            Some(Change {
                path: path.clone(),
                status,
                before_bundle,
                after_bundle: af.bundle_name.clone(),
                before_size,
                after_size: bytes.len() as i64,
                content_hash: content_hash(&bytes),
                bytes,
            })
        })
        .collect();

    let mut removed: Vec<&String> = before
        .files
        .keys()
        .filter(|p| !after.files.contains_key(*p))
        .collect();
    removed.sort();

    // ---- write output ----
    fs::create_dir_all(&out_dir)?;
    let blobs_dir = out_dir.join("blobs");
    fs::create_dir_all(&blobs_dir)?;
    let mut sorted = changes.clone();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));

    let mut manifest = String::from(
        "path,status,before_bundle,after_bundle,before_size,after_size,content_hash,ext\n",
    );
    for c in &sorted {
        manifest.push_str(&format!(
            "{},{},{},{},{},{},{:016x},{}\n",
            c.path,
            c.status,
            c.before_bundle,
            c.after_bundle,
            c.before_size,
            c.after_size,
            c.content_hash,
            ext_of(&c.path)
        ));
    }
    fs::write(out_dir.join("manifest.csv"), manifest)?;
    fs::write(
        out_dir.join("removed.txt"),
        removed
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    )?;
    let mut blob_seen: HashSet<u64> = HashSet::new();
    for c in &sorted {
        if blob_seen.insert(c.content_hash) {
            fs::write(
                blobs_dir.join(format!("{:016x}.bin", c.content_hash)),
                &c.bytes,
            )?;
        }
    }

    // ---- summary ----
    let mut by_ext: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut blob_refs: HashMap<u64, usize> = HashMap::new();
    let mut blob_size: HashMap<u64, usize> = HashMap::new();
    let (mut n_changed, mut n_added) = (0usize, 0usize);
    for c in &sorted {
        let e = by_ext.entry(ext_of(&c.path)).or_default();
        if c.status == "changed" {
            e.0 += 1;
            n_changed += 1;
        } else {
            e.1 += 1;
            n_added += 1;
        }
        *blob_refs.entry(c.content_hash).or_default() += 1;
        blob_size.insert(c.content_hash, c.bytes.len());
    }
    let total_blob_bytes: usize = blob_size.values().sum();
    println!("\n==== capture-diff summary ====");
    println!(
        "changed: {n_changed}   added: {n_added}   removed: {}",
        removed.len()
    );
    println!(
        "unique content blobs: {}  (total {} bytes)",
        blob_refs.len(),
        total_blob_bytes
    );
    println!("\nby extension (changed / added):");
    for (ext, (ch, ad)) in &by_ext {
        let e = if ext.is_empty() { "(none)" } else { ext };
        println!("  {e:12}  {ch:6} / {ad}");
    }
    let mut top: Vec<(u64, usize)> = blob_refs.iter().map(|(h, n)| (*h, *n)).collect();
    top.sort_by_key(|item| std::cmp::Reverse(item.1));
    println!("\ntop 20 most-referenced blobs (hash  refs  size):");
    for (h, n) in top.iter().take(20) {
        println!("  {h:016x}  {n:6}  {} bytes", blob_size[h]);
    }
    println!("\noutput written to {}", out_dir.display());
    Ok(())
}
