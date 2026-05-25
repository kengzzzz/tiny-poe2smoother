pub mod app;
pub mod backup;
pub mod bundle;
pub mod cli;
pub mod install;
pub mod patches;

use std::time::Instant;

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
