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
        }
    }
}
