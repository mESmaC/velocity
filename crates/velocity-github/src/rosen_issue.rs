//! GitHub comments and the durable effects of landed Rosen issue work.
use serde::Deserialize;
use serde_json::{Value, json};
use velocity_core::issue_dependencies::IssueReference;
use velocity_core::issue_holds::{
    UpstreamTarget, is_blocked_upstream_label, is_person_hold_label, is_tracking_label,
};

use crate::issue_dependencies::waits_line_for;
use crate::issues::IssueBackend;
use crate::{GitHubCommandError, GitHubIssue, IssueClient, IssueState};

const READY_LABEL: &str = "ready-to-close";
const NO_CHANGE_MARKER: &str = "<!-- velocity-issue-no-change:";
/// The attempt of an overlap review: `overlap:<blocking issue>:<landed commit>`.
const OVERLAP_ATTEMPT: &str = "overlap:";
/// A land can change hundreds of files; the comment names the first of them.
const LISTED_LAND_FILES: usize = 40;
/// The status context Velocity posts on a commit its integration build passed.
pub(crate) const INTEGRATION_CONTEXT: &str = "velocity/integration";
/// How long another machine's recent starting/progress comment keeps an issue out of
/// dispatch before a stalled remote run is retried (#519).
pub const REMOTE_PROGRESS_WINDOW_SECONDS: u64 = 2 * 60 * 60;

/// What an issue's GitHub comments show another machine (or a deleted thread) has landed
/// or is actively working on (#504, #519).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternalIssueState {
    /// Comments show a Velocity land marker with no later reopen, close or non-fixed
    /// parked review.
    Landed {
        commit: String,
        /// Unix seconds when the land comment was posted, when readable.
        landed_at: Option<u64>,
    },
    /// Comments show a recent `issue-N` starting/progress comment with no later ended,
    /// failed, landed, reopened, closed, dependency-wait or needs-owner comment.
    InProgressElsewhere {
        /// Unix seconds when the latest active comment was posted.
        started_at: u64,
    },
}

/// A green `velocity/integration` status on GitHub covering a landed commit (#519).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationCoverage {
    pub commit: String,
    /// Unix seconds when GitHub recorded the green status, when readable.
    pub passed_at: Option<u64>,
}

pub enum RosenIssueEffect<'a> {
    Landed { change: &'a str },
    Reviewed { by: &'a str },
    Watcher,
    Reverted { reason: &'a str },
}

/// Outcome of applying a saved Rosen event effect to an issue on the configured host (#614).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RosenIssueFollowOutcome {
    /// Comment and/or state change succeeded (or was already recorded).
    Applied,
    /// Mirror has not yet caught up to the landed commit; retry later.
    WaitingForMirror,
    /// The host issue at this number does not match the landed change's title or GitHub source.
    Mismatch {
        host_title: String,
        host_url: String,
    },
}

/// How an issue parked under `no-change` leaves review (#506, #526).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParkedOutcome {
    /// The current base already meets the acceptance criteria, so the issue closes.
    AlreadyFixed,
    /// Work remains: the issue goes back to dispatch with instructions or a note.
    Returned,
    /// The work waits on an upstream PR or release, checked daily by dispatch (#526).
    BlockedUpstream { target: UpstreamTarget },
    /// The referenced spec section was removed; close as `not_planned` citing the commit (#526).
    SpecRemoved {
        spec_section: String,
        removed_commit: String,
    },
    /// The work needs a person (hardware or explicit scope hold), shown on Work with its release action (#526).
    Hold { release_action: String },
}

impl ParkedOutcome {
    /// The last field of the review marker.
    fn marker(&self) -> &'static str {
        match self {
            Self::AlreadyFixed => "true",
            Self::Returned => "false",
            Self::BlockedUpstream { .. } => "blocked-upstream",
            Self::SpecRemoved { .. } => "not-planned",
            Self::Hold { .. } => "hold",
        }
    }
}

/// The report that parked an issue under `no-change`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParkedReview {
    pub attempt: String,
    /// Unix seconds of the report comment. `None` when GitHub's time could not be read.
    pub reported_at: Option<u64>,
}

impl ParkedReview {
    /// The blocking issue and its landed commit when the report is an overlap review.
    pub fn overlapping_land(&self) -> Option<(u32, &str)> {
        let (source, commit) = self
            .attempt
            .strip_prefix(OVERLAP_ATTEMPT)?
            .split_once(':')?;
        Some((source.parse().ok()?, commit))
    }
}

struct IssueComment {
    body: String,
    created_at: String,
}

impl IssueClient {
    pub const NO_CHANGE_LABEL: &str = "no-change";

    pub fn open_no_change_issues(&self) -> Result<Vec<GitHubIssue>, GitHubCommandError> {
        Ok(self
            .open_issues()?
            .into_iter()
            .filter(issue_has_no_change_label)
            .collect())
    }

    /// `blockers` are the issues this one waits on (#511). They are linked as issue
    /// dependencies before the comment, so a failed link posts nothing and the agent's
    /// retry does both.
    pub fn post_issue_progress(
        &self,
        number: u32,
        byline: &str,
        body: &str,
        ready: bool,
        blockers: &[IssueReference],
    ) -> Result<String, GitHubCommandError> {
        let issue = self.read_issue(number)?;
        let waits = waits_line_for(self.host(), &self.link_blockers(number, blockers)?);
        if ready {
            self.label_issue(number, READY_LABEL)?;
        }
        let heading = if ready { "Ready to close · " } else { "" };
        let url = self.add_issue_comment(
            number,
            &issue.id,
            &format!("{heading}{byline}\n\n{body}{waits}"),
        )?;
        crate::issue_cache::invalidate_discussion(self.host(), self.repository(), number);
        if ready {
            crate::issue_cache::invalidate(self.host(), self.repository());
        }
        Ok(url)
    }

    /// An issue that waits on open blockers gets the comment without the `no-change`
    /// label (#511): its dependencies already hold it, and dispatch starts it when they
    /// close with no label to remove.
    pub fn post_no_change(
        &self,
        number: u32,
        change: &str,
        byline: &str,
        reason: &str,
    ) -> Result<(), GitHubCommandError> {
        self.post_no_change_with_blocker_check(
            number,
            change,
            byline,
            reason,
            GitHubIssue::has_open_blockers,
        )
    }

