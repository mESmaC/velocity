//! Issues parked under `no-change` leave review without a staffed review swarm (#506).
//! An overlap review whose land shares no code with the issue returns at once with the
//! land's files as evidence. Any other report gets one bounded review run, and a report
//! still pending after the limit returns to dispatch with a note.
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};
use velocity_github::{
    GitHubIssue, IssueClient, IssueState, ParkedOutcome, ParkedReview, overlap_clear_evidence,
};
use velocity_store::ProjectItem;
use velocity_threads::parked_review::{OverlappingLand, ParkedReviewRequest, ParkedReviews};

use super::{Reader, overlap, unix_seconds};
use crate::{
    state::ManagedStateStore,
    threads::{lock, report},
};

/// How long a report may hold an issue out of dispatch.
const PARKED_LIMIT_SECONDS: u64 = 2 * 60 * 60;
/// A report that could not be read or resolved is tried again after this long.
const RETRY: Duration = Duration::from_secs(60);
const RESOLVER: &str = "Velocity continuous dispatch";
const LIMIT_NOTE: &str = "No review finished within two hours of this report, so the issue \
    returns to dispatch unreviewed. The next agent should read the report above and check the \
    acceptance criteria against the current base before changing anything.";

/// What dispatch last read about each parked issue, by issue number.
pub(super) type Parked = BTreeMap<u32, Entry>;

#[derive(Default)]
pub(super) struct Entry {
    /// The issue version the report was read at. A newer version is read again.
    updated_at: Option<String>,
    failed: Option<Instant>,
    /// `None` when the label has no report, such as one a person added by hand.
    report: Option<Report>,
}

struct Report {
    review: ParkedReview,
    /// Unix seconds when dispatch first read the report, for one whose time is unreadable.
    seen: u64,
    /// Whether an overlap review's land was compared with the issue's code.
    land_checked: bool,
    /// The land of an overlap review that shares code with the issue.
    land: Option<OverlappingLand>,
}

pub(super) fn refresh(
    application: &AppHandle,
    project: &ProjectItem,
    reader: &mut Reader,
) -> Result<(), String> {
    if project.continuous_dispatch.slots == 0 || reader.backlog.error.is_some() {
        return Ok(());
    }
    let Some(client) = reader.client.as_ref() else {
        return Ok(());
    };
    let waiting: Vec<_> = reader
        .backlog
        .issues
        .iter()
        .filter(|issue| issue.state == IssueState::Open && is_parked(issue))
        .cloned()
        .collect();
    reader
        .parked
        .retain(|number, _| waiting.iter().any(|issue| issue.number == *number));
    if waiting.is_empty() {
        return Ok(());
    }
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let context = Context {
        application,
        project,
        client,
        data: &data,
    };
    for issue in &waiting {
        let entry = reader.parked.entry(issue.number).or_default();
        if context.check_after_failure(entry, issue) {
            // The label is gone on GitHub: read the backlog again before dispatching.
            reader.refreshed = None;
        }
    }
    Ok(())
}

fn is_parked(issue: &GitHubIssue) -> bool {
    issue
        .labels
        .iter()
        .any(|label| label.eq_ignore_ascii_case(IssueClient::NO_CHANGE_LABEL))
}

struct Context<'a> {
    application: &'a AppHandle,
    project: &'a ProjectItem,
    client: &'a IssueClient,
    data: &'a Path,
}

impl Context<'_> {
    /// `true` when the issue left review in this pass. A failure is reported and tried
    /// again later: one unreadable issue must not hold up the rest of the dispatch pass.
    fn check_after_failure(&self, entry: &mut Entry, issue: &GitHubIssue) -> bool {
        if entry.failed.is_some_and(|failed| failed.elapsed() < RETRY) {
            return false;
        }
        entry.failed = None;
        self.check(entry, issue).unwrap_or_else(|error| {
            entry.failed = Some(Instant::now());
            report::<()>(Err(format!(
                "Could not resolve the parked review of {}#{}: {error}",
                self.client.repository(),
                issue.number
            )));
            false
        })
    }

    fn check(&self, entry: &mut Entry, issue: &GitHubIssue) -> Result<bool, String> {
        if entry.updated_at.as_deref() != Some(issue.updated_at.as_str()) {
            entry.report = self
                .client
                .parked_review(issue.number)
                .map_err(|error| error.to_string())?
                .map(|review| Report {
                    review,
                    seen: unix_seconds(),
                    land_checked: false,
                    land: None,
                });
            entry.updated_at = Some(issue.updated_at.clone());
        }
        let Some(report) = &mut entry.report else {
            return Ok(false);
        };
        let since = report.review.reported_at.unwrap_or(report.seen);
        let expired = unix_seconds().saturating_sub(since) >= PARKED_LIMIT_SECONDS;
        if !report.land_checked {
            match self.resolve_clear_land(report, issue) {
                Ok(true) => return Ok(true),
                Ok(false) => report.land_checked = true,
                // A land that cannot be read must not hold the issue past the limit.
                Err(error) if !expired => return Err(error),
                Err(_) => {}
            }
        }
        if expired {
            self.resolve(issue.number, &report.review, LIMIT_NOTE)?;
            return Ok(true);
        }
        self.start_review(issue, report)?;
        Ok(false)
    }

    /// Returns an overlap review to dispatch when its land changed none of the issue's
    /// code. Otherwise keeps the land for the bounded review's prompt.
    fn resolve_clear_land(&self, report: &mut Report, issue: &GitHubIssue) -> Result<bool, String> {
        let Some((source, commit)) = report.review.overlapping_land() else {
            return Ok(false);
        };
        let evidence = overlap::land_evidence(
            self.client,
            self.project,
            self.data,
            issue,
            (source, commit),
        )?;
        if evidence.clear {
            let note = overlap_clear_evidence(source, commit, &evidence.files);
            self.resolve(issue.number, &report.review, &note)?;
            return Ok(true);
        }
        report.land = Some(OverlappingLand {
            source,
            commit: commit.to_owned(),
            files: evidence.files,
        });
        Ok(false)
    }

    fn resolve(&self, number: u32, review: &ParkedReview, note: &str) -> Result<(), String> {
        self.client
            .resolve_no_change(
                number,
                &review.attempt,
                RESOLVER,
                note,
                ParkedOutcome::Returned,
                &[],
            )
            .map_err(|error| error.to_string())
    }

    /// Starts the report's one bounded review when the project's review slot is free.
    fn start_review(&self, issue: &GitHubIssue, parked: &Report) -> Result<(), String> {
        let project = self.project;
        if project.continuous_dispatch.paused || velocity_rosen::memory::is_low() {
            return Ok(());
        }
        let Some(reviews) = self.application.try_state::<ParkedReviews>() else {
            return Ok(());
        };
        let attempt = &parked.review.attempt;
        if !reviews.available(project.id, issue.number, attempt) {
            return Ok(());
        }
        let (settings, worktrees) = {
            let store = lock(
                self.application.state::<ManagedStateStore>().inner(),
                "persisted state",
            )?;
            (store.settings(), store.worktree_preferences(project))
        };
        let request = ParkedReviewRequest {
            settings,
            project: project.clone(),
            worktrees,
            repository: self.client.repository().to_owned(),
            issue: issue.number,
            title: issue.title.clone(),
            attempt: attempt.clone(),
            land: parked.land.clone(),
        };
        if reviews.reserve(project.id, issue.number, attempt) {
            let reviews = reviews.inner().clone();
            tauri::async_runtime::spawn(async move { report(reviews.run(request).await) });
        }
        Ok(())
    }
}
