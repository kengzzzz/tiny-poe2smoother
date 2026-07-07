pub mod app;
pub mod backup;
pub mod bundle;
pub mod install;
pub mod patches;

use std::time::Instant;

/// Install the global tracing subscriber, writing to stderr. `RUST_LOG`
/// selects a plain level (`error`..`trace`, no per-target filters — the
/// env-filter feature costs an extra regex engine); defaults to `warn`.
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