    /// Like `post_no_change`, but lets the caller treat integration-green blockers as
    /// satisfied so an issue whose only open blockers already passed integration still
    /// takes `no-change` instead of looping dispatch (#529).
    pub fn post_no_change_with_blocker_check(
        &self,
        number: u32,
        change: &str,
        byline: &str,
        reason: &str,
        has_blocking_dependencies: impl FnOnce(&GitHubIssue) -> bool,
    ) -> Result<(), GitHubCommandError> {
        self.post_issue_for_verification(
            number,
            change,
            ("Ended with no change", byline, reason),
            has_blocking_dependencies,
        )
    }

    pub fn post_overlap_review(
        &self,
        number: u32,
        source: u32,
        commit: &str,
    ) -> Result<(), GitHubCommandError> {
        let reason = format!(
            "This issue waited for #{source}. Rosen commit `{commit}` landed for that issue. \
             Check this issue's acceptance criteria against the current base. Close it if \
             the fix is verified, or return it to dispatch with the remaining work."
        );
        self.post_issue_for_verification(
            number,
            &format!("{OVERLAP_ATTEMPT}{source}:{commit}"),
            (
                "Check against an overlapping land",
                "Velocity continuous dispatch",
                reason.as_str(),
            ),
            |_| false,
        )
    }

    /// Evidence that a land this issue waited for changed none of its code (#506). The
    /// issue takes no label and goes straight back to dispatch.
    pub fn post_overlap_clear(
        &self,
        number: u32,
        source: u32,
        commit: &str,
        files: &[String],
    ) -> Result<(), GitHubCommandError> {
        let marker = format!("<!-- velocity-overlap-clear:{source}:{commit} -->");
        let body = format!(
            "## Returned to dispatch after an overlapping land\n\nVelocity continuous dispatch\n\n{}",
            overlap_clear_evidence(source, commit, files)
        );
        self.sync_rosen_issue(number, &marker, &body, None)
    }

    /// A dispatch thread failed before its agent's first turn (#514). The issue takes no
    /// label: it is already back in dispatch, and the comment says why the thread failed
    /// and what happens next. `attempt` names the thread, so a retried post after an
    /// ambiguous response reuses its comment.
    pub fn post_launch_failure(
        &self,
        number: u32,
        attempt: &str,
        byline: &str,
        reason: &str,
        next: &str,
    ) -> Result<(), GitHubCommandError> {
        let marker = format!("<!-- velocity-launch-failed:{attempt} -->");
        let body = format!("## Thread failed before it started\n\n{byline}\n\n{reason}\n\n{next}");
        self.sync_rosen_issue(number, &marker, &body, None)
    }

    /// `has_blocking_dependencies` is whether unsatisfied open blockers already hold the
    /// issue so it should not take `no-change`.
    fn post_issue_for_verification(
        &self,
        number: u32,
        attempt: &str,
        (heading, byline, reason): (&str, &str, &str),
        has_blocking_dependencies: impl FnOnce(&GitHubIssue) -> bool,
    ) -> Result<(), GitHubCommandError> {
        let marker = format!("{NO_CHANGE_MARKER}{attempt} -->");
        if self.has_rosen_comment(number, &marker)? {
            // A reviewer may have removed the label before the local acknowledgement.
            return Ok(());
        }
        let issue = self.read_issue(number)?;
        let already_no_change = issue_has_no_change_label(&issue);
        let has_hold = issue_has_hold_state(&issue);
        let has_blockers = has_blocking_dependencies(&issue);
        // An issue already held or tracking keeps that state without adding `no-change` (#506, #526).
        let added_no_change =
            issue.state == IssueState::Open && !already_no_change && !has_hold && !has_blockers;
        if added_no_change {
            self.label_issue(number, Self::NO_CHANGE_LABEL)?;
        }
        let hold_note = no_change_hold_note(
            heading == "Ended with no change" && issue.state == IssueState::Open,
            added_no_change || already_no_change,
            has_hold,
            has_blockers,
        );
        let end = reason.floor_char_boundary(60_000);
        let truncated = if end < reason.len() {
            "\n\n[Message truncated.]"
        } else {
            ""
        };
        self.add_issue_comment(
            number,
            &issue.id,
            &format!(
                "## {heading}\n\n{byline}\n\n{}{truncated}{hold_note}\n\n{marker}",
                &reason[..end]
            ),
        )?;
        crate::issue_cache::invalidate_discussion(self.host(), self.repository(), number);
        Ok(())
    }

    pub fn no_change_discussion(
        &self,
        number: u32,
    ) -> Result<(GitHubIssue, Vec<String>, String), GitHubCommandError> {
        let issue = self.read_issue(number)?;
        if issue.state != IssueState::Open || !issue_has_no_change_label(&issue) {
            return Err(GitHubCommandError::Invalid(
                "The issue is no longer waiting for no-change review".into(),
            ));
        }
        let comments = self.issue_comment_bodies(number)?;
        let (report, attempt) = comments
            .iter()
            .rev()
            .find_map(|body| Some((body, report_attempt(body)?.to_owned())))
            .ok_or_else(|| {
                GitHubCommandError::Missing("The no-change report is still pending".into())
            })?;
        let mut discussion = vec![report.clone()];
        discussion.extend(
            comments
                .iter()
                .rev()
                .filter(|body| *body != report)
                .take(2)
                .cloned(),
        );
        Ok((issue, discussion, attempt))
    }

    /// The latest report that parked `number`, for an issue the caller saw labelled
    /// `no-change`. `None` when no report comment exists, such as a label added by hand.
    pub fn parked_review(&self, number: u32) -> Result<Option<ParkedReview>, GitHubCommandError> {
        Ok(self
            .issue_comments(number)?
            .iter()
            .rev()
            .find_map(|comment| {
                Some(ParkedReview {
                    attempt: report_attempt(&comment.body)?.to_owned(),
                    reported_at: unix_seconds(&comment.created_at),
                })
            }))
    }

