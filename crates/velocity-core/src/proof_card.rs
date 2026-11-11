//! The proof card on a PR (SPEC.md F17 "Proof card", F6 PR history): what Velocity's own
//! proof checks found at one PR head, the theorems the PR proves and how firmly each
//! language's code is tied to the proven model. `velocity-proofs` builds it; the thread view
//! renders it under the PR's "opened" card and the PR track's "Proved" step reads it.

/// How one of the card's checks stands.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProofCheckState {
    Passed,
    Failed,
    /// Not decided yet: the statement lock is taken only when PR duty approves.
    #[default]
    Pending,
}

/// One of the card's checks: lake build, no sorry or admit, standard axioms, statements
/// locked at review (and the design proof's SPEC.md section, shown only when it fails).
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProofCheck {
    pub label: String,
    pub state: ProofCheckState,
    /// Why it failed or is pending; empty when it passed.
    pub detail: String,
}

impl ProofCheck {
    pub fn new(label: &str, state: ProofCheckState, detail: &str) -> Self {
        Self {
            label: label.to_owned(),
            state,
            detail: detail.to_owned(),
        }
    }
}

/// One proven theorem, or one fixture table the PR adds or changes.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProofTheorem {
    /// `Design`, `Confidence`, `Fixture`, or the file's own name for any other proof file.
    pub kind: String,
    pub name: String,
    pub statement: String,
}

impl ProofTheorem {
    pub fn new(kind: &str, name: &str, statement: &str) -> Self {
        Self {
            kind: kind.to_owned(),
            name: name.to_owned(),
            statement: statement.to_owned(),
        }
    }
}

/// How firmly a language's code is tied to the proven model (SPEC.md §2 "Proof level").
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProofLevelKind {
    #[default]
    Model,
    Reference,
    Code,
}

impl ProofLevelKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Reference => "reference",
            Self::Code => "code",
        }
    }
}

/// One language's row on the card.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProofLevel {
    pub language: String,
    pub level: ProofLevelKind,
    pub detail: String,
    /// A model-only level for a language that has a stronger one available (F17): left to
    /// the reviewer, never a block on its own.
    pub flagged: bool,
}

impl ProofLevel {
    pub fn new(language: &str, level: ProofLevelKind, detail: &str, flagged: bool) -> Self {
        Self {
            language: language.to_owned(),
            level,
            detail: detail.to_owned(),
            flagged,
        }
    }
}

/// The proof card for one PR head.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProofCard {
    /// The PR head the checks ran on.
    pub head: String,
    /// The proof folders the PR changes, such as `proofs/ClaimLock/`.
    pub folders: Vec<String>,
    pub theorem_count: u32,
    /// Every check that decides a merge passed (the statement lock follows at approval).
    pub proved: bool,
    pub checks: Vec<ProofCheck>,
    pub theorems: Vec<ProofTheorem>,
    pub levels: Vec<ProofLevel>,
}

/// The label of the statement-lock check, which turns from pending to passed when PR duty
/// approves this head.
pub const STATEMENT_LOCK_CHECK: &str = "statements locked at review";

impl ProofCard {
    pub fn new(head: &str, folders: Vec<String>) -> Self {
        Self {
            head: head.to_owned(),
            folders,
            ..Self::default()
        }
    }

    /// Ticks the statement-lock check once PR duty approved exactly this head, without
    /// running the other checks again. Returns whether the card changed.
    pub fn locked_at(&mut self, approved_head: &str) -> bool {
        if approved_head != self.head {
            return false;
        }
        let Some(check) = self
            .checks
            .iter_mut()
            .find(|check| check.label == STATEMENT_LOCK_CHECK)
        else {
            return false;
        };
        if check.state == ProofCheckState::Passed {
            return false;
        }
        *check = ProofCheck::new(STATEMENT_LOCK_CHECK, ProofCheckState::Passed, "");
        true
    }

    /// The one-line summary the `proved` event carries: the folders, the theorem count and
    /// each language's level.
    pub fn summary(&self) -> String {
        let mut parts = vec![self.folders.join(", ")];
        parts.push(match self.theorem_count {
            1 => "1 theorem".to_owned(),
            count => format!("{count} theorems"),
        });
        parts.extend(
            self.levels
                .iter()
                .map(|level| format!("{} {}", level.language, level.level.label())),
        );
        parts.retain(|part| !part.is_empty());
        parts.join(" · ")
    }
}
