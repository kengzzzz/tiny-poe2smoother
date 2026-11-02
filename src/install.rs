use anyhow::{Result, anyhow, bail};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System, UpdateKind};

const POE2_APP_ID: &str = "2694490";
const STANDALONE_DIRS: &[&str] = &["Path of Exile 2", "Path of Exile 2 - poe2_production"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallLayout {
    LooseBundles,
    ContentGgpk,
}

pub fn resolve_game_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    crate::timing!("resolve_game_dir");
    if let Some(path) = explicit {
        return validate_game_dir(path);
    }

    resolve_from_candidates(steam_candidates(), standalone_candidates)
}

fn resolve_from_candidates(
    steam: Vec<PathBuf>,
    standalone: impl FnOnce() -> Vec<PathBuf>,
) -> Result<PathBuf> {
    for candidate in steam {
        let manifest = candidate
            .join("steamapps")
            .join(format!("appmanifest_{POE2_APP_ID}.acf"));
        if !manifest.exists() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            tracing::warn!("skipping unreadable {}", manifest.display());
            continue;
        };
        let install_dir = parse_install_dir(&text).unwrap_or_else(|| "Path of Exile 2".to_string());
        let game_dir = candidate.join("steamapps").join("common").join(install_dir);
        if game_dir.exists()
            && let Ok(valid) = validate_game_dir(game_dir.clone())
        {
            return Ok(valid);
        }
    }

    for candidate in standalone() {
        if candidate.exists()
            && let Ok(valid) = validate_game_dir(candidate.clone())
        {
            return Ok(valid);
        }
    }

    bail!(
        "could not autodetect Path of Exile 2.\n\
         Use Browse to choose the game folder, or verify Steam/standalone install paths\n\
         Searched default Steam folders, configured Steam libraries, and standalone locations"
    );
}

/// For comparing installs only, since Windows adds a `\\?\` prefix.
pub(crate) fn normalized_game_dir(game_dir: &Path) -> PathBuf {
    std::fs::canonicalize(game_dir)
        .or_else(|_| std::path::absolute(game_dir))
        .unwrap_or_else(|_| game_dir.to_path_buf())
}

pub fn validate_game_dir(path: PathBuf) -> Result<PathBuf> {
    detect_install_layout(&path)?;
    Ok(path)
}

pub fn detect_install_layout(path: &Path) -> Result<InstallLayout> {
    let content = path.join("Content.ggpk");
    if content.is_file() {
        return Ok(InstallLayout::ContentGgpk);
    }

    let index = path.join("Bundles2").join("_.index.bin");
    if index.is_file() {
        return Ok(InstallLayout::LooseBundles);
    }

    Err(anyhow!(
        "{} does not look like a POE2 install. Missing {} or {}",
        path.display(),
        index.display(),
        content.display()
    ))
}

pub fn ensure_game_not_running(game_dir: &Path) -> Result<()> {
    if is_game_running(Some(game_dir)) {
        bail!(
            "Path of Exile 2 appears to be running.\n\
             Close the game (and Steam if needed) before apply/restore"
        );
    }
    Ok(())
}

pub fn is_game_running(game_dir: Option<&Path>) -> bool {
    let kind = ProcessRefreshKind::nothing().without_tasks();
    #[cfg(windows)]
    let kind = kind.with_exe(UpdateKind::Always);
    // Under Wine the exe is Wine's loader, so the game's path comes from the first argument.
    #[cfg(not(windows))]
    let kind = kind.with_cmd(UpdateKind::Always);
    let system = System::new_with_specifics(RefreshKind::nothing().with_processes(kind));
    let game = game_dir.and_then(GameFolder::read);
    // The app and its launchers match when it sits in the game folder.
    let mut ours = HashSet::new();
    let mut next = sysinfo::get_current_pid().ok();
    while let Some(pid) = next {
        if !ours.insert(pid) {
            break;
        }
        next = system.process(pid).and_then(|process| process.parent());
    }
    system.processes().iter().any(|(pid, process)| {
        if ours.contains(pid) || process.thread_kind().is_some() {
            return false;
        }
        if is_steam_launch(process.cmd()) {
            return true;
        }
        #[cfg(windows)]
        let (program, location) = (process.name(), process.exe());
        #[cfg(not(windows))]
        let Some((program, location)) = process
            .cmd()
            .first()
            .map(|program| (program.as_os_str(), Some(Path::new(program))))
        else {
            return false;
        };
        runs_game_program(*pid, program, location, game.as_ref())
    })
}