    /// Ends the review of the current report. `reviewer` names who decided, such as
    /// ``Velocity review swarm · `maple` `` or `Velocity continuous dispatch`.
    /// `blockers` are issues the review found this one waits on (#511): they are linked
    /// as GitHub dependencies, and an issue that only waits on them returns to dispatch
    /// instead of taking a hold (#526).
    pub fn resolve_no_change(
        &self,
        number: u32,
        attempt: &str,
        reviewer: &str,
        evidence: &str,
        outcome: ParkedOutcome,
        blockers: &[IssueReference],
    ) -> Result<(), GitHubCommandError> {
        let (_, _, current) = self.no_change_discussion(number)?;
        if current != attempt || evidence.trim().is_empty() {
            return Err(GitHubCommandError::Invalid(
                "Review the current report and supply verification, retry instructions or hold evidence".into(),
            ));
        }
        let marker = format!(
            "<!-- velocity-no-change-review:{current}:{} -->",
            outcome.marker()
        );
        match outcome {
            ParkedOutcome::AlreadyFixed if !blockers.is_empty() => {
                Err(GitHubCommandError::Invalid(
                    "An already fixed issue waits on no other issue".into(),
                ))
            }
            ParkedOutcome::AlreadyFixed => {
                let body = format!("{reviewer} verified already fixed.\n\n{evidence}");
                self.sync_rosen_issue(number, &marker, &body, Some(IssueState::Closed))
            }
            ParkedOutcome::Returned => {
                let waits = waits_line_for(self.host(), &self.link_blockers(number, blockers)?);
                let body = format!("{reviewer} returned to dispatch.\n\n{evidence}{waits}");
                self.sync_rosen_issue(number, &marker, &body, None)?;
                self.remove_issue_label(number, Self::NO_CHANGE_LABEL)
            }
            ParkedOutcome::BlockedUpstream { target } => {
                self.post_blocked_upstream(number, reviewer, &target, evidence, blockers)?;
                Ok(())
            }
            ParkedOutcome::SpecRemoved {
                spec_section,
                removed_commit,
            } => {
                if !blockers.is_empty() {
                    return Err(GitHubCommandError::Invalid(
                        "An orphaned spec issue being closed waits on no other issue".into(),
                    ));
                }
                self.close_not_planned(number, reviewer, &spec_section, &removed_commit, evidence)?;
                Ok(())
            }
            ParkedOutcome::Hold { release_action } => {
                self.post_person_hold(number, reviewer, evidence, &release_action, blockers)?;
                Ok(())
            }
        }
    }

    pub(crate) fn label_issue(&self, number: u32, name: &str) -> Result<(), GitHubCommandError> {
        match &self.backend {
            IssueBackend::Collapse(collapse) => {
                collapse.ensure_label(name)?;
                collapse.update_issue(
                    number,
                    velocity_collapse::issues::UpdateIssueRequest {
                        add_labels: vec![name.to_owned()],
                        ..Default::default()
                    },
                )?;
                Ok(())
            }
            IssueBackend::GitHub(transport) => {
                let labels = format!("repos/{}/labels", self.repository());
                let label = format!("{labels}/{name}");
                if transport.command.read::<Value>(&["api", &label]).is_err()
                    && let Err(error) = transport.command.api::<Value>(
                        "POST",
                        &labels,
                        &json!({"name": name, "color": "ededed"}),
                    )
                {
                    // Another commenter can create the repository label at the same time.
                    transport
                        .command
                        .read::<Value>(&["api", &label])
                        .map_err(|_| error)?;
                }
                let endpoint = format!("repos/{}/issues/{number}/labels", self.repository());
                let _: Value =
                    transport
                        .command
                        .api("POST", &endpoint, &json!({"labels": [name]}))?;
                crate::issue_cache::invalidate(self.host(), self.repository());
                Ok(())
            }
        }
    }

    pub(crate) fn remove_issue_label(
        &self,
        number: u32,
        label: &str,
    ) -> Result<(), GitHubCommandError> {
        match &self.backend {
            IssueBackend::Collapse(collapse) => {
                collapse.update_issue(
                    number,
                    velocity_collapse::issues::UpdateIssueRequest {
                        remove_labels: vec![label.to_owned()],
                        ..Default::default()
                    },
                )?;
                Ok(())
            }
            IssueBackend::GitHub(transport) => {
                let endpoint =
                    format!("repos/{}/issues/{number}/labels/{label}", self.repository());
                transport
                    .command
                    .text(&["api", "--method", "DELETE", &endpoint])?;
                crate::issue_cache::invalidate(self.host(), self.repository());
                Ok(())
            }
        }
    }

    pub(crate) fn add_issue_comment(
        &self,
        number: u32,
        subject: &str,
        body: &str,
    ) -> Result<String, GitHubCommandError> {
        match &self.backend {
            IssueBackend::Collapse(collapse) => collapse.add_comment(number, body),
            IssueBackend::GitHub(transport) => transport.graphql_at(
                "mutation($input:AddCommentInput!) { addComment(input:$input) { commentEdge { node { url } } } }",
                json!({"input":{"subjectId":subject,"body":body}}),
                "/data/addComment/commentEdge/node/url",
            ),
        }
    }

    /// Applies one saved Rosen event effect. A lagging mirror leaves the close pending.
    pub fn follow_rosen_issue(
        &self,
        repository: &str,
        number: u32,
        commit: &str,
        sequence: u64,
        effect: RosenIssueEffect<'_>,
    ) -> Result<bool, GitHubCommandError> {
        Ok(matches!(
            self.follow_rosen_issue_checked(
                repository,
                number,
                commit,
                sequence,
                effect,
                velocity_core::collapse::ExpectedIssueIdentity::default(),
            )?,
            RosenIssueFollowOutcome::Applied
        ))
    }

