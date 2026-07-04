use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use sysinfo::System;

const POE2_APP_ID: &str = "2694490";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallLayout {
    LooseBundles,
    ContentGgpk,
}

impl InstallLayout {
    pub fn label(self) -> &'static str {
        match self {
            Self::LooseBundles => "loose Bundles2",
            Self::ContentGgpk => "standalone Content.ggpk",
        }
    }
}

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

    for candidate in standalone_candidates() {
        if candidate.exists() {
            return validate_game_dir(candidate);
        }
    }

    bail!(
        "could not autodetect Path of Exile 2;\n\
         use Browse to choose the game folder, or verify Steam/standalone install paths\n\
         Searched default Steam folders, configured Steam libraries, and standalone locations"
    );
}

pub fn validate_game_dir(path: PathBuf) -> Result<PathBuf> {
    detect_install_layout(&path)?;
    Ok(path)
}

pub fn detect_install_layout(path: &Path) -> Result<InstallLayout> {
    let content = path.join("Content.ggpk");
    if content.exists() {
        return Ok(InstallLayout::ContentGgpk);
    }

    let index = path.join("Bundles2").join("_.index.bin");
    if index.exists() {
        return Ok(InstallLayout::LooseBundles);
    }

    Err(anyhow!(
        "{} does not look like a POE2 install; missing {} or {}",
        path.display(),
        index.display(),
        content.display()
    ))
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
    expand_steam_libraries(out)
}

fn standalone_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    #[cfg(target_os = "windows")]
    {
        if let Some(program_files_x86) = std::env::var_os("ProgramFiles(x86)") {
            push_unique(
                &mut out,
                &mut seen,
                PathBuf::from(program_files_x86)
                    .join("Grinding Gear Games")
                    .join("Path of Exile 2"),
            );
        }
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            push_unique(
                &mut out,
                &mut seen,
                PathBuf::from(program_files)
                    .join("Grinding Gear Games")
                    .join("Path of Exile 2"),
            );
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Some(prefix) = std::env::var_os("WINEPREFIX") {
            push_wine_standalone_candidates(&mut out, &mut seen, PathBuf::from(prefix));
        }
        if let Some(home) = dirs::home_dir() {
            push_wine_standalone_candidates(&mut out, &mut seen, home.join(".wine"));
        }
    }

    out
}

#[cfg(not(target_os = "windows"))]
fn push_wine_standalone_candidates(
    out: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    prefix: PathBuf,
) {
    for program_dir in ["Program Files (x86)", "Program Files"] {
        push_unique(
            out,
            seen,
            prefix
                .join("drive_c")
                .join(program_dir)
                .join("Grinding Gear Games")
                .join("Path of Exile 2"),
        );
    }
}

fn expand_steam_libraries(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        push_unique(&mut out, &mut seen, root.clone());
        let libraryfolders = root.join("steamapps").join("libraryfolders.vdf");
        let Ok(text) = std::fs::read_to_string(&libraryfolders) else {
            continue;
        };
        for library in parse_library_folders(&text) {
            push_unique(&mut out, &mut seen, library);
        }
    }
    out
}

fn push_unique(out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    if seen.insert(path.clone()) {
        out.push(path);
    }
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

fn parse_library_folders(vdf: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in vdf.lines() {
        let fields = quoted_fields(line);
        match fields.as_slice() {
            [key, value] if key == "path" => {
                out.push(PathBuf::from(value.replace("\\\\", "\\")));
            }
            [key, value] if key.chars().all(|ch| ch.is_ascii_digit()) => {
                if value.contains(':') || value.contains('/') || value.contains("\\\\") {
                    out.push(PathBuf::from(value.replace("\\\\", "\\")));
                }
            }
            _ => {}
        }
    }
    out
}

fn quoted_fields(line: &str) -> Vec<String> {
    line.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

pub fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn validates_loose_bundle_install() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("Path of Exile 2");
        fs::create_dir_all(game.join("Bundles2")).unwrap();
        fs::write(game.join("Bundles2/_.index.bin"), b"index").unwrap();

        assert_eq!(
            detect_install_layout(&game).unwrap(),
            InstallLayout::LooseBundles
        );
        assert_eq!(validate_game_dir(game.clone()).unwrap(), game);
    }

    #[test]
    fn validates_standalone_content_ggpk_install() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("Path of Exile 2");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("Content.ggpk"), b"ggpk").unwrap();

        assert_eq!(
            detect_install_layout(&game).unwrap(),
            InstallLayout::ContentGgpk
        );
        assert_eq!(validate_game_dir(game.clone()).unwrap(), game);
    }

    #[test]
    fn content_ggpk_layout_wins_when_overlay_index_exists() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("Path of Exile 2");
        fs::create_dir_all(game.join("Bundles2")).unwrap();
        fs::write(game.join("Content.ggpk"), b"ggpk").unwrap();
        fs::write(game.join("Bundles2/_.index.bin"), b"index").unwrap();

        assert_eq!(
            detect_install_layout(&game).unwrap(),
            InstallLayout::ContentGgpk
        );
    }

    #[test]
    fn rejects_unknown_install_layout() {
        let temp = tempfile::tempdir().unwrap();
        let err = validate_game_dir(temp.path().to_path_buf())
            .unwrap_err()
            .to_string();

        assert!(err.contains("Bundles2"));
        assert!(err.contains("Content.ggpk"));
    }

    #[test]
    fn parses_steam_install_dir() {
        let manifest = "\"AppState\"\n{\n\t\"installdir\"\t\t\"Path of Exile 2\"\n}";
        assert_eq!(
            parse_install_dir(manifest).as_deref(),
            Some("Path of Exile 2")
        );
    }

    #[test]
    fn parses_steam_library_folder_with_app_entry() {
        let vdf = r#"
"libraryfolders"
{
    "0"
    {
        "path"      "C:\\Program Files (x86)\\Steam"
        "apps"
        {
            "123" "1"
        }
    }
    "1"
    {
        "path"      "D:\\SteamLibrary"
        "apps"
        {
            "2694490" "123456"
        }
    }
}
"#;

        assert_eq!(
            parse_library_folders(vdf),
            vec![
                PathBuf::from(r"C:\Program Files (x86)\Steam"),
                PathBuf::from(r"D:\SteamLibrary")
            ]
        );
    }

    #[test]
    fn parses_legacy_steam_library_folder_entries() {
        let vdf = r#"
"LibraryFolders"
{
    "0" "C:\\Program Files (x86)\\Steam"
    "1" "D:\\SteamLibrary"
}
"#;

        assert_eq!(
            parse_library_folders(vdf),
            vec![
                PathBuf::from(r"C:\Program Files (x86)\Steam"),
                PathBuf::from(r"D:\SteamLibrary")
            ]
        );
    }
}
