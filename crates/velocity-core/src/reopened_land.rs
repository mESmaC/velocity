//! Land-history guard for reopened issues (#615).
//!
//! #602 makes reopened issues dispatchable again when a person reopens a regression.
//! When an import (such as re-running Collapse's GitHub issue import) or non-person event
//! reopens an issue whose fix is already in Rosen's non-reverted land history after the
//! issue's last reopen by a person, continuous dispatch holds it and surfaces it in
//! project health as `"reopened, already landed in <commit>"`.

use std::collections::{BTreeMap, HashSet};

use crate::collapse::parse_rfc3339_seconds;
use crate::project_health::short_commit;

/// Clock/formatting tolerance (seconds) between Collapse's `issue_changes.created_at`
/// and `issues.updated_at` for a genuine human reopen. Human reopens set both to `now`,
/// whereas GitHub imports preserve GitHub's older `updated_at`.
pub const COLLAPSE_IMPORT_REOPEN_SKEW_SECS: u64 = 5;

/// One base commit from Rosen's land history (#615).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandHistoryEntry {
    pub commit: String,
    /// Commit author/land timestamp in UTC Unix seconds.
    pub landed_at: u64,
    /// First line of the commit message.
    pub summary: String,
}

/// Open-issue timestamps and reopen provenance evaluated against land history (#615).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpenIssueReopenCheck<'a> {
    pub number: u32,
    pub updated_at: &'a str,
    /// Timestamp of the last reopen by a person, if known.
    pub person_reopened_at: Option<&'a str>,
    /// `true` when Collapse's change feed recorded an import reopening this issue.
    pub reopened_by_import: bool,
    /// `true` when the issue host recorded a non-person (bot/automation) reopen.
    pub reopened_by_non_person: bool,
}

/// Non-reverted landed commit that already covers a reopened open issue (#615).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReopenedLandMatch {
    pub commit: String,
    pub landed_at: u64,
    pub summary: String,
    pub reopened_by_import: bool,
}

/// Whether `summary` is a revert commit subject (`Revert ...`).
pub fn is_revert_summary(summary: &str) -> bool {
    summary.trim_start().starts_with("Revert ")
}

/// Extracts issue numbers from parenthesized `(#N)` or `(#N, #M)` tokens in `summary`.
pub fn issue_numbers_in_parentheses(summary: &str) -> Vec<u32> {
    let mut numbers = Vec::new();
    let mut rest = summary;
    while let Some(open) = rest.find('(') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(')') else {
            break;
        };
        let inside = &after[..close];
        for token in inside.split(|ch: char| ch.is_whitespace() || ch == ',') {
            if let Some(digits) = token.strip_prefix('#')
                && !digits.is_empty()
                && digits.bytes().all(|byte| byte.is_ascii_digit())
                && let Ok(number) = digits.parse::<u32>()
                && number > 0
                && !numbers.contains(&number)
            {
                numbers.push(number);
            }
        }
        rest = &after[close + 1..];
    }
    numbers
}

/// Extracts issue numbers named by a landed change summary (`(#N)`), ignoring reverts (#615).
pub fn issue_numbers_in_land_summary(summary: &str) -> Vec<u32> {
    if is_revert_summary(summary) {
        return Vec::new();
    }
    issue_numbers_in_parentheses(summary)
}

/// Index of the newest non-reverted landed commit per issue number (#615).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LandHistoryIndex {
    latest_by_issue: BTreeMap<u32, LandHistoryEntry>,
}

impl LandHistoryIndex {
    /// Builds an index from base commits ordered newest first, excluding server-recorded
    /// reverted culprits (`reverted_commits`) and manual `Revert "..."` subjects.
    pub fn from_entries(
        newest_first: &[LandHistoryEntry],
        reverted_commits: &HashSet<String>,
    ) -> Self {
        let mut latest_by_issue = BTreeMap::new();
        let mut manual_revert_at: BTreeMap<u32, u64> = BTreeMap::new();
        for entry in newest_first {
            let lower = entry.commit.trim().to_ascii_lowercase();
            if lower.is_empty() || reverted_commits.contains(&lower) {
                continue;
            }
            if is_revert_summary(&entry.summary) {
                for number in issue_numbers_in_parentheses(&entry.summary) {
                    manual_revert_at
                        .entry(number)
                        .and_modify(|ts| *ts = (*ts).max(entry.landed_at))
                        .or_insert(entry.landed_at);
                }
                continue;
            }
            for number in issue_numbers_in_parentheses(&entry.summary) {
                if manual_revert_at
                    .get(&number)
                    .is_some_and(|&reverted_at| entry.landed_at <= reverted_at)
                {
                    continue;
                }
                latest_by_issue
                    .entry(number)
                    .or_insert_with(|| entry.clone());
            }
        }
        Self { latest_by_issue }
    }