    /// Applies one saved Rosen event effect after verifying the host issue's title or
    /// GitHub source matches `expected` (#614). Returns `Mismatch` instead of commenting or
    /// closing when a number collision points to a different issue.
    pub fn follow_rosen_issue_checked(
        &self,
        repository: &str,
        number: u32,
        commit: &str,
        sequence: u64,
        effect: RosenIssueEffect<'_>,
        expected: velocity_core::collapse::ExpectedIssueIdentity<'_>,
    ) -> Result<RosenIssueFollowOutcome, GitHubCommandError> {
        if !self.repository().eq_ignore_ascii_case(repository) {
            return Err(GitHubCommandError::Invalid(
                "The thread's issue repository differs from the project's mirror".into(),
            ));
        }
        let state = match effect {
            RosenIssueEffect::Landed { .. } => None,
            RosenIssueEffect::Reviewed { .. } | RosenIssueEffect::Watcher => {
                Some(IssueState::Closed)
            }
            RosenIssueEffect::Reverted { .. } => Some(IssueState::Open),
        };
        if state == Some(IssueState::Closed) && !self.rosen_commit_mirrored(commit)? {
            return Ok(RosenIssueFollowOutcome::WaitingForMirror);
        }
        let (kind, message) = rosen_issue_effect_message(commit, effect);
        // Deduplicate across machines by `(kind, commit)`, keeping the machine-local
        // Rosen sequence in its own diagnostic comment above the marker (#550).
        let body = format!("{message}\n\n<!-- velocity-rosen-sequence:{sequence} -->");
        let marker = format!("<!-- velocity-rosen-issue:{kind}:{commit} -->");
        let landed_marker = format!("<!-- velocity-rosen-issue:landed:{commit} -->");
        let issue = self.read_issue(number)?;
        let comments = self.issue_comment_bodies(number)?;
        let already_linked = comments.iter().any(|existing| {
            comment_matches_marker(existing, &marker)
                || comment_matches_marker(existing, &landed_marker)
        });
        if !already_linked
            && expected.has_expected_title()
            && !velocity_core::collapse::host_issue_matches_expected(
                &expected,
                &issue.title,
                &issue.body,
            )
        {
            return Ok(RosenIssueFollowOutcome::Mismatch {
                host_title: issue.title,
                host_url: issue.url,
            });
        }
        self.sync_loaded_rosen_issue(&issue, &comments, &marker, &body, state)?;
        Ok(RosenIssueFollowOutcome::Applied)
    }

