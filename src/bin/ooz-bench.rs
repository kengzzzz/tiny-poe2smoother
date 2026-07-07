//! Dev-only benchmark for the vendored ooz codec paths.
//!
//! Usage:
//!   cargo run --release --bin ooz-bench -- synth [--iters N] [--mib M]
//!   cargo run --release --bin ooz-bench -- real --game-dir <dir> \
//!       [--iters N] [--budget-mib B] [--stride S]
//!
//! Prints one machine-readable `key=value` line per metric. Digest lines let a
//! runner assert byte-identical output across differently-built binaries.

use anyhow::{bail, ensure, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tiny_poe2smoother::bundle::{decompress_bundle, pack_uncompressed_bundle, BundleStore};

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// (median, min) of the samples, in the samples' unit.
fn median_min(mut samples: Vec<f64>) -> (f64, f64) {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (samples[samples.len() / 2], samples[0])
}

fn time_ms(f: impl FnOnce() -> Result<Vec<u8>>) -> Result<(f64, Vec<u8>)> {
    let start = Instant::now();
    let out = f()?;
    Ok((start.elapsed().as_secs_f64() * 1e3, out))
}

/// Deterministic mixed buffer: LCG noise blocks interleaved with repeated
/// ASCII runs so Mermaid has real matches to find.
fn synth_data(mib: usize) -> Vec<u8> {
    let len = mib << 20;
    let mut out = Vec::with_capacity(len + 8192);
    let mut state = 0x2545_f491_u32;
    let text = b"path/of/exile/2/bundle/data/".repeat(146); // ~4 KiB
    while out.len() < len {
        for _ in 0..4096 {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            out.push((state >> 24) as u8);
        }
        out.extend_from_slice(&text);
    }
    out.truncate(len);
    out
}

fn bench_synth(iters: usize, mib: usize) -> Result<()> {
    let data = synth_data(mib);
    // Warmup + correctness pin.
    let packed = pack_uncompressed_bundle(&data)?;
    let unpacked = decompress_bundle(&packed)?;
    ensure!(unpacked == data, "synth roundtrip mismatch");
    println!("synth_mib={mib}");
    println!("synth_packed_digest={:016x}", fnv1a(&packed));
    println!("synth_unpacked_digest={:016x}", fnv1a(&unpacked));
    println!("synth_ratio={:.4}", packed.len() as f64 / data.len() as f64);

    let mut pack_t = Vec::with_capacity(iters);
    let mut unpack_t = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (ms, p) = time_ms(|| pack_uncompressed_bundle(&data))?;
        pack_t.push(ms);
        let (ms, u) = time_ms(|| decompress_bundle(&p))?;
        unpack_t.push(ms);
        std::hint::black_box(u);
    }
    let (median, min) = median_min(pack_t);
    println!("synth_pack_median_ms={median:.2}");
    println!("synth_pack_min_ms={min:.2}");
    let (median, min) = median_min(unpack_t);
    println!("synth_unpack_median_ms={median:.2}");
    println!("synth_unpack_min_ms={min:.2}");
    Ok(())
}

fn collect_bundles(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_bundles(&path, out)?;
        } else if path.to_string_lossy().ends_with(".bundle.bin") {
            out.push(path);
        }
    }
    Ok(())
}

/// Print one digest line per bundle so differently-built binaries can be
/// diffed down to the exact bundle whose decompressed bytes diverge.
fn digest_real(game_dir: &Path, budget_mib: u64, stride: usize) -> Result<()> {
    let store = BundleStore::new(game_dir);
    let index_bytes = store.read_index_bytes().context("read bundle index")?;
    let index = decompress_bundle(&index_bytes)?;
    println!("index={:016x}", fnv1a(&index));
    let mut all = Vec::new();
    collect_bundles(&store.bundles_dir, &mut all)?;
    all.sort();
    let budget = budget_mib << 20;
    let mut total = 0u64;
    for path in all.iter().step_by(stride.max(1)) {
        let size = std::fs::metadata(path)?.len();
        if total + size > budget {
            continue;
        }
        total += size;
        let out = decompress_bundle(&std::fs::read(path)?)?;
        println!("{:016x} len={} {}", fnv1a(&out), out.len(), path.display());
    }
    Ok(())
}