    pub fn latest_land(&self, number: u32) -> Option<&LandHistoryEntry> {
        self.latest_by_issue.get(&number)
    }

    pub fn is_empty(&self) -> bool {
        self.latest_by_issue.is_empty()
    }
}

/// Whether a Collapse `issue.reopened` change-feed event came from a GitHub import rather
/// than a person reopening the issue on Collapse (#615).
///
/// On Collapse, a human reopen sets `issues.updated_at` and `issue_changes.created_at` to
/// the same `now`, whereas `issueimport` overwrites `issues.updated_at` with GitHub's
/// historical timestamp while recording `issue_changes.created_at` as the import time.
pub fn is_collapse_import_reopen(reopen_event_at: &str, issue_updated_at: &str) -> bool {
    let (Some(reopen_secs), Some(updated_secs)) = (
        parse_rfc3339_seconds(reopen_event_at),
        parse_rfc3339_seconds(issue_updated_at),
    ) else {
        return false;
    };
    reopen_secs > updated_secs.saturating_add(COLLAPSE_IMPORT_REOPEN_SKEW_SECS)
}

/// Checks whether an open issue should be held because Rosen's land history already
/// contains a non-reverted landed change named `(#N)` after its last reopen by a person (#615).
pub fn match_reopened_already_landed(
    issue: OpenIssueReopenCheck<'_>,
    index: &LandHistoryIndex,
) -> Option<ReopenedLandMatch> {
    let land = index.latest_land(issue.number)?;
    let person_reopened = issue.person_reopened_at.and_then(parse_rfc3339_seconds);
    let updated = parse_rfc3339_seconds(issue.updated_at);

    let covered = if let Some(reopened_secs) = person_reopened {
        land.landed_at > reopened_secs
    } else if issue.reopened_by_import || issue.reopened_by_non_person {
        true
    } else {
        updated.is_some_and(|updated_secs| land.landed_at >= updated_secs)
    };

    covered.then(|| ReopenedLandMatch {
        commit: land.commit.clone(),
        landed_at: land.landed_at,
        summary: land.summary.clone(),
        reopened_by_import: issue.reopened_by_import,
    })
}