    /// Publishes a pass on the exact mirrored base tested by Velocity.
    /// On Collapse, Rosen's own post-land checkpoints replace GitHub commit statuses (#591).
    pub fn publish_integration_success(&self, commit: &str) -> Result<bool, GitHubCommandError> {
        validate_commit_sha(commit)?;
        if matches!(self.backend, IssueBackend::Collapse(_)) {
            return Ok(true);
        }
        if crate::commit_cache::is_known_green(self.repository(), commit) {
            return Ok(true);
        }
        if !self.rosen_commit_mirrored(commit)? {
            return Ok(false);
        }
        if let Some(passed_at) = self.integration_status_at(commit)? {
            crate::commit_cache::record_green(self.repository(), commit, passed_at);
            return Ok(true);
        }
        let transport = self.github_transport()?;
        let endpoint = format!("repos/{}/statuses/{commit}", self.repository());
        let _: Value = transport.command.api(
            "POST",
            &endpoint,
            &json!({
                "context": INTEGRATION_CONTEXT,
                "state": "success",
                "description": "Velocity integration build passed"
            }),
        )?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|elapsed| elapsed.as_secs());
        crate::commit_cache::record_green(self.repository(), commit, now);
        Ok(true)
    }

    /// Returns false while the mirror's default branch has not caught up to the land.
    pub fn rosen_commit_mirrored(&self, commit: &str) -> Result<bool, GitHubCommandError> {
        validate_commit_sha(commit)?;
        if let IssueBackend::Collapse(collapse) = &self.backend {
            return collapse.commit_mirrored(commit);
        }
        if crate::commit_cache::is_mirrored(self.repository(), commit) {
            return Ok(true);
        }
        let snapshot = self.branch_tip_snapshot()?;
        if snapshot.position(commit).is_some() {
            crate::commit_cache::record_mirrored(self.repository(), commit);
            return Ok(true);
        }
        self.is_ancestor_of(commit, &snapshot.tip)
    }

    /// Whether any machine has already posted a green `velocity/integration` commit status
    /// on `commit` or a newer commit on the mirror's default branch (#519, #556).
    ///
    /// On Collapse, reads post-land verification state from Collapse instead of GitHub (#589).
    pub fn integration_status_covering(
        &self,
        commit: &str,
    ) -> Result<Option<IntegrationCoverage>, GitHubCommandError> {
        validate_commit_sha(commit)?;
        if let IssueBackend::Collapse(collapse) = &self.backend {
            return collapse.integration_status_covering(commit);
        }
        if let Some(coverage) = crate::commit_cache::covered(self.repository(), commit) {
            return Ok(Some(coverage));
        }
        let snapshot = self.branch_tip_snapshot()?;
        if let Some(index) = snapshot.position(commit) {
            let coverage = snapshot.green_covering_index(index);
            if let Some(coverage) = &coverage {
                crate::commit_cache::record_covered(self.repository(), commit, coverage.clone());
            }
            return Ok(coverage);
        }
        if let Some(green) = snapshot.newest_green()
            && self.is_ancestor_of(commit, &green.commit)?
        {
            crate::commit_cache::record_covered(self.repository(), commit, green.clone());
            return Ok(Some(green));
        }
        if crate::commit_cache::is_settled_non_green(self.repository(), commit) {
            return Ok(None);
        }
        if !self.is_ancestor_of(commit, &snapshot.tip)? {
            return Ok(None);
        }
        if let Some(passed_at) = self.integration_status_at(commit)? {
            crate::commit_cache::record_green(self.repository(), commit, passed_at);
            return Ok(Some(IntegrationCoverage {
                commit: commit.to_owned(),
                passed_at,
            }));
        }
        // `commit` is already behind the newest commits on the default branch and has no
        // green status of its own. Future green builds will cover it through `snapshot`.
        crate::commit_cache::record_settled_non_green(self.repository(), commit);
        Ok(None)
    }

    /// Returns a recent snapshot of the default branch tip and the integration statuses of
    /// its newest commits, reusing the snapshot populated by the shared open-issues read.
    pub(crate) fn branch_tip_snapshot(
        &self,
    ) -> Result<crate::commit_cache::BranchTipSnapshot, GitHubCommandError> {
        let ttl = crate::commit_cache::BRANCH_SNAPSHOT_TTL;
        if let Some(snapshot) = crate::commit_cache::fresh_branch_snapshot(self.repository(), ttl) {
            return Ok(snapshot);
        }
        let lock = crate::commit_cache::branch_fetch_lock(self.repository());
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(snapshot) = crate::commit_cache::fresh_branch_snapshot(self.repository(), ttl) {
            return Ok(snapshot);
        }
        let transport = self.github_transport()?;
        let branch = transport
            .default_branch
            .as_deref()
            .ok_or_else(|| GitHubCommandError::Missing("default branch".into()))?;
        let mut variables = transport.variables(0);
        variables["branch"] = format!("refs/heads/{branch}").into();
        let query = format!(
            "query($owner:String!,$name:String!,$branch:String!) {{
              repository(owner:$owner,name:$name) {{ ref(qualifiedName:$branch) {{
                name {} }} }} }}",
            crate::issues::branch_history_selection(),
        );
        match transport.graphql_at::<crate::issues::DefaultBranchRefNode>(
            &query,
            variables.clone(),
            "/data/repository/ref",
        ) {
            Ok(branch_ref) => {
                crate::issues::record_branch_ref(self.repository(), branch_ref);
                if let Some(snapshot) =
                    crate::commit_cache::fresh_branch_snapshot(self.repository(), ttl)
                {
                    return Ok(snapshot);
                }
            }
            Err(GitHubCommandError::Invalid(message))
                if message.contains("GitHub GraphQL errors") => {}
            Err(error) => return Err(error),
        }
        // Fallback for servers/mocks without commit history status selection.
        let tip = self.default_branch_tip()?;
        Ok(crate::commit_cache::record_branch_snapshot(
            self.repository(),
            tip,
            Vec::new(),
        ))
    }

    fn default_branch_tip(&self) -> Result<String, GitHubCommandError> {
        let transport = self.github_transport()?;
        let branch = transport
            .default_branch
            .as_deref()
            .ok_or_else(|| GitHubCommandError::Missing("default branch".into()))?;
        let mut variables = transport.variables(0);
        variables["branch"] = format!("refs/heads/{branch}").into();
        // Resolve the branch via GraphQL so names with slashes need no URL escaping.
        transport.graphql_at(
            "query($owner:String!,$name:String!,$branch:String!) {
              repository(owner:$owner,name:$name) { ref(qualifiedName:$branch) { target { oid } } } }",
            variables,
            "/data/repository/ref/target/oid",
        )
    }

    /// Cached check of whether `commit` is an ancestor of (or identical to) `target`.
    fn is_ancestor_of(&self, commit: &str, target: &str) -> Result<bool, GitHubCommandError> {
        if commit.eq_ignore_ascii_case(target) {
            crate::commit_cache::record_mirrored(self.repository(), commit);
            return Ok(true);
        }
        if let Some(known) = crate::commit_cache::cached_ancestry(self.repository(), commit, target)
        {
            return Ok(known);
        }
        let is_ancestor = self.compare_to_tip(commit, target)?.is_some();
        crate::commit_cache::record_ancestry(self.repository(), commit, target, is_ancestor);
        Ok(is_ancestor)
    }

    /// Returns `Some(descendant_shas)` when `commit` is an ancestor of (or identical to)
    /// `tip` on GitHub, and `None` when `commit` is missing or not in `tip`'s history.
    fn compare_to_tip(
        &self,
        commit: &str,
        tip: &str,
    ) -> Result<Option<Vec<String>>, GitHubCommandError> {
        let endpoint = format!("repos/{}/compare/{commit}...{tip}", self.repository());
        #[derive(Deserialize)]
        struct ComparedCommit {
            sha: String,
        }
        #[derive(Deserialize)]
        struct Comparison {
            status: String,
            #[serde(default)]
            commits: Vec<ComparedCommit>,
        }
        let transport = self.github_transport()?;
        let comparison: Comparison = match transport.command.read(&["api", &endpoint]) {
            Ok(comparison) => comparison,
            Err(GitHubCommandError::Command { message, .. })
                if message.contains("HTTP 404") || message.contains("Not Found") =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        if !matches!(comparison.status.as_str(), "ahead" | "identical") {
            return Ok(None);
        }
        Ok(Some(
            comparison
                .commits
                .into_iter()
                .map(|entry| entry.sha)
                .collect(),
        ))
    }

    /// `Some(passed_at)` when `commit` carries a green `velocity/integration` status.
    fn integration_status_at(
        &self,
        commit: &str,
    ) -> Result<Option<Option<u64>>, GitHubCommandError> {
        #[derive(Deserialize)]
        struct Status {
            context: String,
            state: String,
            #[serde(default)]
            updated_at: Option<String>,
            #[serde(default)]
            created_at: Option<String>,
        }
        let endpoint = format!(
            "repos/{}/commits/{commit}/statuses?per_page=100",
            self.repository()
        );
        let transport = self.github_transport()?;
        let statuses: Vec<Status> = transport.command.read(&["api", &endpoint])?;
        Ok(statuses
            .into_iter()
            .find(|status| status.context == INTEGRATION_CONTEXT)
            .filter(|status| status.state == "success")
            .map(|status| {
                status
                    .updated_at
                    .or(status.created_at)
                    .as_deref()
                    .and_then(unix_seconds)
            }))
    }

    /// The marker makes a retry after an ambiguous mutation response reuse its comment.
    /// Comment first, state second: a failure can never lose the evidence on a closed issue.
    fn sync_rosen_issue(
        &self,
        number: u32,
        marker: &str,
        body: &str,
        state: Option<IssueState>,
    ) -> Result<(), GitHubCommandError> {
        let issue = self.read_issue(number)?;
        let comments = self.issue_comment_bodies(number)?;
        self.sync_loaded_rosen_issue(&issue, &comments, marker, body, state)
    }

    fn sync_loaded_rosen_issue(
        &self,
        issue: &GitHubIssue,
        comments: &[String],
        marker: &str,
        body: &str,
        state: Option<IssueState>,
    ) -> Result<(), GitHubCommandError> {
        let number = issue.number;
        if already_closed_for_commit(issue, state, marker, comments) {
            return Ok(());
        }
        if !comments
            .iter()
            .any(|existing| comment_matches_marker(existing, marker))
        {
            self.add_issue_comment(number, &issue.id, &format_marked_comment(body, marker))?;
            crate::issue_cache::invalidate_discussion(self.host(), self.repository(), number);
        }
        if state.is_some()
            && issue
                .labels
                .iter()
                .any(|label| label.eq_ignore_ascii_case(READY_LABEL))
        {
            self.remove_issue_label(number, READY_LABEL)?;
        }
        let Some(state) = state.filter(|state| *state != issue.state) else {
            return Ok(());
        };
        let reopen = state == IssueState::Open;
        match &self.backend {
            IssueBackend::Collapse(collapse) => {
                if reopen {
                    collapse.update_issue(
                        number,
                        velocity_collapse::issues::UpdateIssueRequest {
                            state: Some("open".to_owned()),
                            ..Default::default()
                        },
                    )?;
                } else {
                    collapse.close_issue(number, "completed", "")?;
                }
            }
            IssueBackend::GitHub(transport) => {
                let mutation = if reopen {
                    "mutation($input:ReopenIssueInput!) { reopenIssue(input:$input) { issue { id } } }"
                } else {
                    "mutation($input:CloseIssueInput!) { closeIssue(input:$input) { issue { id } } }"
                };
                let path = if reopen {
                    "/data/reopenIssue"
                } else {
                    "/data/closeIssue"
                };
                let _: Value =
                    transport.graphql_at(mutation, json!({"input":{"issueId":issue.id}}), path)?;
            }
        }
        crate::issue_cache::invalidate(self.host(), self.repository());
        crate::issue_cache::invalidate_discussion(self.host(), self.repository(), number);
        Ok(())
    }

    /// The landed commit when Velocity's comments show a land that no reopen, close or
    /// "returned to dispatch" review followed (#504). Such an issue waits for its
    /// integration build; the GitHub record survives local thread and project cleanup.
    pub fn landed_awaiting_integration(
        &self,
        number: u32,
    ) -> Result<Option<String>, GitHubCommandError> {
        Ok(awaiting_integration(&self.issue_comment_bodies(number)?))
    }

    /// Whether the issue's GitHub comments show a Velocity land or a recent `issue-N`
    /// starting/progress comment with no later ended or landed comment (#504, #519).
    pub fn external_issue_state(
        &self,
        number: u32,
        now: u64,
    ) -> Result<Option<ExternalIssueState>, GitHubCommandError> {
        let comments = self.issue_comments(number)?;
        Ok(classify_external_issue_state(
            number,
            &comments,
            now,
            REMOTE_PROGRESS_WINDOW_SECONDS,
        ))
    }

    /// Whether a review already checked this issue against an overlapping land and
    /// returned it to dispatch with work remaining (#525). The GitHub record survives
    /// local state, so such an issue is never held on overlap a second time.
    pub fn overlap_review_returned(&self, number: u32) -> Result<bool, GitHubCommandError> {
        Ok(self.issue_comment_bodies(number)?.iter().any(|body| {
            body.lines()
                .last()
                .and_then(|line| line.trim().strip_prefix("<!-- velocity-no-change-review:"))
                .and_then(|marker| marker.strip_suffix(" -->"))
                .is_some_and(|marker| marker.starts_with("overlap:") && marker.ends_with(":false"))
        }))
    }

    pub(crate) fn has_rosen_comment(
        &self,
        number: u32,
        marker: &str,
    ) -> Result<bool, GitHubCommandError> {
        Ok(self
            .issue_comment_bodies(number)?
            .iter()
            .any(|body| comment_matches_marker(body, marker)))
    }

    pub(crate) fn issue_comment_bodies(
        &self,
        number: u32,
    ) -> Result<Vec<String>, GitHubCommandError> {
        Ok(self
            .issue_comments(number)?
            .into_iter()
            .map(|comment| comment.body)
            .collect())
    }

    /// Oldest first.
    fn issue_comments(&self, number: u32) -> Result<Vec<IssueComment>, GitHubCommandError> {
        if let IssueBackend::Collapse(collapse) = &self.backend {
            return Ok(collapse
                .issue_comments(number)?
                .into_iter()
                .map(|comment| IssueComment {
                    body: comment.body,
                    created_at: comment.created_at,
                })
                .collect());
        }
        let transport = self.github_transport()?;
        #[derive(Deserialize)]
        struct Comment {
            body: String,
            #[serde(rename = "createdAt")]
            created_at: String,
        }
        let comments: Vec<Comment> = transport.connection(
            "query($owner:String!,$name:String!,$number:Int!,$cursor:String) {
              repository(owner:$owner,name:$name) { issue(number:$number) {
                comments(first:100,after:$cursor) { nodes { body createdAt }
                  pageInfo { hasNextPage endCursor } } } } }",
            transport.variables(number),
            "/data/repository/issue/comments",
        )?;
        Ok(comments
            .into_iter()
            .map(|comment| IssueComment {
                body: comment.body,
                created_at: comment.created_at,
            })
            .collect())
    }
}

