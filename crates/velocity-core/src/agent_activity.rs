//! Pure supervision decisions for SPEC.md §2.2. Clock values are elapsed seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StallAction {
    Nothing,
    Nudge,
    Restart,
}

pub fn stall_action(silence: u64, timeout: u64, nudged: bool, exempt: bool) -> StallAction {
    if exempt || silence < timeout {
        StallAction::Nothing
    } else if nudged {
        StallAction::Restart
    } else {
        StallAction::Nudge
    }
}

/// Lifecycle errors and limits start the grace clock; silence alone never does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Available,
    Failed,
    AtLimit,
}

impl Availability {
    pub fn from_status(failed: bool, at_limit: bool) -> Self {
        if failed {
            Self::Failed
        } else if at_limit {
            Self::AtLimit
        } else {
            Self::Available
        }
    }

    /// An available observation breaks the continuous interval, including when
    /// a PR, parking or a build-queue exemption previously protected the claims.
    pub fn checkpoint(self, previous: Option<u64>, now: u64) -> Option<u64> {
        match self {
            Self::Available => None,
            Self::Failed | Self::AtLimit => Some(previous.unwrap_or(now)),
        }
    }

    pub fn release_due(
        self,
        since: Option<u64>,
        now: u64,
        grace: u64,
        protected: bool,
        released: bool,
    ) -> bool {
        self != Self::Available
            && !protected
            && !released
            && since.is_some_and(|start| now.saturating_sub(start) >= grace)
    }
}
