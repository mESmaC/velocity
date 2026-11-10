//! Pure F16 goal swarm work-choice and post-merge decisions (`SPEC.md` F16, F17).
//!
//! Native pool reading, channel locks and worktree git commands live at the outer edges;
//! this module decides which open piece an agent claims, what it does after a refusal,
//! when it leaves, and whether `keep_going` starts another round after a merge.

use std::collections::BTreeSet;

use crate::thread_titles::title_slug;
use crate::work_pool::{WorkKey, WorkPiece, WorkPool};

/// What an agent does after reading the claims board and work pool (F16 steps 2-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChoiceDecision {
    Claim(usize),
    Leave,
}

/// Reply from the atomic channel lock (`airc_claim`) on a single piece attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockReply {
    Granted,
    Refused,
}

/// Outcome of one claim step, including the immediate re-pick after a refusal (F16 step 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChoiceOutcome {
    Claimed(usize),
    Retry(usize),
    Left,
}

/// What a goal swarm agent does after its PR merges and its worktree is removed (F16 step 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostMergeDecision {
    Stop,
    Continue,
    LeaveEmpty,
}

/// Canonical lock key shared by the work pool and the AIRC claim board: collapses runs of
/// whitespace, lowercases known prefixes, and drops any leading `#` on issue numbers.
pub fn canonical_piece_key(piece: &str) -> String {
    let text = piece.split_whitespace().collect::<Vec<_>>().join(" ");
    let Some((prefix, rest)) = text.split_once(':') else {
        return text;
    };
    let prefix = prefix.trim().to_lowercase();
    let rest = rest.trim();
    match prefix.as_str() {
        "issue" => format!("issue:{}", rest.trim_start_matches('#').trim()),
        "task" | "spec" | "branch" => format!("{prefix}:{rest}"),
        _ => text,
    }
}

/// Picks the first unclaimed slot in pool order, or leaves only when none remain (F16).
pub fn choose_open_index(open_flags: &[bool]) -> ChoiceDecision {
    match open_flags.iter().position(|open| *open) {
        Some(index) => ChoiceDecision::Claim(index),
        None => ChoiceDecision::Leave,
    }
}

/// Resolves one lock attempt: a grant finishes; a refusal re-reads the refreshed board and
/// either retries the first open piece within one step or leaves if none remain (F16 step 3).
pub fn resolve_attempt(
    attempted: usize,
    reply: LockReply,
    refreshed_open: &[bool],
) -> ChoiceOutcome {
    match reply {
        LockReply::Granted => ChoiceOutcome::Claimed(attempted),
        LockReply::Refused => match choose_open_index(refreshed_open) {
            ChoiceDecision::Claim(next) => ChoiceOutcome::Retry(next),
            ChoiceDecision::Leave => ChoiceOutcome::Left,
        },
    }
}

/// Decides whether an agent stops, claims another piece in a fresh worktree, or posts `left`
/// after its PR merges (`SPEC.md` F16 step 5, `keep_going`).
pub fn decide_after_merge(keep_going: bool, refreshed_open: &[bool]) -> PostMergeDecision {
    if !keep_going {
        PostMergeDecision::Stop
    } else {
        match choose_open_index(refreshed_open) {
            ChoiceDecision::Claim(_) => PostMergeDecision::Continue,
            ChoiceDecision::Leave => PostMergeDecision::LeaveEmpty,
        }
    }
}

/// True when `piece` has no saved thread owner and its lock key is not held in AIRC.
pub fn is_piece_unclaimed(piece: &WorkPiece, held_keys: &BTreeSet<String>) -> bool {
    piece.thread_identifier.is_none()
        && !held_keys.contains(&canonical_piece_key(&piece.key.claim_piece()))
}

/// Availability bitmap for `pool` against the channel's held claim keys.
pub fn open_flags_for_pool(pool: &WorkPool, held_keys: &BTreeSet<String>) -> Vec<bool> {
    let normalized: BTreeSet<String> = held_keys
        .iter()
        .map(|key| canonical_piece_key(key))
        .collect();
    pool.pieces
        .iter()
        .map(|piece| is_piece_unclaimed(piece, &normalized))
        .collect()
}

/// Picks the first unclaimed piece in `pool`, or `None` when every piece is taken.
pub fn choose_pool_piece<'a>(
    pool: &'a WorkPool,
    held_keys: &BTreeSet<String>,
) -> Option<&'a WorkPiece> {
    let flags = open_flags_for_pool(pool, held_keys);
    match choose_open_index(&flags) {
        ChoiceDecision::Claim(index) => pool.pieces.get(index),
        ChoiceDecision::Leave => None,
    }
}

/// The `vel/`-less branch slug an agent renames to once its claim is granted (F16 step 3):
/// `issue-<n>` for GitHub issues, or `<title-slug>` for Task List items and SPEC sections.
pub fn piece_branch_slug(piece: &WorkPiece) -> String {
    match &piece.key {
        WorkKey::Issue { number, .. } => format!("issue-{number}"),
        WorkKey::Task { .. } | WorkKey::Spec { .. } => title_slug(&piece.title),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ChoiceDecision, ChoiceOutcome, LockReply, PostMergeDecision, choose_open_index,
        decide_after_merge, resolve_attempt,
    };

    fn parse_flags(raw: &str) -> Result<Vec<bool>, String> {
        if raw == "-" {
            return Ok(Vec::new());
        }
        raw.split(',')
            .map(|item| match item {
                "1" => Ok(true),
                "0" => Ok(false),
                other => Err(format!("unexpected flag {other}")),
            })
            .collect()
    }

    // A changed work-choice or keep_going decision could claim taken work, strand open
    // work after refusal, or stop a keep_going agent while open pieces remain.
    #[test]
    fn lean_work_choice_decisions() -> Result<(), Box<dyn std::error::Error>> {
        for line in include_str!("../../../proofs/fixtures/goal-swarm-work-choice.tsv").lines() {
            let fields: Vec<_> = line.split('\t').collect();
            let flags = parse_flags(fields[1])?;
            match fields[0] {
                "choose" => {
                    let expected = match fields[3].strip_prefix("claim:") {
                        Some(index) => ChoiceDecision::Claim(index.parse()?),
                        None => ChoiceDecision::Leave,
                    };
                    assert_eq!(choose_open_index(&flags), expected);
                }
                "attempt" => {
                    let (index, tag) = fields[2]
                        .split_once(':')
                        .ok_or("missing attempt separator")?;
                    let reply = if tag == "granted" {
                        LockReply::Granted
                    } else {
                        LockReply::Refused
                    };
                    let expected = if let Some(idx) = fields[3].strip_prefix("claimed:") {
                        ChoiceOutcome::Claimed(idx.parse()?)
                    } else if let Some(idx) = fields[3].strip_prefix("retry:") {
                        ChoiceOutcome::Retry(idx.parse()?)
                    } else {
                        ChoiceOutcome::Left
                    };
                    assert_eq!(resolve_attempt(index.parse()?, reply, &flags), expected);
                }
                "merge" => {
                    let expected = match fields[3] {
                        "stop" => PostMergeDecision::Stop,
                        "continue" => PostMergeDecision::Continue,
                        _ => PostMergeDecision::LeaveEmpty,
                    };
                    assert_eq!(decide_after_merge(fields[2].parse()?, &flags), expected);
                }
                other => return Err(format!("unknown fixture kind {other}").into()),
            }
        }
        Ok(())
    }
}