/// What an overlap review that needs no reviewer says (#506): the land's files are the
/// evidence that it changed none of the issue's code.
pub fn overlap_clear_evidence(source: u32, commit: &str, files: &[String]) -> String {
    let mut evidence = format!(
        "This issue waited for #{source}. Rosen commit `{commit}` landed for that issue and \
         changed none of the code paths this issue is predicted to touch (documents \
         excluded), so it needs no review against that land.\n\nFiles in the land:"
    );
    if files.is_empty() {
        evidence.push_str("\n- none");
    }
    for file in files.iter().take(LISTED_LAND_FILES) {
        evidence.push_str(&format!("\n- `{file}`"));
    }
    if files.len() > LISTED_LAND_FILES {
        evidence.push_str(&format!("\n- and {} more", files.len() - LISTED_LAND_FILES));
    }
    evidence
}

/// The attempt a no-change or overlap report names in its closing marker.
fn report_attempt(body: &str) -> Option<&str> {
    body.lines()
        .last()?
        .strip_prefix(NO_CHANGE_MARKER)?
        .strip_suffix(" -->")
}

/// GitHub's `2026-11-19T16:31:00Z` as Unix seconds.
pub(crate) fn unix_seconds(timestamp: &str) -> Option<u64> {
    let (date, time) = timestamp.strip_suffix('Z')?.split_once('T')?;
    let mut date = date.split('-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let mut time = time.split(':').map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (time.next()??, time.next()??, time.next()??);
    // Days since 1970-01-01 in the proleptic Gregorian calendar, counting years from
    // March so the leap day falls last.
    let year = if month <= 2 { year - 1 } else { year };
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let year_of_era = year.rem_euclid(400);
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = year.div_euclid(400) * 146_097 + day_of_era - 719_468;
    u64::try_from(days * 86_400 + hour * 3_600 + minute * 60 + second).ok()
}

/// Comments arrive oldest first, so the last Velocity marker decides.
fn awaiting_integration(comments: &[String]) -> Option<String> {
    let mut landed = None;
    for marker in comments.iter().filter_map(|body| comment_marker(body)) {
        if let Some((kind, commit)) = parse_rosen_issue_marker_payload(marker) {
            landed = (kind == "landed").then(|| commit.to_owned());
        } else if marker.starts_with("velocity-no-change-review:") && !marker.ends_with(":true") {
            // A reviewer returned the issue to dispatch with remaining work, or found
            // that it needs the owner.
            landed = None;
        }
    }
    landed
}

fn validate_commit_sha(commit: &str) -> Result<(), GitHubCommandError> {
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(GitHubCommandError::Invalid(
            "Invalid landed commit SHA".into(),
        ));
    }
    Ok(())
}

