//! Project-visible Rosen mirror failures (F5, #402).

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct MirrorState {
    pub rejected: bool,
    pub error: String,
    /// Decimal Unix milliseconds, preserved as a string across IPC.
    pub retry_at_ms: Option<String>,
    pub last_pushed_commit: Option<String>,
}

impl MirrorState {
    pub fn new(
        rejected: bool,
        error: &str,
        retry_at_ms: Option<String>,
        last_pushed_commit: Option<String>,
    ) -> Self {
        Self {
            rejected,
            error: error.lines().take(3).collect::<Vec<_>>().join("\n"),
            retry_at_ms,
            last_pushed_commit,
        }
    }
}