/// Steam on Linux runs the game under `reaper SteamLaunch AppId=2694490 -- ...`.
fn is_steam_launch(cmd: &[OsString]) -> bool {
    let end = cmd.iter().position(|arg| arg == "--").unwrap_or(cmd.len());
    let launcher = &cmd[..end];
    launcher
        .first()
        .is_some_and(|program| Path::new(program).file_name() == Some(OsStr::new("reaper")))
        && launcher.iter().any(|arg| arg == "SteamLaunch")
        && launcher.iter().any(|arg| arg == "AppId=2694490")
}

struct GameFolder {
    forms: Vec<String>,
    programs: Vec<GameProgram>,
}

struct GameProgram {
    name: String,
    id: Option<FileId>,
}

impl GameFolder {
    fn read(dir: &Path) -> Option<Self> {
        let programs = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let name = program_name(&entry.file_name());
                name.ends_with(".exe").then(|| GameProgram {
                    name,
                    id: file_id(&entry.path()),
                })
            })
            .collect();
        let mut forms = vec![comparable_path(&dir.to_string_lossy())];
        if let Ok(canonical) = std::fs::canonicalize(dir) {
            let canonical = comparable_path(&canonical.to_string_lossy());
            if !forms.contains(&canonical) {
                forms.push(canonical);
            }
        }
        Some(Self { forms, programs })
    }
}

/// Path of Exile 1 uses the same program names, so the running file must be this folder's.
fn runs_game_program(
    pid: Pid,
    program: &OsStr,
    location: Option<&Path>,
    game: Option<&GameFolder>,
) -> bool {
    let name = program_name(program);
    let Some(game) = game else {
        return is_game_name(&name);
    };
    let Some(ours) = game.programs.iter().find(|ours| ours.name == name) else {
        return false;
    };
    let same_file = ours
        .id
        .as_ref()
        .and_then(|ours| running_file_id(pid, location, &name).map(|theirs| *ours == theirs));
    let in_folder =
        location.and_then(|location| folder_match(&location.to_string_lossy(), &game.forms));
    game_verdict(&name, same_file, in_folder)
}

/// Identity can break on overlay mounts and paths on symlinks, so both must say elsewhere.
fn game_verdict(name: &str, same_file: Option<bool>, in_folder: Option<bool>) -> bool {
    match (same_file, in_folder) {
        (Some(true), _) | (_, Some(true)) => true,
        (Some(false), Some(false)) => false,
        _ => is_game_name(name),
    }
}

fn is_game_name(name: &str) -> bool {
    name.starts_with("pathofexile") && name.ends_with(".exe")
}

fn program_name(program: &OsStr) -> String {
    let program = program.to_string_lossy();
    let name = program.rsplit(['/', '\\']).next().unwrap_or_default();
    name.to_ascii_lowercase()
}

fn folder_match(program: &str, forms: &[String]) -> Option<bool> {
    let program = comparable_path(program);
    let bytes = program.as_bytes();
    let drive = bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && &bytes[1..3] == b":/";
    if !drive && !program.starts_with('/') {
        return None;
    }
    // Wine drive roots are unknown (Proton maps the Steam library to S:), so compare folder tails.
    #[cfg(not(windows))]
    if drive {
        let folder = &program[2..program.rfind('/')?];
        return (folder.len() > 1).then(|| forms.iter().any(|dir| dir.ends_with(folder)));
    }
    Some(forms.iter().any(|dir| {
        program.len() > dir.len()
            && program.starts_with(dir.as_str())
            && program.as_bytes()[dir.len()] == b'/'
    }))
}

fn comparable_path(path: &str) -> String {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let path = if let Some(rest) = path.strip_prefix("//?/unc/") {
        format!("//{rest}")
    } else if let Some(rest) = path.strip_prefix("//?/") {
        rest.to_string()
    } else {
        path
    };
    path.trim_end_matches('/').to_string()
}

