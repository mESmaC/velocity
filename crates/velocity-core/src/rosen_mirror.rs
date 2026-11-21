//! Project-visible Rosen mirror failures (F5, #402).

use std::time::Duration;

/// Background mirror tip check interval for GitHub mirrors and recently active Collapse
/// mirrors (#520, #572).
pub const MIRROR_POLL_INTERVAL: Duration = Duration::from_secs(2 * 60);

/// Background mirror tip check interval for a Collapse mirror after an hour with no lands (#572).
pub const COLLAPSE_MIRROR_IDLE_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// How long to wait before the next background `ls-remote` tip check for a project (#572).
///
/// GitHub mirrors keep the fixed 2-minute interval. Collapse mirrors check every 2 minutes
/// right after a land, then step past Collapse's 5-minute idle timeout (10, 15, 20 minutes)
/// and settle at 30 minutes after an hour with no lands so Collapse's server can stop.
pub fn mirror_poll_interval(collapse_mirror: bool, since_last_land: Option<Duration>) -> Duration {
    if !collapse_mirror {
        return MIRROR_POLL_INTERVAL;
    }
    let Some(elapsed) = since_last_land else {
        return COLLAPSE_MIRROR_IDLE_INTERVAL;
    };
    if elapsed < Duration::from_secs(3 * 60) {
        MIRROR_POLL_INTERVAL
    } else if elapsed < Duration::from_secs(15 * 60) {
        Duration::from_secs(10 * 60)
    } else if elapsed < Duration::from_secs(30 * 60) {
        Duration::from_secs(15 * 60)
    } else if elapsed < Duration::from_secs(60 * 60) {
        Duration::from_secs(20 * 60)
    } else {
        COLLAPSE_MIRROR_IDLE_INTERVAL
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct MirrorState {
    pub rejected: bool,
    pub error: String,
    /// Decimal Unix milliseconds, preserved as a string across IPC.
    pub retry_at_ms: Option<String>,
    pub last_pushed_commit: Option<String>,
    /// The Rosen base tip that could not be pushed, shown beside `last_pushed_commit` (#509).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
}

/// A project whose Rosen base trails its Git mirror while a catch-up is pending or
/// running (#520). Kept in memory for the current run and cleared as soon as the base
/// catches up or diverges.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct MirrorBehind {
    /// How many commits the mirror branch holds beyond Rosen's base.
    pub commits: u32,
    /// Rosen's current base commit.
    pub base_commit: String,
    /// The mirror branch tip ahead of the base.
    pub mirror_commit: String,
    /// Whether a catch-up is running right now (versus waiting for a retry).
    pub catching_up: bool,
    /// Why the latest catch-up attempt did not finish, while a retry waits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl MirrorBehind {
    pub fn new(
        commits: u32,
        base_commit: String,
        mirror_commit: String,
        catching_up: bool,
        error: Option<String>,
    ) -> Self {
        Self {
            commits,
            base_commit,
            mirror_commit,
            catching_up,
            error: error
                .map(|text| text.lines().take(3).collect::<Vec<_>>().join("\n"))
                .filter(|text| !text.trim().is_empty()),
        }
    }
}

/// A base and mirror branch that have diverged, found by catching up from the mirror
/// (#501). Nothing moved; a person reconciles them or adopts the mirror tip (Rosen #396).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct MirrorDivergence {
    /// The tip of Rosen's base.
    pub base_commit: String,
    /// The tip of the mirror branch.
    pub mirror_commit: String,
    /// Rosen's refusal: both tips and the commits each side has that the other lacks.
    pub detail: String,
}

impl MirrorDivergence {
    /// Reads Rosen's diverged refusal (`rosen mirror catch-up`, Rosen #416), which names
    /// `Rosen base <commit>` and `Mirror tip <commit>`. `None` for any other refusal.
    pub fn from_refusal(message: &str) -> Option<Self> {
        if !message.contains("have diverged") {
            return None;
        }
        Some(Self {
            base_commit: commit_after(message, "Rosen base ")?,
            mirror_commit: commit_after(message, "Mirror tip ")?,
            detail: message.trim_end().to_owned(),
        })
    }
}

/// The hexadecimal commit ID that follows the first `label` in `message`.
fn commit_after(message: &str, label: &str) -> Option<String> {
    let start = message.find(label)? + label.len();
    let commit: String = message[start..]
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .collect();
    (commit.len() >= 40).then_some(commit)
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
            base_commit: None,
        }
    }

    pub fn with_base_commit(mut self, base_commit: Option<String>) -> Self {
        self.base_commit = base_commit;
        self
    }
}