fn rosen_issue_effect_message(
    commit: &str,
    effect: RosenIssueEffect<'_>,
) -> (&'static str, String) {
    match effect {
        RosenIssueEffect::Landed { change } => (
            "landed",
            format!("Velocity · Rosen change `{change}` landed as commit `{commit}`."),
        ),
        RosenIssueEffect::Reverted { reason } => {
            let end = reason.floor_char_boundary(32_000);
            let output = &reason[..end];
            let truncated = if end < reason.len() {
                "\n\n[Output truncated.]"
            } else {
                ""
            };
            (
                "reopen",
                format!(
                    "Rosen reverted landed commit `{commit}`. Reopening this issue.\n\n{output}{truncated}"
                ),
            )
        }
        RosenIssueEffect::Reviewed { by } => (
            "close",
            format!(
                "Velocity review swarm · `{by}` reviewed Rosen commit `{commit}` and confirmed it addresses this issue."
            ),
        ),
        RosenIssueEffect::Watcher => (
            "close",
            format!(
                "Velocity post-land verification · Rosen commit `{commit}` passed its post-land build. \
                 The 30-minute review window elapsed without review coverage."
            ),
        ),
    }
}

fn format_marked_comment(body: &str, marker: &str) -> String {
    if body.lines().last().is_some_and(|line| {
        let trimmed = line.trim();
        trimmed.starts_with("<!-- ") && trimmed.ends_with(" -->")
    }) {
        format!("{body}\n{marker}")
    } else {
        format!("{body}\n\n{marker}")
    }
}

fn comment_marker(body: &str) -> Option<&str> {
    body.lines()
        .rev()
        .find(|line| !line.trim().is_empty())?
        .trim()
        .strip_prefix("<!-- ")?
        .strip_suffix(" -->")
}

fn parse_rosen_issue_marker(line: &str) -> Option<(&str, &str)> {
    let payload = line.trim().strip_prefix("<!-- ")?.strip_suffix(" -->")?;
    parse_rosen_issue_marker_payload(payload)
}

fn parse_rosen_issue_marker_payload(payload: &str) -> Option<(&str, &str)> {
    let rest = payload.strip_prefix("velocity-rosen-issue:")?;
    let mut fields = rest.split(':');
    let kind = fields.next()?;
    let commit = fields.next()?;
    if kind.is_empty() || commit.is_empty() {
        return None;
    }
    // Accept both deduplicated (`kind:commit`) and legacy (`kind:commit:sequence`) markers (#550).
    match (fields.next(), fields.next()) {
        (None, _) => Some((kind, commit)),
        (Some(sequence), None)
            if !sequence.is_empty() && sequence.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            Some((kind, commit))
        }
        _ => None,
    }
}

fn comment_matches_marker(body: &str, marker: &str) -> bool {
    let Some(last) = body.lines().rev().find(|line| !line.trim().is_empty()) else {
        return false;
    };
    let last = last.trim();
    if last == marker {
        return true;
    }
    if let (Some((expected_kind, expected_commit)), Some((actual_kind, actual_commit))) = (
        parse_rosen_issue_marker(marker),
        parse_rosen_issue_marker(last),
    ) {
        return expected_kind == actual_kind && expected_commit.eq_ignore_ascii_case(actual_commit);
    }
    false
}

/// Skips a close when the issue is already closed and an existing close comment covers
/// the same commit (#550).
fn already_closed_for_commit(
    issue: &GitHubIssue,
    state: Option<IssueState>,
    marker: &str,
    comments: &[String],
) -> bool {
    if issue.state != IssueState::Closed || state != Some(IssueState::Closed) {
        return false;
    }
    let Some(("close", commit)) = parse_rosen_issue_marker(marker) else {
        return false;
    };
    comments
        .iter()
        .any(|body| close_comment_covers_commit(body, commit))
}

