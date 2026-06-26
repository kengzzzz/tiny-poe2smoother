use crate::app::{
    apply_patches, backup_entries, find_paths as find_index_paths,
    inspect_path as inspect_index_path, load_status, parse_patch_names, patch_names,
    preview_patches, resolve_patch_selection, restore_backup, PatchRequest,
    PatchSelection, PatchState,
};
use crate::install::display_path;
use crate::patches::{all_patches, all_presets, PatchId};
use anyhow::{bail, Result};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "tiny-poe2smoother")]
#[command(about = "Rust CLI smoother/patcher for Path of Exile 2 bundle files")]
pub struct Cli {
    #[arg(long, global = true)]
    game_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Status,
    ListPatches,
    FindPaths(FindPathsArgs),
    InspectPath(InspectPathArgs),
    DryRun(PatchArgs),
    Apply(ApplyArgs),
    Restore(ConfirmArgs),
    BackupInfo,
}

#[derive(Debug, Args)]
struct PatchArgs {
    #[arg(long = "patch", short = 'p')]
    patches: Vec<String>,

    #[arg(long = "preset")]
    presets: Vec<String>,

    #[arg(long)]
    all: bool,

    #[arg(long, default_value_t = 2.4)]
    zoom: f64,
}

#[derive(Debug, Args)]
struct FindPathsArgs {
    query: String,

    #[arg(long, default_value_t = 25)]
    limit: usize,
}

#[derive(Debug, Args)]
struct InspectPathArgs {
    path: String,

    #[arg(long, default_value_t = 512)]
    bytes: usize,
}

#[derive(Debug, Args)]
struct ApplyArgs {
    #[command(flatten)]
    patch_args: PatchArgs,

    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
struct ConfirmArgs {
    #[arg(long)]
    yes: bool,
}

pub fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .without_time()
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Status => status(cli.game_dir),
        Command::ListPatches => list_patches(),
        Command::FindPaths(args) => find_paths(cli.game_dir, args),
        Command::InspectPath(args) => inspect_path(cli.game_dir, args),
        Command::DryRun(args) => dry_run(cli.game_dir, args),
        Command::Apply(args) => apply(cli.game_dir, args),
        Command::Restore(args) => restore(cli.game_dir, args),
        Command::BackupInfo => backup_info(),
    }
}

fn status(game_dir: Option<PathBuf>) -> Result<()> {
    let status = load_status(game_dir)?;
    println!("Game dir: {}", display_path(&status.game_dir));
    println!("Bundle index: {}", display_path(&status.index_path));
    println!("Indexed paths: {}", status.indexed_paths);
    println!("Patch state: {}", patch_state_label(status.patch_state));
    println!(
        "Backup: {} ({})",
        display_path(&status.backup_path),
        backup_label(status.patch_state)
    );
    Ok(())
}

fn list_patches() -> Result<()> {
    println!("Patches:");
    for patch in all_patches() {
        println!("{:<16} {}", patch.name, patch.description);
    }
    println!();
    println!("Aliases:");
    println!("{:<16} Alias for particles.", "zero-particles");
    println!();
    println!("Presets:");
    for preset in all_presets() {
        println!("{:<16} {}", preset.name, preset.description);
    }
    Ok(())
}

fn find_paths(game_dir: Option<PathBuf>, args: FindPathsArgs) -> Result<()> {
    let results = find_index_paths(game_dir, &args.query, args.limit)?;
    for result in &results {
        let marker = if result.has_record {
            "file"
        } else {
            "no-record"
        };
        println!("{} [{}]", result.path, marker);
    }
    if results.is_empty() {
        println!("No paths matched {:?}.", args.query.to_ascii_lowercase());
    }
    Ok(())
}

fn inspect_path(game_dir: Option<PathBuf>, args: InspectPathArgs) -> Result<()> {
    let inspected = inspect_index_path(game_dir, &args.path, args.bytes)?;
    println!("Path: {}", inspected.path);
    println!("Size: {} bytes", inspected.size);
    println!("First {} byte(s):", args.bytes.min(inspected.size));
    for byte in inspected.bytes {
        print!("{byte:02x} ");
    }
    println!();
    if let Some(text) = inspected.text_preview {
        println!("{text}");
    }
    Ok(())
}

fn dry_run(game_dir: Option<PathBuf>, args: PatchArgs) -> Result<()> {
    let patches = selected_patches(&args)?;
    let preview = preview_patches(PatchRequest {
        game_dir,
        patches,
        zoom: args.zoom,
    })?;

    println!("Game dir: {}", display_path(&preview.game_dir));
    println!(
        "Selected patches: {}",
        patch_names(&preview.patches).join(", ")
    );
    if preview.changes.is_empty() {
        println!("No changes needed.");
        return Ok(());
    }
    println!("Would modify {} file(s):", preview.changes.len());
    for change in &preview.changes {
        println!(
            "{} [{}] {} -> {} bytes",
            change.path, change.bundle_name, change.old_size, change.new_size
        );
    }
    Ok(())
}

fn apply(game_dir: Option<PathBuf>, args: ApplyArgs) -> Result<()> {
    if !args.yes {
        bail!("refusing to modify game files without --yes");
    }
    let patches = selected_patches(&args.patch_args)?;
    let report = apply_patches(PatchRequest {
        game_dir,
        patches,
        zoom: args.patch_args.zoom,
    })?;

    if report.changed_files == 0 {
        println!("No changes needed.");
        return Ok(());
    }

    println!("Applied {} modified file(s).", report.changed_files);
    println!(
        "Touched {} bundle/index file(s):",
        report.touched_paths.len()
    );
    for path in report.touched_paths {
        println!("{}", display_path(&path));
    }
    println!("Backup: {}", display_path(&report.backup_path));
    Ok(())
}

fn restore(game_dir: Option<PathBuf>, args: ConfirmArgs) -> Result<()> {
    if !args.yes {
        bail!("refusing to restore game files without --yes");
    }
    let report = restore_backup(game_dir)?;
    if report.restored_files == 0 {
        println!("No backup found.");
        return Ok(());
    }
    println!("Restored {} file(s).", report.restored_files);
    Ok(())
}

fn backup_info() -> Result<()> {
    let (path, entries) = backup_entries()?;
    println!("Backup: {}", display_path(&path));
    if entries.is_empty() {
        println!("No backup found.");
        return Ok(());
    }
    println!("Entries: {}", entries.len());
    for entry in entries {
        println!("{} ({} bytes)", entry.rel_path.display(), entry.bytes.len());
    }
    Ok(())
}

fn selected_patches(args: &PatchArgs) -> Result<Vec<PatchId>> {
    if args.presets.is_empty() {
        return parse_patch_names(&args.patches, args.all);
    }
    resolve_patch_selection(PatchSelection {
        patches: args.patches.clone(),
        presets: args.presets.clone(),
        all: args.all,
    })
}

fn patch_state_label(state: PatchState) -> &'static str {
    match state {
        PatchState::Clean => "not patched",
        PatchState::Patched => "patched",
        PatchState::StaleBackup => "not patched (obsolete backup found)",
        PatchState::PatchedMissingBackup => "patched (backup missing)",
    }
}

fn backup_label(state: PatchState) -> &'static str {
    match state {
        PatchState::Clean => "none",
        PatchState::Patched => "present",
        PatchState::StaleBackup => "obsolete",
        PatchState::PatchedMissingBackup => "missing",
    }
}
