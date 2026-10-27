pub mod app;
pub mod backup;
pub mod bundle;
pub mod install;
pub mod patches;

use anyhow::{Context, Result, anyhow, bail};
use std::io::Write;
use std::path::Path;
use std::time::Instant;

/// `RUST_LOG` takes a plain level only (no env-filter). Defaults to `warn`.
pub fn init_tracing() {
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(tracing::level_filters::LevelFilter::WARN);
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .try_init();
}

const TEMP_SUFFIX: &str = ".tps-tmp";

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        bail!("not a file path: {}", path.display());
    };
    // tempfile calls Win32 directly, so long paths need the `\\?\` form.
    let parent = std::fs::canonicalize(parent)
        .with_context(|| format!("failed to resolve {}", parent.display()))?;
    let target = parent.join(name);
    let prefix = format!(".{}.", name.to_string_lossy());
    // Remove temp files left by a killed write.
    if let Ok(entries) = std::fs::read_dir(&parent) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            if file_name.starts_with(&prefix) && file_name.ends_with(TEMP_SUFFIX) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let mut temp = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(TEMP_SUFFIX)
        .tempfile_in(&parent)?;
    // tempfile creates 0600 files.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&target).map_or(0o644, |meta| meta.permissions().mode());
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    temp.write_all(bytes)?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    temp.persist(&target)
        .map_err(|err| anyhow!("failed to replace {}: {}", path.display(), err.error))?;
    Ok(())
}

pub struct TimingScope {
    label: &'static str,
    start: Instant,
}

impl TimingScope {
    pub fn new(label: &'static str) -> Self {
        let start = Instant::now();
        tracing::debug!("{}: start", label);
        Self { label, start }
    }
}

impl Drop for TimingScope {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        tracing::debug!("{}: {:.2?}", self.label, elapsed);
    }
}

#[macro_export]
macro_rules! timing {
    ($label:expr) => {
        let _timing = $crate::TimingScope::new($label);
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_clears_its_own_leftovers_only() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("_.index.bin");
        let leftover = temp.path().join("._.index.bin.a1b2c3.tps-tmp");
        let unrelated = [
            temp.path().join(".tmpAbC123"),
            temp.path().join(".other.bin.a1b2c3.tps-tmp"),
        ];
        std::fs::write(&leftover, b"").unwrap();
        for path in &unrelated {
            std::fs::write(path, b"keep").unwrap();
        }

        atomic_write(&target, b"index").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"index");
        assert!(!leftover.exists());
        for path in &unrelated {
            assert!(path.exists(), "{}", path.display());
        }
        let names: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 3, "{names:?}");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_keeps_the_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let existing = temp.path().join("existing.bin");
        std::fs::write(&existing, b"old").unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o640)).unwrap();
        let fresh = temp.path().join("fresh.bin");

        atomic_write(&existing, b"new").unwrap();
        atomic_write(&fresh, b"new").unwrap();

        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&existing), 0o640);
        assert_eq!(mode(&fresh), 0o644);
    }
}
