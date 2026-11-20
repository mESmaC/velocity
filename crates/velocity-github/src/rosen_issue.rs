//! GitHub comments and the durable effects of landed Rosen issue work.
use serde::Deserialize;
use serde_json::{Value, json};
use velocity_core::issue_dependencies::IssueReference;

use crate::issue_dependencies::{NEEDS_OWNER_LABEL, refuse_dependency_hold, waits_line};
use crate::{GitHubCommandError, GitHubIssue, IssueClient, IssueState};

const READY_LABEL: &str = "ready-to-close";
const NO_CHANGE_MARKER: &str = "<!-- velocity-issue-no-change:";
/// The attempt of an overlap review: `overlap:<blocking issue>:<landed commit>`.
const OVERLAP_ATTEMPT: &str = "overlap:";
/// A land can change hundreds of files; the comment names the first of them.
const LISTED_LAND_FILES: usize = 40;

pub enum RosenIssueEffect<'a> {
    Landed { change: &'a str },
    Reviewed { by: &'a str },
    Watcher,
    Reverted { reason: &'a str },
}

/// How an issue parked under `no-change` leaves review (#506).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParkedOutcome {
    /// The current base already meets the acceptance criteria, so the issue closes.
    AlreadyFixed,
    /// Work remains: the issue goes back to dispatch with instructions or a note.
    Returned,
    /// The work needs hardware, an upstream release or an owner decision. The issue takes
    /// `needs-owner` with the reason and loses `no-change`.
    NeedsOwner,
}

impl ParkedOutcome {
    /// The last field of the review marker. `true` and `false` predate `needs-owner`.
    fn marker(self) -> &'static str {
        match self {
            Self::AlreadyFixed => "true",
            Self::Returned => "false",
            Self::NeedsOwner => "needs-owner",
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

    /// `blockers` are the issues this one waits on (#511). They are linked as GitHub
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
        let waits = waits_line(&self.link_blockers(number, blockers)?);
        if ready {
            self.label_issue(number, READY_LABEL)?;
        }
        let heading = if ready { "Ready to close · " } else { "" };
        self.add_issue_comment(&issue.id, &format!("{heading}{byline}\n\n{body}{waits}"))
    }

    /// An agent found that the work needs hardware, an upstream release or an owner
    /// decision (#506). The label goes on first: a failed comment leaves the issue out of
    /// dispatch, and the agent's retry posts the reason. A reason that only waits on other
    /// issues is refused: those are linked as `blockers`, never held by the label (#511).
    pub fn post_needs_owner(
        &self,
        number: u32,
        byline: &str,
        reason: &str,
        blockers: &[IssueReference],
    ) -> Result<String, GitHubCommandError> {
        refuse_dependency_hold(reason)?;
        let issue = self.read_issue(number)?;
        let waits = waits_line(&self.link_blockers(number, blockers)?);
        self.label_issue(number, NEEDS_OWNER_LABEL)?;
        if issue_has_no_change_label(&issue) {
            self.remove_issue_label(number, Self::NO_CHANGE_LABEL)?;
        }
        self.add_issue_comment(
            &issue.id,
            &format!("Needs the owner · {byline}\n\n{reason}{waits}"),
        )
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
        self.post_issue_for_verification(
            number,
            change,
            ("Ended with no change", byline, reason),
            false,
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
            true,
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

    /// `park_blocked` is whether an issue with open blockers still takes `no-change`.
    fn post_issue_for_verification(
        &self,
        number: u32,
        attempt: &str,
        (heading, byline, reason): (&str, &str, &str),
        park_blocked: bool,
    ) -> Result<(), GitHubCommandError> {
        let marker = format!("{NO_CHANGE_MARKER}{attempt} -->");
        if self.has_rosen_comment(number, &marker)? {
            // A reviewer may have removed the label before the local acknowledgement.
            return Ok(());
        }
        let issue = self.read_issue(number)?;
        // An issue that already waits for the owner keeps that label alone (#506).
        if issue.state == IssueState::Open
            && !issue_has_no_change_label(&issue)
            && !issue_has_label(&issue, NEEDS_OWNER_LABEL)
            && (park_blocked || !issue.has_open_blockers())
        {
            self.label_issue(number, Self::NO_CHANGE_LABEL)?;
        }
        let end = reason.floor_char_boundary(60_000);
        let truncated = if end < reason.len() {
            "\n\n[Message truncated.]"
        } else {
            ""
        };
        self.add_issue_comment(
            &issue.id,
            &format!(
                "## {heading}\n\n{byline}\n\n{}{truncated}\n\n{marker}",
                &reason[..end]
            ),
        )?;
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
    /// instead of taking `needs-owner`.
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
                "Review the current report and supply verification, retry instructions or the reason the owner is needed".into(),
            ));
        }
        let (decision, state) = match outcome {
            ParkedOutcome::AlreadyFixed if !blockers.is_empty() => {
                return Err(GitHubCommandError::Invalid(
                    "An already fixed issue waits on no other issue".into(),
                ));
            }
            ParkedOutcome::AlreadyFixed => ("verified already fixed", Some(IssueState::Closed)),
            ParkedOutcome::Returned => ("returned to dispatch", None),
            ParkedOutcome::NeedsOwner => {
                refuse_dependency_hold(evidence)?;
                ("found that this needs the owner", None)
            }
        };
        let waits = waits_line(&self.link_blockers(number, blockers)?);
        let marker = format!(
            "<!-- velocity-no-change-review:{current}:{} -->",
            outcome.marker()
        );
        let body = format!("{reviewer} {decision}.\n\n{evidence}{waits}");
        self.sync_rosen_issue(number, &marker, &body, state)?;
        match outcome {
            ParkedOutcome::AlreadyFixed => {}
            ParkedOutcome::Returned => self.remove_issue_label(number, Self::NO_CHANGE_LABEL)?,
            ParkedOutcome::NeedsOwner => {
                // The owner label goes on before `no-change` comes off, so a failure
                // between the two never leaves the issue open to dispatch.
                self.label_issue(number, NEEDS_OWNER_LABEL)?;
                self.remove_issue_label(number, Self::NO_CHANGE_LABEL)?;
            }
        }
        Ok(())
    }

    pub(crate) fn label_issue(&self, number: u32, name: &str) -> Result<(), GitHubCommandError> {
        let labels = format!("repos/{}/labels", self.repository());
        let label = format!("{labels}/{name}");
        if self
            .transport
            .command
            .read::<Value>(&["api", &label])
            .is_err()
            && let Err(error) = self.transport.command.api::<Value>(
                "POST",
                &labels,
                &json!({"name": name, "color": "ededed"}),
            )
        {
            // Another commenter can create the repository label at the same time.
            self.transport
                .command
                .read::<Value>(&["api", &label])
                .map_err(|_| error)?;
        }
        let endpoint = format!("repos/{}/issues/{number}/labels", self.repository());
        let _: Value = self
            .transport
            .command
            .api("POST", &endpoint, &json!({"labels": [name]}))?;
        Ok(())
    }

    pub(crate) fn remove_issue_label(
        &self,
        number: u32,
        label: &str,
    ) -> Result<(), GitHubCommandError> {
        let endpoint = format!("repos/{}/issues/{number}/labels/{label}", self.repository());
        self.transport
            .command
            .text(&["api", "--method", "DELETE", &endpoint])?;
        Ok(())
    }

    fn add_issue_comment(&self, subject: &str, body: &str) -> Result<String, GitHubCommandError> {
        self.transport.graphql_at(
            "mutation($input:AddCommentInput!) { addComment(input:$input) { commentEdge { node { url } } } }",
            json!({"input":{"subjectId":subject,"body":body}}),
            "/data/addComment/commentEdge/node/url",
        )
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
            return Ok(false);
        }
        let (kind, body) = match effect {
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
        };
        let marker = format!("<!-- velocity-rosen-issue:{kind}:{commit}:{sequence} -->");
        self.sync_rosen_issue(number, &marker, &body, state)?;
        Ok(true)
    }

    /// Publishes a pass on the exact mirrored base tested by Velocity.
    pub fn publish_integration_success(&self, commit: &str) -> Result<bool, GitHubCommandError> {
        if !self.rosen_commit_mirrored(commit)? {
            return Ok(false);
        }
        #[derive(Deserialize)]
        struct Status {
            context: String,
            state: String,
        }
        const CONTEXT: &str = "velocity/integration";
        let statuses = format!(
            "repos/{}/commits/{commit}/statuses?per_page=100",
            self.repository()
        );
        let existing: Vec<Status> = self.transport.command.read(&["api", &statuses])?;
        if existing
            .iter()
            .find(|status| status.context == CONTEXT)
            .is_some_and(|status| status.state == "success")
        {
            return Ok(true);
        }
        let endpoint = format!("repos/{}/statuses/{commit}", self.repository());
        let _: Value = self.transport.command.api(
            "POST",
            &endpoint,
            &json!({
                "context": CONTEXT,
                "state": "success",
                "description": "Velocity integration build passed"
            }),
        )?;
        Ok(true)
    }

    /// Returns false while the mirror's default branch has not caught up to the land.
    pub fn rosen_commit_mirrored(&self, commit: &str) -> Result<bool, GitHubCommandError> {
        if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(GitHubCommandError::Invalid(
                "Invalid landed commit SHA".into(),
            ));
        }
        let branch = self
            .transport
            .default_branch
            .as_deref()
            .ok_or_else(|| GitHubCommandError::Missing("default branch".into()))?;
        let mut variables = self.transport.variables(0);
        variables["branch"] = format!("refs/heads/{branch}").into();
        // Resolve the branch via GraphQL so names with slashes need no URL escaping.
        let tip: String = self.transport.graphql_at(
            "query($owner:String!,$name:String!,$branch:String!) {
              repository(owner:$owner,name:$name) { ref(qualifiedName:$branch) { target { oid } } } }",
            variables,
            "/data/repository/ref/target/oid",
        )?;
        let endpoint = format!("repos/{}/compare/{commit}...{tip}", self.repository());
        #[derive(Deserialize)]
        struct Comparison {
            status: String,
        }
        let comparison: Comparison = self.transport.command.read(&["api", &endpoint])?;
        Ok(matches!(comparison.status.as_str(), "ahead" | "identical"))
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
        if !self.has_rosen_comment(number, marker)? {
            self.add_issue_comment(&issue.id, &format!("{body}\n\n{marker}"))?;
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
            self.transport
                .graphql_at(mutation, json!({"input":{"issueId":issue.id}}), path)?;
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

    fn has_rosen_comment(&self, number: u32, marker: &str) -> Result<bool, GitHubCommandError> {
        Ok(self
            .issue_comment_bodies(number)?
            .iter()
            .any(|body| body.lines().last() == Some(marker)))
    }

    fn issue_comment_bodies(&self, number: u32) -> Result<Vec<String>, GitHubCommandError> {
        Ok(self
            .issue_comments(number)?
            .into_iter()
            .map(|comment| comment.body)
            .collect())
    }

    /// Oldest first.
    fn issue_comments(&self, number: u32) -> Result<Vec<IssueComment>, GitHubCommandError> {
        #[derive(Deserialize)]
        struct Comment {
            body: String,
            #[serde(rename = "createdAt")]
            created_at: String,
        }
        let comments: Vec<Comment> = self.transport.connection(
            "query($owner:String!,$name:String!,$number:Int!,$cursor:String) {
              repository(owner:$owner,name:$name) { issue(number:$number) {
                comments(first:100,after:$cursor) { nodes { body createdAt }
                  pageInfo { hasNextPage endCursor } } } } }",
            self.transport.variables(number),
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
fn unix_seconds(timestamp: &str) -> Option<u64> {
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
    for marker in comments
        .iter()
        .filter_map(|body| body.lines().last())
        .filter_map(|line| line.trim().strip_prefix("<!-- ")?.strip_suffix(" -->"))
    {
        if let Some(rest) = marker.strip_prefix("velocity-rosen-issue:") {
            let mut fields = rest.split(':');
            landed = match (fields.next(), fields.next()) {
                (Some("landed"), Some(commit)) => Some(commit.to_owned()),
                _ => None,
            };
        } else if marker.starts_with("velocity-no-change-review:") && !marker.ends_with(":true") {
            // A reviewer returned the issue to dispatch with remaining work, or found
            // that it needs the owner.
            landed = None;
        }
    }
    landed
}

fn issue_has_no_change_label(issue: &GitHubIssue) -> bool {
    issue_has_label(issue, IssueClient::NO_CHANGE_LABEL)
}

fn issue_has_label(issue: &GitHubIssue, name: &str) -> bool {
    issue
        .labels
        .iter()
        .any(|label| label.eq_ignore_ascii_case(name))
}
