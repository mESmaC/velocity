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

pub fn release_due(elapsed: u64, grace: u64, eligible: bool, protected: bool) -> bool {
    eligible && !protected && elapsed >= grace
}

#[cfg(test)]
mod tests {
    use super::{StallAction, release_due, stall_action};
    // A changed deadline guard can restart a running build or release work held by an
    // open PR. Check the runtime kernel against the Lean model's decision classes.
    #[test]
    fn lean_supervision_decisions() -> Result<(), Box<dyn std::error::Error>> {
        for line in include_str!("../../../proofs/fixtures/agent-activity.tsv").lines() {
            let fields: Vec<_> = line.split('\t').collect();
            let elapsed = fields[1].parse()?;
            let threshold = fields[2].parse()?;
            let first = fields[3].parse()?;
            let second = fields[4].parse()?;
            if fields[0] == "stall" {
                let expected = match fields[5] {
                    "nothing" => StallAction::Nothing,
                    "nudge" => StallAction::Nudge,
                    "restart" => StallAction::Restart,
                    _ => return Err("Invalid stall fixture".into()),
                };
                assert_eq!(stall_action(elapsed, threshold, first, second), expected);
            } else {
                assert_eq!(
                    release_due(elapsed, threshold, first, second),
                    fields[5].parse::<bool>()?
                );
            }
        }
        Ok(())
    }
}