#[cfg(windows)]
type FileId = PathBuf;

#[cfg(windows)]
fn file_id(path: &Path) -> Option<FileId> {
    std::fs::canonicalize(path).ok()
}

#[cfg(windows)]
fn running_file_id(_pid: Pid, location: Option<&Path>, _name: &str) -> Option<FileId> {
    location
        .filter(|exe| !exe.as_os_str().is_empty())
        .and_then(file_id)
}

#[cfg(not(windows))]
type FileId = (u64, u64);

#[cfg(not(windows))]
fn file_id(path: &Path) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

/// Wine maps the program file, and `/proc/<pid>/root` resolves it in container mounts.
#[cfg(not(windows))]
fn running_file_id(pid: Pid, _location: Option<&Path>, name: &str) -> Option<FileId> {
    let maps = std::fs::read_to_string(format!("/proc/{pid}/maps")).ok()?;
    let path = maps
        .lines()
        .filter_map(mapped_path)
        .find(|path| program_name(OsStr::new(path)) == name)?;
    file_id(Path::new(&format!("/proc/{pid}/root{path}")))
}

/// Five fields come before the path, and the path may contain spaces.
#[cfg(not(windows))]
fn mapped_path(line: &str) -> Option<&str> {
    let mut rest = line;
    for _ in 0..5 {
        rest = rest.trim_start();
        rest = &rest[rest.find(' ')?..];
    }
    let path = rest.trim_start();
    let path = path.strip_suffix(" (deleted)").unwrap_or(path);
    path.starts_with('/').then_some(path)
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
        out.extend(registry_steam_roots());
        if let Some(program_files_x86) = std::env::var_os("ProgramFiles(x86)") {
            out.push(PathBuf::from(program_files_x86).join("Steam"));
        }
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            out.push(PathBuf::from(program_files).join("Steam"));
        }
    }
    expand_steam_libraries(out)
}