/// Standard skip/hold reason shown in project health, dispatch logs and issue cards (#615).
pub fn reopened_already_landed_reason(commit: &str) -> String {
    format!("reopened, already landed in {}", short_commit(commit))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(commit: &str, landed_at: u64, summary: &str) -> LandHistoryEntry {
        LandHistoryEntry {
            commit: commit.into(),
            landed_at,
            summary: summary.into(),
        }
    }

    #[test]
    fn import_reopen_detection_distinguishes_stale_github_updated_at_from_human_reopen() {
        // Re-import sets change.created_at = 12:00 while issue.updated_at = GitHub's 09:00.
        assert!(is_collapse_import_reopen(
            "2026-11-22T12:00:00Z",
            "2026-11-22T09:00:00Z",
        ));
        // Human reopen on Collapse sets both updated_at and change.created_at to now.
        assert!(!is_collapse_import_reopen(
            "2026-11-22T13:00:00Z",
            "2026-11-22T13:00:00Z",
        ));
        // A human comment right after a human reopen moves updated_at even later.
        assert!(!is_collapse_import_reopen(
            "2026-11-22T13:00:00Z",
            "2026-11-22T13:02:00Z",
        ));
    }

    #[test]
    fn holds_import_reopened_issue_and_allows_person_reopen_after_land() {
        let land_time = parse_rfc3339_seconds("2026-11-22T10:00:00Z").unwrap_or(0);
        let index = LandHistoryIndex::from_entries(
            &[entry(
                "9b7f817abcdef1234567890abcdef12345678901",
                land_time,
                "Fix Move to Collapse bug (#609)",
            )],
            &HashSet::new(),
        );

        // Reopened by Collapse import: held with landing commit even if GitHub updated_at was newer.
        let import_reopened = match_reopened_already_landed(
            OpenIssueReopenCheck {
                number: 609,
                updated_at: "2026-11-22T11:00:00Z",
                person_reopened_at: None,
                reopened_by_import: true,
                reopened_by_non_person: false,
            },
            &index,
        );
        assert!(import_reopened.is_some());
        if let Some(found) = import_reopened {
            assert_eq!(found.commit, "9b7f817abcdef1234567890abcdef12345678901");
            assert!(found.reopened_by_import);
            assert_eq!(
                reopened_already_landed_reason(&found.commit),
                "reopened, already landed in 9b7f817abcde"
            );
        }

        // After restart without in-memory import flag, stale GitHub updated_at <= land_time still holds.
        let after_restart = match_reopened_already_landed(
            OpenIssueReopenCheck {
                number: 609,
                updated_at: "2026-11-22T09:00:00Z",
                person_reopened_at: None,
                reopened_by_import: false,
                reopened_by_non_person: false,
            },
            &index,
        );
        assert!(after_restart.is_some());

        // Person reopens regression AFTER the land commit: dispatchable (#602).
        let person_regression = match_reopened_already_landed(
            OpenIssueReopenCheck {
                number: 609,
                updated_at: "2026-11-22T13:00:00Z",
                person_reopened_at: Some("2026-11-22T13:00:00Z"),
                reopened_by_import: false,
                reopened_by_non_person: false,
            },
            &index,
        );
        assert!(person_regression.is_none());

        // Person reopened BEFORE the fix landed: land history already has the fix after reopen -> hold.
        let reopened_before_land = match_reopened_already_landed(
            OpenIssueReopenCheck {
                number: 609,
                updated_at: "2026-11-22T09:30:00Z",
                person_reopened_at: Some("2026-11-22T09:30:00Z"),
                reopened_by_import: false,
                reopened_by_non_person: false,
            },
            &index,
        );
        assert!(reopened_before_land.is_some());
    }

    #[test]
    fn excludes_auto_reverted_and_manually_reverted_lands_but_accepts_reland() {
        let t1 = 1_700_000_100;
        let t2 = 1_700_000_200;
        let t3 = 1_700_000_300;
        let reverted = HashSet::from(["aaaaaaa111111111111111111111111111111111".to_owned()]);

        // Auto-reverted commit for #610 must not hold #610.
        let auto_index = LandHistoryIndex::from_entries(
            &[
                entry(
                    "bbbbbbb222222222222222222222222222222222",
                    t2,
                    "Revert \"Fix (#610)\"",
                ),
                entry("aaaaaaa111111111111111111111111111111111", t1, "Fix (#610)"),
            ],
            &reverted,
        );
        assert!(auto_index.latest_land(610).is_none());

        // Manual revert without auto_reverts row also suppresses older land for #611.
        let manual_index = LandHistoryIndex::from_entries(
            &[
                entry(
                    "ddddddd222222222222222222222222222222222",
                    t2,
                    "Revert \"Fix (#611)\"",
                ),
                entry("ccccccc111111111111111111111111111111111", t1, "Fix (#611)"),
            ],
            &HashSet::new(),
        );
        assert!(manual_index.latest_land(611).is_none());

        // Re-landing AFTER a revert counts as the active fix for #611.
        let relanded_index = LandHistoryIndex::from_entries(
            &[
                entry(
                    "eeeeeee333333333333333333333333333333333",
                    t3,
                    "Re-fix (#611)",
                ),
                entry(
                    "ddddddd222222222222222222222222222222222",
                    t2,
                    "Revert \"Fix (#611)\"",
                ),
                entry("ccccccc111111111111111111111111111111111", t1, "Fix (#611)"),
            ],
            &HashSet::new(),
        );
        assert_eq!(
            relanded_index
                .latest_land(611)
                .map(|land| land.commit.as_str()),
            Some("eeeeeee333333333333333333333333333333333")
        );
    }
}