fn bench_real(game_dir: &Path, iters: usize, budget_mib: u64, stride: usize) -> Result<()> {
    let store = BundleStore::new(game_dir);
    let index_bytes = store.read_index_bytes().context("read bundle index")?;

    // Deterministic sample: sorted paths, every stride-th, capped by budget.
    let mut all = Vec::new();
    collect_bundles(&store.bundles_dir, &mut all)?;
    all.sort();
    ensure!(
        !all.is_empty(),
        "no .bundle.bin files under {}",
        store.bundles_dir.display()
    );
    let budget = budget_mib << 20;
    let mut picked = Vec::new();
    let mut total = 0u64;
    for path in all.iter().step_by(stride.max(1)) {
        let size = std::fs::metadata(path)?.len();
        if total + size > budget {
            continue;
        }
        total += size;
        picked.push(path.clone());
    }
    println!("real_bundles={}", picked.len());
    println!("real_compressed_mib={}", total >> 20);

    // Everything in RAM up front: benchmark the codec, not the disk.
    let blobs: Vec<Vec<u8>> = picked
        .iter()
        .map(|p| std::fs::read(p).with_context(|| p.display().to_string()))
        .collect::<Result<_>>()?;

    // Warmup + cross-variant correctness digest over decompressed output.
    let index = decompress_bundle(&index_bytes)?;
    let mut digest = fnv1a(&index);
    let mut decompressed_bytes = index.len() as u64;
    for blob in &blobs {
        let out = decompress_bundle(blob)?;
        digest ^= fnv1a(&out);
        decompressed_bytes += out.len() as u64;
    }
    println!("real_decompressed_mib={}", decompressed_bytes >> 20);
    println!("real_digest={digest:016x}");

    let mut unpack_t = Vec::with_capacity(iters);
    let mut pack_t = Vec::with_capacity(iters);
    for _ in 0..iters {
        let start = Instant::now();
        let index = decompress_bundle(&index_bytes)?;
        for blob in &blobs {
            std::hint::black_box(decompress_bundle(blob)?);
        }
        unpack_t.push(start.elapsed().as_secs_f64() * 1e3);
        // Realistic compress workload: repack the decompressed index.
        let (ms, packed) = time_ms(|| pack_uncompressed_bundle(&index))?;
        pack_t.push(ms);
        std::hint::black_box(packed);
    }
    let (median, min) = median_min(unpack_t);
    println!("real_unpack_median_ms={median:.2}");
    println!("real_unpack_min_ms={min:.2}");
    let (median, min) = median_min(pack_t);
    println!("real_pack_index_median_ms={median:.2}");
    println!("real_pack_index_min_ms={min:.2}");
    Ok(())
}

fn parse_flag<T: std::str::FromStr>(args: &[String], name: &str, default: T) -> Result<T> {
    match args.iter().position(|a| a == name) {
        Some(i) => {
            let value = args
                .get(i + 1)
                .with_context(|| format!("{name} requires a value"))?;
            value
                .parse()
                .ok()
                .with_context(|| format!("invalid value for {name}: {value}"))
        }
        None => Ok(default),
    }
}

fn main() -> Result<()> {
    tiny_poe2smoother::init_tracing();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("synth") => {
            let iters = parse_flag(&args, "--iters", 15usize)?;
            let mib = parse_flag(&args, "--mib", 64usize)?;
            bench_synth(iters, mib)
        }
        Some("real") => {
            let game_dir: String = parse_flag(&args, "--game-dir", String::new())?;
            ensure!(!game_dir.is_empty(), "real requires --game-dir <dir>");
            let iters = parse_flag(&args, "--iters", 7usize)?;
            let budget_mib = parse_flag(&args, "--budget-mib", 512u64)?;
            let stride = parse_flag(&args, "--stride", 23usize)?;
            bench_real(Path::new(&game_dir), iters, budget_mib, stride)
        }
        Some("dump-index") => {
            let game_dir: String = parse_flag(&args, "--game-dir", String::new())?;
            let out: String = parse_flag(&args, "--out", String::new())?;
            ensure!(
                !game_dir.is_empty() && !out.is_empty(),
                "dump-index requires --game-dir <dir> --out <file>"
            );
            let store = BundleStore::new(Path::new(&game_dir));
            let index = decompress_bundle(&store.read_index_bytes()?)?;
            std::fs::write(&out, &index)?;
            println!("wrote {} bytes to {out}", index.len());
            Ok(())
        }
        Some("digest") => {
            let game_dir: String = parse_flag(&args, "--game-dir", String::new())?;
            ensure!(!game_dir.is_empty(), "digest requires --game-dir <dir>");
            let budget_mib = parse_flag(&args, "--budget-mib", 512u64)?;
            let stride = parse_flag(&args, "--stride", 23usize)?;
            digest_real(Path::new(&game_dir), budget_mib, stride)
        }
        _ => bail!("usage: ooz-bench synth [--iters N] [--mib M] | ooz-bench real|digest --game-dir <dir> [--iters N] [--budget-mib B] [--stride S]"),
    }
}