/// Steam may be installed outside Program Files.
#[cfg(target_os = "windows")]
fn registry_steam_roots() -> Vec<PathBuf> {
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6432KEY,
        RegGetValueW,
    };

    fn read(root: HKEY, key: &str, value: &str, flags: u32) -> Option<PathBuf> {
        use std::os::windows::ffi::OsStringExt;
        let key: Vec<u16> = key.encode_utf16().chain(std::iter::once(0)).collect();
        let value: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let mut buf = vec![0u16; 1024];
        let mut bytes = (buf.len() * 2) as u32;
        let status = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ | flags,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if status != 0 {
            return None;
        }
        let len = (bytes as usize / 2).saturating_sub(1);
        let path = PathBuf::from(std::ffi::OsString::from_wide(&buf[..len]));
        (!path.as_os_str().is_empty()).then_some(path)
    }

    [
        read(HKEY_CURRENT_USER, "Software\\Valve\\Steam", "SteamPath", 0),
        read(
            HKEY_LOCAL_MACHINE,
            "SOFTWARE\\Valve\\Steam",
            "InstallPath",
            RRF_SUBKEY_WOW6432KEY,
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn standalone_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    #[cfg(target_os = "windows")]
    {
        if let Some(program_files_x86) = std::env::var_os("ProgramFiles(x86)") {
            push_standalone_candidates_from_program_files(
                &mut out,
                &mut seen,
                PathBuf::from(program_files_x86),
            );
        }
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            push_standalone_candidates_from_program_files(
                &mut out,
                &mut seen,
                PathBuf::from(program_files),
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
            if let Some(user) = home.file_name().and_then(|name| name.to_str()) {
                push_mounted_windows_standalone_candidates(
                    &mut out,
                    &mut seen,
                    PathBuf::from("/run/media").join(user),
                );
                push_mounted_windows_standalone_candidates(
                    &mut out,
                    &mut seen,
                    PathBuf::from("/media").join(user),
                );
            }
        }
        push_mounted_windows_standalone_candidates(&mut out, &mut seen, PathBuf::from("/mnt"));
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
        push_standalone_candidates_from_program_files(
            out,
            seen,
            prefix.join("drive_c").join(program_dir),
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn push_mounted_windows_standalone_candidates(
    out: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    mount_root: PathBuf,
) {
    push_mounted_windows_drive_candidates(out, seen, &mount_root);

    let Ok(entries) = std::fs::read_dir(&mount_root) else {
        return;
    };
    for entry in entries.flatten() {
        push_mounted_windows_drive_candidates(out, seen, &entry.path());
    }
}

#[cfg(not(target_os = "windows"))]
fn push_mounted_windows_drive_candidates(
    out: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    drive_root: &Path,
) {
    for program_dir in ["Program Files (x86)", "Program Files"] {
        push_standalone_candidates_from_program_files(out, seen, drive_root.join(program_dir));
    }
}

fn push_standalone_candidates_from_program_files(
    out: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    program_files: PathBuf,
) {
    let ggg_dir = program_files.join("Grinding Gear Games");
    for dir in STANDALONE_DIRS {
        push_unique(out, seen, ggg_dir.join(dir));
    }

    let Ok(entries) = std::fs::read_dir(&ggg_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.is_dir()
            && (name == "Path of Exile 2" || name.starts_with("Path of Exile 2 -"))
            && detect_install_layout(&path).is_ok()
        {
            push_unique(out, seen, path);
        }
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
            [key, value]
                if key.chars().all(|ch| ch.is_ascii_digit())
                    && (value.contains(':') || value.contains('/') || value.contains("\\\\")) =>
            {
                out.push(PathBuf::from(value.replace("\\\\", "\\")));
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
    let text = path.to_string_lossy();
    // Hide the `\\?\` prefix of canonical Windows paths.
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn display_path_hides_windows_verbatim_prefixes() {
        assert_eq!(
            display_path(Path::new(r"\\?\C:\Games\PoE2")),
            r"C:\Games\PoE2"
        );
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share\PoE2")),
            r"\\server\share\PoE2"
        );
        assert_eq!(
            display_path(Path::new("/home/deck/PoE2")),
            "/home/deck/PoE2"
        );
    }

    #[test]
    fn autodetect_skips_invalid_steam_install_and_prefers_next_library() {
        let temp = tempfile::tempdir().unwrap();
        let libraries = [temp.path().join("old"), temp.path().join("current")];
        for library in &libraries {
            fs::create_dir_all(library.join("steamapps/common/Path of Exile 2")).unwrap();
            fs::write(
                library.join("steamapps/appmanifest_2694490.acf"),
                "\"installdir\" \"Path of Exile 2\"",
            )
            .unwrap();
        }
        let game = libraries[1].join("steamapps/common/Path of Exile 2");
        fs::create_dir_all(game.join("Bundles2")).unwrap();
        fs::write(game.join("Bundles2/_.index.bin"), b"index").unwrap();

        let detected = resolve_from_candidates(libraries.to_vec(), || {
            panic!("valid Steam install should take precedence over standalone discovery")
        })
        .unwrap();
        assert_eq!(detected, game);
    }

    #[test]
    fn autodetect_skips_invalid_standalone_candidate() {
        let temp = tempfile::tempdir().unwrap();
        let invalid = temp.path().join("invalid");
        let game = temp.path().join("valid");
        fs::create_dir_all(invalid.join("Content.ggpk")).unwrap();
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("Content.ggpk"), b"ggpk").unwrap();

        assert_eq!(
            resolve_from_candidates(Vec::new(), || vec![invalid, game.clone()]).unwrap(),
            game
        );
    }

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
    fn standalone_candidates_include_production_suffix() {
        let temp = tempfile::tempdir().unwrap();
        let program_files = temp.path().join("Program Files (x86)");
        let expected = program_files
            .join("Grinding Gear Games")
            .join("Path of Exile 2 - poe2_production");
        let mut out = Vec::new();
        let mut seen = HashSet::new();

        push_standalone_candidates_from_program_files(&mut out, &mut seen, program_files);

        assert!(out.contains(&expected));
    }

    #[test]
    fn standalone_candidates_discover_valid_path_of_exile_children() {
        let temp = tempfile::tempdir().unwrap();
        let program_files = temp.path().join("Program Files");
        let game = program_files
            .join("Grinding Gear Games")
            .join("Path of Exile 2 - test_channel");
        fs::create_dir_all(game.join("Bundles2")).unwrap();
        fs::write(game.join("Bundles2/_.index.bin"), b"index").unwrap();
        let mut out = Vec::new();
        let mut seen = HashSet::new();

        push_standalone_candidates_from_program_files(&mut out, &mut seen, program_files);

        assert!(out.contains(&game));
    }

    #[test]
    fn standalone_candidates_ignore_similar_prefix_without_channel_separator() {
        let temp = tempfile::tempdir().unwrap();
        let program_files = temp.path().join("Program Files");
        let similar = program_files
            .join("Grinding Gear Games")
            .join("Path of Exile 2backup");
        fs::create_dir_all(similar.join("Bundles2")).unwrap();
        fs::write(similar.join("Bundles2/_.index.bin"), b"index").unwrap();
        let mut out = Vec::new();
        let mut seen = HashSet::new();

        push_standalone_candidates_from_program_files(&mut out, &mut seen, program_files);

        assert!(!out.contains(&similar));
    }

    #[test]
    fn standalone_candidates_ignore_space_suffix_without_dash() {
        let temp = tempfile::tempdir().unwrap();
        let program_files = temp.path().join("Program Files");
        let similar = program_files
            .join("Grinding Gear Games")
            .join("Path of Exile 2 backup");
        fs::create_dir_all(&similar).unwrap();
        fs::write(similar.join("Content.ggpk"), b"ggpk").unwrap();
        let mut out = Vec::new();
        let mut seen = HashSet::new();

        push_standalone_candidates_from_program_files(&mut out, &mut seen, program_files);

        assert!(!out.contains(&similar));
    }

    #[test]
    fn detect_layout_rejects_directory_markers() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("Path of Exile 2");
        fs::create_dir_all(game.join("Content.ggpk")).unwrap();
        fs::create_dir_all(game.join("Bundles2").join("_.index.bin")).unwrap();

        assert!(detect_install_layout(&game).is_err());
    }

    #[test]
    fn standalone_candidates_ignore_unrelated_children() {
        let temp = tempfile::tempdir().unwrap();
        let program_files = temp.path().join("Program Files");
        let unrelated = program_files
            .join("Grinding Gear Games")
            .join("Path of Exile 1");
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(unrelated.join("Content.ggpk"), b"ggpk").unwrap();
        let mut out = Vec::new();
        let mut seen = HashSet::new();

        push_standalone_candidates_from_program_files(&mut out, &mut seen, program_files);

        assert!(!out.contains(&unrelated));
    }

    #[test]
    fn standalone_candidates_deduplicate_fixed_and_discovered_paths() {
        let temp = tempfile::tempdir().unwrap();
        let program_files = temp.path().join("Program Files (x86)");
        let game = program_files
            .join("Grinding Gear Games")
            .join("Path of Exile 2 - poe2_production");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("Content.ggpk"), b"ggpk").unwrap();
        let mut out = Vec::new();
        let mut seen = HashSet::new();

        push_standalone_candidates_from_program_files(&mut out, &mut seen, program_files);

        assert_eq!(out.iter().filter(|path| *path == &game).count(), 1);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn mounted_windows_candidates_include_standalone_production_suffix() {
        let temp = tempfile::tempdir().unwrap();
        let volume = temp.path().join("WindowsDrive");
        let game = volume
            .join("Program Files (x86)")
            .join("Grinding Gear Games")
            .join("Path of Exile 2 - poe2_production");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("Content.ggpk"), b"ggpk").unwrap();
        let mut out = Vec::new();
        let mut seen = HashSet::new();

        push_mounted_windows_standalone_candidates(&mut out, &mut seen, temp.path().to_path_buf());

        assert!(out.contains(&game));
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

    fn no_process() -> Pid {
        Pid::from_u32(u32::MAX)
    }

    fn game_dir(root: &Path, name: &str, programs: &[&str]) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        for program in programs {
            fs::write(dir.join(program), name).unwrap();
        }
        dir
    }

    fn runs(program: &str, location: Option<&Path>, game: Option<&GameFolder>) -> bool {
        runs_game_program(no_process(), OsStr::new(program), location, game)
    }

    #[test]
    fn running_check_ignores_programs_that_only_mention_the_game() {
        let temp = tempfile::tempdir().unwrap();
        let poe2 = game_dir(
            temp.path(),
            "Path of Exile 2",
            &["PathOfExile.exe", "Client.exe"],
        );
        let game = GameFolder::read(&poe2).unwrap();
        for program in [
            "zsh",
            "/usr/bin/nvim",
            "/usr/lib/firefox/firefox",
            r"C:\Windows\System32\notepad.exe",
            r"c:\windows\system32\steam.exe",
        ] {
            assert!(!runs(program, Some(Path::new(program)), Some(&game)));
        }
    }

    #[test]
    fn running_check_needs_both_checks_to_call_a_program_elsewhere() {
        assert!(game_verdict("client.exe", Some(true), Some(false)));
        assert!(game_verdict("client.exe", Some(false), Some(true)));
        assert!(!game_verdict("pathofexile.exe", Some(false), Some(false)));
        assert!(game_verdict("pathofexile.exe", Some(false), None));
        assert!(game_verdict("pathofexile.exe", None, Some(false)));
        assert!(!game_verdict("client.exe", None, Some(false)));
        assert!(!game_verdict("client.exe", None, None));
    }

    #[test]
    fn running_check_falls_back_to_the_program_name() {
        let temp = tempfile::tempdir().unwrap();
        let poe2 = game_dir(
            temp.path(),
            "Path of Exile 2",
            &["PathOfExile.exe", "Client.exe"],
        );
        let game = GameFolder::read(&poe2).unwrap();
        assert!(runs("PathOfExile.exe", None, Some(&game)));
        assert!(!runs("Client.exe", None, Some(&game)));
        assert!(!runs("PathOfExileSteam.exe", None, Some(&game)));
        assert!(runs("PathOfExileSteam.exe", None, None));
        assert!(!runs("Client.exe", None, None));
    }

    #[test]
    fn running_check_reads_the_folder_programs() {
        let temp = tempfile::tempdir().unwrap();
        let poe1 = game_dir(temp.path(), "Path of Exile", &["PathOfExile.exe"]);
        let poe2 = game_dir(
            temp.path(),
            "Path of Exile 2",
            &["PathOfExile.exe", "Client.exe", "fmod.dll"],
        );
        let game = GameFolder::read(&poe2).unwrap();
        let mut names: Vec<&str> = game
            .programs
            .iter()
            .map(|program| program.name.as_str())
            .collect();
        names.sort();
        assert_eq!(names, ["client.exe", "pathofexile.exe"]);
        let ours = game
            .programs
            .iter()
            .find(|program| program.name == "pathofexile.exe")
            .unwrap();
        assert!(ours.id.is_some());
        assert_ne!(ours.id, file_id(&poe1.join("PathOfExile.exe")));
        assert_eq!(ours.id, file_id(&poe2.join("PathOfExile.exe")));
    }

    #[test]
    fn running_check_trusts_only_steams_launcher_for_the_app_id() {
        let cmd = |args: &[&str]| args.iter().map(OsString::from).collect::<Vec<_>>();
        let reaper = "/home/u/.local/share/Steam/ubuntu12_32/reaper";
        assert!(is_steam_launch(&cmd(&[
            reaper,
            "SteamLaunch",
            "AppId=2694490",
            "--",
            "proton"
        ])));
        assert!(!is_steam_launch(&cmd(&[
            "bash",
            "-c",
            "echo",
            "AppId=2694490"
        ])));
        assert!(!is_steam_launch(&cmd(&[
            reaper,
            "SteamLaunch",
            "AppId=238960",
            "--",
            "wrapper",
            "AppId=2694490"
        ])));
    }

    #[test]
    fn running_check_matches_the_folder_of_a_path() {
        let forms = [comparable_path(r"C:\Games\Path of Exile 2")];
        assert_eq!(
            folder_match(r"C:\Games\Path of Exile 2\Client.exe", &forms),
            Some(true)
        );
        assert_eq!(
            folder_match(r"C:\Games\Path of Exile\Client.exe", &forms),
            Some(false)
        );
        assert_eq!(
            folder_match(
                r"C:\Games\Path of Exile 2 - poe2_production\Client.exe",
                &forms
            ),
            Some(false)
        );
        assert_eq!(folder_match("Client.exe", &forms), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn running_check_matches_wine_paths_on_any_drive() {
        let steam = [comparable_path(
            "/home/u/.local/share/Steam/steamapps/common/Path of Exile 2",
        )];
        let proton = r"S:\steamapps\common\Path of Exile 2\PathOfExileSteam.exe";
        assert_eq!(folder_match(proton, &steam), Some(true));
        let first_game = r"S:\steamapps\common\Path of Exile\PathOfExileSteam.exe";
        assert_eq!(folder_match(first_game, &steam), Some(false));
        let z_drive = r"Z:\home\u\.local\share\Steam\steamapps\common\Path of Exile 2\Client.exe";
        assert_eq!(folder_match(z_drive, &steam), Some(true));
        assert_eq!(folder_match(r"D:\PathOfExile.exe", &steam), None);
        let prefix = [comparable_path(
            "/home/u/Games/poe2/drive_c/Program Files (x86)/Grinding Gear Games/Path of Exile 2",
        )];
        let client = r"C:\Program Files (x86)\Grinding Gear Games\Path of Exile 2\Client.exe";
        assert_eq!(folder_match(client, &prefix), Some(true));
    }

    #[cfg(not(windows))]
    #[test]
    fn running_check_parses_mapped_paths() {
        let line = "7f00-7f10 r--p 00000000 00:2a 1234                       /games/Path of Exile 2/PathOfExile.exe";
        assert_eq!(
            mapped_path(line),
            Some("/games/Path of Exile 2/PathOfExile.exe")
        );
        let deleted = "7f00-7f10 r--p 00000000 00:2a 1234 /games/a b/x.exe (deleted)";
        assert_eq!(mapped_path(deleted), Some("/games/a b/x.exe"));
        assert_eq!(mapped_path("7f00-7f10 rw-p 00000000 00:00 0 "), None);
        assert_eq!(
            mapped_path("7ffd-7ffe rw-p 00000000 00:00 0                  [stack]"),
            None
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn running_check_finds_the_mapped_program_of_a_process() {
        let exe = std::env::current_exe().unwrap();
        let name = program_name(exe.file_name().unwrap());
        let pid = Pid::from_u32(std::process::id());
        assert_eq!(running_file_id(pid, None, &name), file_id(&exe));
        assert_eq!(running_file_id(no_process(), None, &name), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn running_check_sees_through_symlinked_folders() {
        let temp = tempfile::tempdir().unwrap();
        let poe2 = game_dir(temp.path(), "Path of Exile 2", &["PathOfExile.exe"]);
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&poe2, &alias).unwrap();
        let game = GameFolder::read(&poe2).unwrap();
        assert_eq!(game.programs[0].id, file_id(&alias.join("PathOfExile.exe")));
    }

    #[cfg(windows)]
    #[test]
    fn running_check_tells_path_of_exile_1_from_2() {
        let temp = tempfile::tempdir().unwrap();
        let poe1 = game_dir(temp.path(), "Path of Exile", &["PathOfExile.exe"]);
        let poe2 = game_dir(
            temp.path(),
            "Path of Exile 2",
            &["PathOfExile.exe", "Client.exe"],
        );
        let game = GameFolder::read(&poe2).unwrap();
        let exe = |dir: &Path, name: &str| dir.join(name);
        assert!(runs(
            "PathOfExile.exe",
            Some(&exe(&poe2, "PathOfExile.exe")),
            Some(&game)
        ));
        assert!(runs(
            "Client.exe",
            Some(&exe(&poe2, "Client.exe")),
            Some(&game)
        ));
        assert!(!runs(
            "PathOfExile.exe",
            Some(&exe(&poe1, "PathOfExile.exe")),
            Some(&game)
        ));
        assert!(runs("PathOfExile.exe", Some(Path::new("")), Some(&game)));
    }
}
