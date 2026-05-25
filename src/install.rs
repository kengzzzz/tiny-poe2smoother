use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use sysinfo::System;

const POE2_APP_ID: &str = "2694490";

pub fn resolve_game_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    crate::timing!("resolve_game_dir");
    if let Some(path) = explicit {
        return validate_game_dir(path);
    }

    for candidate in steam_candidates() {
        let manifest = candidate
            .join("steamapps")
            .join(format!("appmanifest_{POE2_APP_ID}.acf"));
        if !manifest.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&manifest)
            .with_context(|| format!("failed to read {}", manifest.display()))?;
        let install_dir = parse_install_dir(&text).unwrap_or_else(|| "Path of Exile 2".to_string());
        let game_dir = candidate.join("steamapps").join("common").join(install_dir);
        if game_dir.exists() {
            return validate_game_dir(game_dir);
        }
    }

    bail!(
        "could not autodetect Path of Exile 2;\n\
         pass --game-dir <path> or verify Steam is installed\n\
         Searched: ~/.steam/steam, ~/.local/share/Steam, ~/.var/app/com.valvesoftware.Steam"
    );
}

pub fn validate_game_dir(path: PathBuf) -> Result<PathBuf> {
    let index = path.join("Bundles2").join("_.index.bin");
    if !index.exists() {
        return Err(anyhow!(
            "{} does not look like a POE2 install; missing {}",
            path.display(),
            index.display()
        ));
    }
    Ok(path)
}

pub fn ensure_game_not_running() -> Result<()> {
    if is_game_running() {
        bail!(
            "Path of Exile 2 appears to be running;\n\
             close the game (and Steam if needed) before apply/restore"
        );
    }
    Ok(())
}

pub fn is_game_running() -> bool {
    let mut system = System::new_all();
    system.refresh_all();
    system.processes().values().any(|process| {
        let name = process.name().to_string_lossy().to_ascii_lowercase();
        if name.contains("pathofexile") || name.contains("path of exile") {
            return true;
        }
        // Under Proton/Wine the process name is often a thread label like "Main",
        // but the command line still contains the game path.
        process.cmd().iter().any(|arg| {
            let arg = arg.to_string_lossy().to_ascii_lowercase();
            arg.contains("pathofexile") || arg.contains("path of exile")
        })
    })
}

fn steam_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".steam/steam"));
        out.push(home.join(".local/share/Steam"));
        out.push(home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(program_files_x86) = std::env::var_os("ProgramFiles(x86)") {
            out.push(PathBuf::from(program_files_x86).join("Steam"));
        }
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            out.push(PathBuf::from(program_files).join("Steam"));
        }
    }
    out
}

fn parse_install_dir(manifest: &str) -> Option<String> {
    manifest.lines().find_map(|line| {
        let mut parts = line.split('"').filter(|part| !part.trim().is_empty());
        match (parts.next(), parts.next()) {
            (Some("installdir"), Some(value)) => Some(value.to_string()),
            _ => None,
        }
    })
}

pub fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_steam_install_dir() {
        let manifest = "\"AppState\"\n{\n\t\"installdir\"\t\t\"Path of Exile 2\"\n}";
        assert_eq!(
            parse_install_dir(manifest).as_deref(),
            Some("Path of Exile 2")
        );
    }
}