fn close_comment_covers_commit(body: &str, commit: &str) -> bool {
    if let Some(marker) = comment_marker(body) {
        if let Some(("close", existing)) = parse_rosen_issue_marker_payload(marker) {
            return commit_matches(existing, commit);
        }
        if let Some(existing) = marker.strip_prefix("velocity-spec-orphaned:") {
            return commit_matches(existing, commit);
        }
        if marker.starts_with("velocity-no-change-review:") && marker.ends_with(":true") {
            return body_mentions_commit(body, commit);
        }
        return false;
    }
    let trimmed = body.trim_start();
    let is_velocity_close = trimmed.starts_with("Velocity post-land verification · ")
        || (trimmed.starts_with("Velocity review swarm · ")
            && (trimmed.contains("confirmed it addresses this issue")
                || trimmed.contains("verified already fixed")));
    is_velocity_close && body_mentions_commit(body, commit)
}

fn commit_matches(candidate: &str, commit: &str) -> bool {
    if candidate.eq_ignore_ascii_case(commit) {
        return true;
    }
    let (short, long) = if candidate.len() <= commit.len() {
        (candidate, commit)
    } else {
        (commit, candidate)
    };
    short.len() >= 7
        && short.bytes().all(|byte| byte.is_ascii_hexdigit())
        && long.bytes().all(|byte| byte.is_ascii_hexdigit())
        && long
            .get(..short.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(short))
}

fn body_mentions_commit(body: &str, commit: &str) -> bool {
    body.split(|character: char| !character.is_ascii_hexdigit())
        .any(|token| commit_matches(token, commit))
}

/// Comments arrive oldest first. A land marker takes precedence over any later duplicate
/// starting comment until a reopen, close or non-fixed review clears the land (#519).
fn classify_external_issue_state(
    number: u32,
    comments: &[IssueComment],
    now: u64,
    window_seconds: u64,
) -> Option<ExternalIssueState> {
    let mut landed: Option<(String, Option<u64>)> = None;
    let mut in_progress: Option<u64> = None;
    for comment in comments {
        let timestamp = unix_seconds(&comment.created_at);
        if let Some(marker) = comment_marker(&comment.body) {
            if let Some((kind, commit)) = parse_rosen_issue_marker_payload(marker) {
                landed = (kind == "landed").then(|| (commit.to_owned(), timestamp));
                in_progress = None;
                continue;
            }
            if marker.starts_with("velocity-no-change-review:") {
                if !marker.ends_with(":true") {
                    landed = None;
                }
                in_progress = None;
                continue;
            }
            if marker.starts_with("velocity-issue-no-change:")
                || marker.starts_with("velocity-launch-failed:")
                || marker.starts_with("velocity-overlap-clear:")
            {
                in_progress = None;
                continue;
            }
        }
        let trimmed = comment.body.trim_start();
        if trimmed.starts_with("Needs the owner · ")
            || trimmed.starts_with("On hold · ")
            || trimmed.starts_with("Blocked upstream · ")
            || trimmed.starts_with("Closed as not planned · ")
        {
            in_progress = None;
            continue;
        }
        if let Some((ready, message)) = issue_agent_comment(&comment.body, number) {
            if ends_with_dependency_wait(message) {
                in_progress = None;
            } else if ready || in_progress.is_some() || is_starting_comment(message, number) {
                in_progress = timestamp.or(in_progress);
            }
        }
    }
    if let Some((commit, landed_at)) = landed {
        return Some(ExternalIssueState::Landed { commit, landed_at });
    }
    let started_at = in_progress?;
    (now.saturating_sub(started_at) < window_seconds)
        .then_some(ExternalIssueState::InProgressElsewhere { started_at })
}

fn issue_agent_comment(body: &str, number: u32) -> Option<(bool, &str)> {
    let (byline, message) = body.split_once("\n\n")?;
    let byline = byline.trim();
    let (ready, rest) = match byline.strip_prefix("Ready to close · ") {
        Some(rest) => (true, rest),
        None => (false, byline),
    };
    let nick = rest.strip_prefix("Velocity agent `")?.split_once('`')?.0;
    let prefix = format!("issue-{number}");
    let matches_issue = nick == prefix
        || nick
            .strip_prefix(&prefix)
            .is_some_and(|tail| tail.starts_with('-') && tail.len() > 1);
    matches_issue.then_some((ready, message.trim()))
}

fn is_starting_comment(message: &str, number: u32) -> bool {
    let lower = message.trim_start().to_ascii_lowercase();
    let starts_with_starting = lower.strip_prefix("starting").is_some_and(|rest| {
        rest.is_empty()
            || rest
                .chars()
                .next()
                .is_some_and(|character| !character.is_ascii_alphanumeric())
    });
    starts_with_starting || lower.contains(&format!("starting #{number}"))
}

fn ends_with_dependency_wait(message: &str) -> bool {
    message.lines().last().is_some_and(|line| {
        let line = line.trim();
        line.starts_with("Waits for ")
            && (line.ends_with(", recorded as GitHub issue dependencies.")
                || line.ends_with(", recorded as issue dependencies."))
    })
}

fn no_change_hold_note(
    is_no_change: bool,
    labelled_no_change: bool,
    has_hold_state: bool,
    has_blockers: bool,
) -> &'static str {
    if !is_no_change {
        return "";
    }
    if labelled_no_change {
        "\n\nHold: this issue is held under `no-change` and paused from continuous dispatch. \
         A bounded review, removing the `no-change` label, or the 2-hour review timeout \
         returns it to dispatch."
    } else if has_blockers {
        "\n\nHold: this issue remains held by its open GitHub blockers (no `no-change` label \
         was added). Continuous dispatch starts it once those blockers close or pass integration."
    } else if has_hold_state {
        "\n\nHold: this issue keeps its existing hold label (no `no-change` label was added). \
         Resolving or removing that hold returns it to dispatch."
    } else {
        ""
    }
}

fn issue_has_no_change_label(issue: &GitHubIssue) -> bool {
    issue_has_label(issue, IssueClient::NO_CHANGE_LABEL)
}

fn issue_has_hold_state(issue: &GitHubIssue) -> bool {
    issue.labels.iter().any(|label| {
        is_person_hold_label(label) || is_tracking_label(label) || is_blocked_upstream_label(label)
    })
}

fn issue_has_label(issue: &GitHubIssue, name: &str) -> bool {
    issue
        .labels
        .iter()
        .any(|label| label.eq_ignore_ascii_case(name))
}
