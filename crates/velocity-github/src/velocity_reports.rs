//! Tool issues from agents' discoveries (F14 Discoveries): marked discoveries are filed on
//! Velocity's, Rosen's or Collapse's repository, keeping each destination's issues separate.
//! Problem reports that match an open issue are added as an **Also seen by** comment;
//! friction and tool_request reports are grouped per repository by #471 similarity and
//! filed as a `self-improvement` issue once three distinct sessions report them or total
//! cost reaches 60 agent-minutes (#620). Everything on GitHub goes through its REST API,
//! which keeps working when a swarm has spent the GraphQL budget.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use velocity_core::collapse::IssueHost;
use velocity_core::issue_dependencies::{IssueReference, declared_blockers};

use crate::collapse_issues::CollapseIssueTransport;
use crate::command::gh;
use crate::gh_command::GitHubCommandError;
use crate::{GitHubIssue, IssueState};

/// Where Velocity's, Rosen's and Collapse's own issues live.
pub const VELOCITY_REPOSITORY: &str = "mESmaC/velocity";
pub const ROSEN_REPOSITORY: &str = "mESmaC/rosen";
pub const COLLAPSE_REPOSITORY: &str = "mESmaC/collapse.sh";

/// Label applied to grouped friction and tool_request issues (#620).
pub const SELF_IMPROVEMENT_LABEL: &str = "self-improvement";
const SELF_IMPROVEMENT_LABEL_COLOR: &str = "1d76db";
const SELF_IMPROVEMENT_LABEL_DESCRIPTION: &str =
    "Tooling friction and tool requests reported by agents";

/// Thresholds that turn a grouped friction/tool_request cluster into one issue (#620).
pub const GROUP_SESSION_THRESHOLD: usize = 3;
pub const GROUP_MINUTES_THRESHOLD: u32 = 60;

/// A discovery targets one of the suite's tools, independently of the project using it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ReportRepository {
    Velocity,
    Rosen,
    Collapse,
}

impl ReportRepository {
    pub fn repository(self) -> &'static str {
        match self {
            Self::Velocity => VELOCITY_REPOSITORY,
            Self::Rosen => ROSEN_REPOSITORY,
            Self::Collapse => COLLAPSE_REPOSITORY,
        }
    }

    /// Issue host to use when the destination repository is not registered as a Velocity
    /// project. Collapse (`mESmaC/collapse.sh`) uses GitHub today (#620).
    pub fn default_host(self, fallback: IssueHost) -> IssueHost {
        match self {
            Self::Collapse => IssueHost::GitHub,
            Self::Velocity | Self::Rosen => fallback,
        }
    }
}

/// The app data file that records what Velocity has reported.
pub const REPORTS_FILE: &str = "velocity-reports.json";

/// The most of a discovery's text a report quotes. `gh api` takes the body on its command
/// line, which Windows caps at 32,767 characters; a longer body would fail on every pass and
/// hold up the project's later reports.
const QUOTED_TEXT_LIMIT: usize = 20_000;

/// Titles whose significant words overlap at least this much (shared words over all words in
/// either title) describe the same problem.
const SAME_PROBLEM: f64 = 0.5;
const RECENT_SECONDS: u64 = 60 * 60;

const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "when", "are", "but", "not",
    "its", "has", "can", "after", "before", "every", "all", "any", "some", "velocity", "rosen",
    "collapse", "under", "own", "their", "same", "issue", "thread", "task", "tool",
];

/// One discovery to report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryReport {
    pub title: String,
    pub text: String,
    /// The agent that recorded it.
    pub nick: String,
    /// The project it was working in.
    pub project: String,
    /// When it was recorded (RFC 3339).
    pub time: String,
}

impl DiscoveryReport {
    pub fn new(title: &str, text: &str, nick: &str, project: &str, time: &str) -> Self {
        Self {
            title: title.into(),
            text: text.into(),
            nick: nick.into(),
            project: project.into(),
            time: time.into(),
        }
    }
}

/// What reporting did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportOutcome {
    /// A new issue was opened.
    Filed(u32),
    /// An open issue already described the problem; the report was added to it.
    Joined(u32),
}

impl ReportOutcome {
    pub fn issue(self) -> u32 {
        match self {
            Self::Filed(number) | Self::Joined(number) => number,
        }
    }
}

/// Kind of grouped self-improvement feedback discovery (#620).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackKind {
    Friction,
    ToolRequest,
}

/// One friction or tool_request discovery report (#620).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackReport {
    pub kind: FeedbackKind,
    pub title: String,
    pub text: String,
    /// Stable agent session identifier (AIRC member ID, falling back to nick).
    pub session: String,
    pub nick: String,
    pub project: String,
    #[serde(default)]
    pub project_id: u32,
    #[serde(default)]
    pub sequence: u64,
    pub time: String,
    #[serde(default)]
    pub cost_minutes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_or_step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub would_do: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub would_replace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saving_per_use: Option<String>,
}

impl FeedbackReport {
    pub fn evidence_text(&self) -> String {
        let mut parts = vec![self.text.as_str()];
        if let Some(step) = self.tool_or_step.as_deref() {
            parts.push(step);
        }
        if let Some(name) = self.tool_name.as_deref() {
            parts.push(name);
        }
        if let Some(would_do) = self.would_do.as_deref() {
            parts.push(would_do);
        }
        if let Some(would_replace) = self.would_replace.as_deref() {
            parts.push(would_replace);
        }
        if let Some(saving) = self.saving_per_use.as_deref() {
            parts.push(saving);
        }
        parts.join("\n")
    }

    fn session_key(&self) -> (u32, String, String) {
        let session = if self.session.trim().is_empty() {
            self.nick.as_str()
        } else {
            self.session.as_str()
        };
        (
            self.project_id,
            self.project.to_ascii_lowercase(),
            session.to_ascii_lowercase(),
        )
    }
}

/// Similar friction and tool_request reports grouped per target repository (#620).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackGroup {
    pub repository: ReportRepository,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<u32>,
    #[serde(default)]
    pub reports: Vec<FeedbackReport>,
}

impl FeedbackGroup {
    pub fn distinct_sessions(&self) -> usize {
        self.reports
            .iter()
            .map(FeedbackReport::session_key)
            .collect::<HashSet<_>>()
            .len()
    }

    pub fn total_minutes(&self) -> u32 {
        self.reports
            .iter()
            .map(|report| report.cost_minutes)
            .fold(0_u32, u32::saturating_add)
    }

    pub fn total_tokens(&self) -> u64 {
        self.reports
            .iter()
            .filter_map(|report| report.cost_tokens.map(u64::from))
            .fold(0_u64, u64::saturating_add)
    }

    pub fn meets_threshold(&self) -> bool {
        self.distinct_sessions() >= GROUP_SESSION_THRESHOLD
            || self.total_minutes() >= GROUP_MINUTES_THRESHOLD
    }

    fn contains_entry(&self, project_id: u32, sequence: u64) -> bool {
        sequence != 0
            && self
                .reports
                .iter()
                .any(|report| report.project_id == project_id && report.sequence == sequence)
    }

    fn push_unique(&mut self, report: FeedbackReport) {
        if !self.contains_entry(report.project_id, report.sequence) {
            self.reports.push(report);
        }
    }

    fn entry_refs(&self) -> Vec<(u32, u64)> {
        self.reports
            .iter()
            .filter(|report| report.sequence != 0)
            .map(|report| (report.project_id, report.sequence))
            .collect()
    }
}

/// What grouped feedback reporting did (#620).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedbackOutcome {
    /// Stored in an unfiled group; threshold not reached yet.
    Grouped,
    /// Group reached threshold and filed a new `self-improvement` issue.
    Filed {
        issue: u32,
        entries: Vec<(u32, u64)>,
    },
    /// Report joined an already-filed open issue via an **Also seen by** comment.
    Joined(u32),
    /// Group reached threshold and joined an existing open issue on the host.
    JoinedGroup {
        issue: u32,
        entries: Vec<(u32, u64)>,
    },
}

impl FeedbackOutcome {
    pub fn issue(&self) -> Option<u32> {
        match self {
            Self::Grouped => None,
            Self::Filed { issue, .. } | Self::Joined(issue) | Self::JoinedGroup { issue, .. } => {
                Some(*issue)
            }
        }
    }
}

/// What Velocity has reported from each project's discoveries, keyed by project identifier.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedDiscoveries {
    #[serde(default)]
    pub projects: BTreeMap<u32, ProjectReports>,
    #[serde(default)]
    pub recent: Vec<RecentDiscovery>,
    /// Grouped friction and tool_request reports per target repository (#620).
    #[serde(default)]
    pub feedback_groups: Vec<FeedbackGroup>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentDiscovery {
    repository: ReportRepository,
    issue: u32,
    report: DiscoveryReport,
}

/// One project's reporting progress.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectReports {
    /// Every channel entry up to this sequence has been looked at.
    #[serde(default)]
    pub cursor: u64,
    /// Discovery sequence → the Velocity issue it was filed on or added to.
    #[serde(default)]
    pub issues: BTreeMap<u64, u32>,
    /// Discovery sequence → the Rosen issue. Existing Velocity records still load unchanged.
    #[serde(default)]
    pub rosen_issues: BTreeMap<u64, u32>,
    /// Discovery sequence → the Collapse issue (#620). Existing records still load unchanged.
    #[serde(default)]
    pub collapse_issues: BTreeMap<u64, u32>,
    /// The discovery being reported, saved before GitHub is asked. Still set on the next pass
    /// means the last attempt may have reached GitHub without being recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<u64>,
}

impl ProjectReports {
    pub fn record_issue(&mut self, repository: ReportRepository, sequence: u64, issue: u32) {
        self.record_issue_for_sequence(repository, sequence, issue);
        self.cursor = sequence;
        self.pending = None;
    }

    pub fn record_issue_for_sequence(
        &mut self,
        repository: ReportRepository,
        sequence: u64,
        issue: u32,
    ) {
        match repository {
            ReportRepository::Velocity => self.issues.insert(sequence, issue),
            ReportRepository::Rosen => self.rosen_issues.insert(sequence, issue),
            ReportRepository::Collapse => self.collapse_issues.insert(sequence, issue),
        };
    }
}

impl ReportedDiscoveries {
    pub fn remember(
        &mut self,
        repository: ReportRepository,
        issue: u32,
        mut report: DiscoveryReport,
    ) {
        let now = jiff::Timestamp::now().as_second();
        self.recent.retain(|entry| {
            recorded_second(&entry.report.time)
                .is_some_and(|time| now.abs_diff(time) <= RECENT_SECONDS)
        });
        report
            .text
            .truncate(report.text.floor_char_boundary(QUOTED_TEXT_LIMIT));
        self.recent.push(RecentDiscovery {
            repository,
            issue,
            report,
        });
    }

    /// Reads the saved record. A missing file means nothing has been reported yet; a damaged
    /// one is an error, so reporting stops rather than filing every discovery again.
    pub fn load(app_data: &Path) -> Result<Self, GitHubCommandError> {
        match std::fs::read(app_data.join(REPORTS_FILE)) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    /// Writes the record through a temporary file, so a crash mid-write keeps the old one.
    pub fn save(&self, app_data: &Path) -> Result<(), GitHubCommandError> {
        std::fs::create_dir_all(app_data)?;
        let mut file = tempfile::NamedTempFile::new_in(app_data)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.as_file().sync_all()?;
        file.persist(app_data.join(REPORTS_FILE))
            .map_err(|error| GitHubCommandError::Process(error.error))?;
        Ok(())
    }

    /// The issue a project's discovery was reported on, if it has been.
    pub fn issue(&self, project: u32, sequence: u64) -> Option<u32> {
        self.issue_for(ReportRepository::Velocity, project, sequence)
    }

    pub fn issue_for(
        &self,
        repository: ReportRepository,
        project: u32,
        sequence: u64,
    ) -> Option<u32> {
        let reports = self.projects.get(&project)?;
        let issues = match repository {
            ReportRepository::Velocity => &reports.issues,
            ReportRepository::Rosen => &reports.rosen_issues,
            ReportRepository::Collapse => &reports.collapse_issues,
        };
        issues.get(&sequence).copied()
    }
}

trait ReportBackend {
    fn open_issues(&self) -> Result<Vec<OpenIssue>, GitHubCommandError>;
    fn issue_is_open(&self, number: u32) -> Result<bool, GitHubCommandError>;
    fn issue_comments(&self, number: u32) -> Result<Vec<String>, GitHubCommandError>;
    fn ensure_label(
        &self,
        name: &str,
        color: &str,
        description: &str,
    ) -> Result<(), GitHubCommandError>;
    fn create_issue(
        &self,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<u32, GitHubCommandError>;
    fn add_comment(&self, number: u32, body: &str) -> Result<(), GitHubCommandError>;
}

struct GitHubBackend<'a> {
    repository: &'a str,
}

impl ReportBackend for GitHubBackend<'_> {
    fn open_issues(&self) -> Result<Vec<OpenIssue>, GitHubCommandError> {
        open_issues(self.repository)
    }

    fn issue_is_open(&self, number: u32) -> Result<bool, GitHubCommandError> {
        issue_is_open(self.repository, number)
    }

    fn issue_comments(&self, number: u32) -> Result<Vec<String>, GitHubCommandError> {
        let endpoint = format!("repos/{}/issues/{number}/comments", self.repository);
        let comments = run(&[
            "api",
            "--paginate",
            &endpoint,
            "--jq",
            ".[] | .body | @json",
        ])?;
        comments
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| Ok(serde_json::from_str::<String>(line)?))
            .collect()
    }

    fn ensure_label(
        &self,
        name: &str,
        color: &str,
        description: &str,
    ) -> Result<(), GitHubCommandError> {
        ensure_github_label(self.repository, name, color, description)
    }

    fn create_issue(
        &self,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<u32, GitHubCommandError> {
        file_issue_with_labels(self.repository, title, body, labels)
    }

    fn add_comment(&self, number: u32, body: &str) -> Result<(), GitHubCommandError> {
        let endpoint = format!("repos/{}/issues/{number}/comments", self.repository);
        run(&[
            "api",
            &endpoint,
            "-f",
            &format!("body={body}"),
            "--jq",
            ".id",
        ])?;
        Ok(())
    }
}

struct CollapseBackend {
    transport: CollapseIssueTransport,
}

impl ReportBackend for CollapseBackend {
    fn open_issues(&self) -> Result<Vec<OpenIssue>, GitHubCommandError> {
        Ok(self
            .transport
            .open_issues_fresh()?
            .into_iter()
            .map(|issue| OpenIssue {
                number: issue.number,
                title: issue.title,
                body: issue.body,
            })
            .collect())
    }

    fn issue_is_open(&self, number: u32) -> Result<bool, GitHubCommandError> {
        self.transport
            .read_issue(number)
            .map(|issue| issue.state == IssueState::Open)
    }

    fn issue_comments(&self, number: u32) -> Result<Vec<String>, GitHubCommandError> {
        Ok(self
            .transport
            .issue_comments(number)?
            .into_iter()
            .map(|comment| comment.body)
            .collect())
    }

    fn ensure_label(
        &self,
        name: &str,
        color: &str,
        description: &str,
    ) -> Result<(), GitHubCommandError> {
        self.transport.ensure_label_with(name, color, description)
    }

    fn create_issue(
        &self,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<u32, GitHubCommandError> {
        let owned: Vec<String> = labels.iter().map(|label| (*label).to_owned()).collect();
        self.transport
            .create_issue(title, body, &owned, &[])
            .map(|issue| issue.number)
    }

    fn add_comment(&self, number: u32, body: &str) -> Result<(), GitHubCommandError> {
        self.transport.add_comment(number, body).map(drop)
    }
}

/// Files a discovery, or adds evidence to a matching open issue in its destination on GitHub.
pub fn report_to(
    destination: ReportRepository,
    report: &DiscoveryReport,
    recent: &[RecentDiscovery],
    mentioned_paths: impl FnOnce(&[(&str, &str)]) -> Vec<BTreeSet<String>>,
    retry: bool,
) -> Result<ReportOutcome, GitHubCommandError> {
    report_to_host(
        IssueHost::GitHub,
        None,
        destination,
        report,
        recent,
        mentioned_paths,
        retry,
    )
}

/// Files a discovery, or adds evidence to a matching open issue in its destination on
/// `host` (`GitHub` or `Collapse`) (#590).
pub fn report_to_host(
    host: IssueHost,
    repository_override: Option<&str>,
    destination: ReportRepository,
    report: &DiscoveryReport,
    recent: &[RecentDiscovery],
    mentioned_paths: impl FnOnce(&[(&str, &str)]) -> Vec<BTreeSet<String>>,
    retry: bool,
) -> Result<ReportOutcome, GitHubCommandError> {
    let repository = repository_override.unwrap_or_else(|| destination.repository());
    if host == IssueHost::Collapse {
        let backend = CollapseBackend {
            transport: CollapseIssueTransport::for_repository(repository)?,
        };
        return report_problem_with_backend(
            &backend,
            destination,
            report,
            recent,
            mentioned_paths,
            retry,
        );
    }
    let backend = GitHubBackend { repository };
    report_problem_with_backend(
        &backend,
        destination,
        report,
        recent,
        mentioned_paths,
        retry,
    )
}

fn report_problem_with_backend(
    backend: &dyn ReportBackend,
    destination: ReportRepository,
    report: &DiscoveryReport,
    recent: &[RecentDiscovery],
    mentioned_paths: impl FnOnce(&[(&str, &str)]) -> Vec<BTreeSet<String>>,
    retry: bool,
) -> Result<ReportOutcome, GitHubCommandError> {
    let open = backend.open_issues()?;
    let body = issue_body(report);
    if retry && let Some(previous) = open.iter().find(|issue| issue.body == body) {
        return Ok(ReportOutcome::Filed(previous.number));
    }
    let candidates = matching_issues(destination, report, &open, recent, mentioned_paths);
    for (number, _) in candidates {
        if open.iter().any(|issue| issue.number == number) || backend.issue_is_open(number)? {
            return join_problem_issue(backend, number, report, retry);
        }
    }
    backend
        .create_issue(&report.title, &body, &[])
        .map(ReportOutcome::Filed)
}

/// Groups a friction or tool_request report per target repository by #471 similarity,
/// filing one `self-improvement` issue once three distinct sessions report it or its
/// reported cost reaches 60 agent-minutes, and posting **Also seen by** comments for
/// later reports (#620).
pub fn report_feedback_to_host(
    host: IssueHost,
    repository_override: Option<&str>,
    destination: ReportRepository,
    report: &FeedbackReport,
    groups: &mut Vec<FeedbackGroup>,
    mentioned_paths: impl Fn(&[(&str, &str)]) -> Vec<BTreeSet<String>>,
    retry: bool,
) -> Result<FeedbackOutcome, GitHubCommandError> {
    let repository = repository_override.unwrap_or_else(|| destination.repository());
    if host == IssueHost::Collapse {
        let backend = CollapseBackend {
            transport: CollapseIssueTransport::for_repository(repository)?,
        };
        return report_feedback_with_backend(
            &backend,
            destination,
            report,
            groups,
            &mentioned_paths,
            retry,
        );
    }
    let backend = GitHubBackend { repository };
    report_feedback_with_backend(
        &backend,
        destination,
        report,
        groups,
        &mentioned_paths,
        retry,
    )
}

type PathResolver<'a> = &'a dyn Fn(&[(&str, &str)]) -> Vec<BTreeSet<String>>;

fn report_feedback_with_backend(
    backend: &dyn ReportBackend,
    destination: ReportRepository,
    report: &FeedbackReport,
    groups: &mut Vec<FeedbackGroup>,
    mentioned_paths: PathResolver<'_>,
    retry: bool,
) -> Result<FeedbackOutcome, GitHubCommandError> {
    let matching = matching_feedback_groups(destination, report, groups, mentioned_paths);
    for index in &matching {
        let Some(number) = groups[*index].issue else {
            continue;
        };
        if backend.issue_is_open(number)? {
            let comment = feedback_also_seen_comment(report);
            post_comment_once(backend, number, &comment, retry)?;
            groups[*index].push_unique(report.clone());
            return Ok(FeedbackOutcome::Joined(number));
        }
    }
    let unfiled_index = matching
        .into_iter()
        .find(|index| groups[*index].issue.is_none());
    let mut candidate = match unfiled_index {
        Some(index) => groups[index].clone(),
        None => FeedbackGroup {
            repository: destination,
            title: report.title.clone(),
            issue: None,
            reports: Vec::new(),
        },
    };
    candidate.push_unique(report.clone());
    if !candidate.meets_threshold() {
        commit_feedback_group(groups, unfiled_index, candidate);
        return Ok(FeedbackOutcome::Grouped);
    }
    let outcome = file_or_join_feedback_group(backend, &mut candidate, mentioned_paths, retry)?;
    commit_feedback_group(groups, unfiled_index, candidate);
    Ok(outcome)
}

fn commit_feedback_group(
    groups: &mut Vec<FeedbackGroup>,
    index: Option<usize>,
    group: FeedbackGroup,
) {
    match index {
        Some(index) => groups[index] = group,
        None => groups.push(group),
    }
}

fn file_or_join_feedback_group(
    backend: &dyn ReportBackend,
    group: &mut FeedbackGroup,
    mentioned_paths: PathResolver<'_>,
    retry: bool,
) -> Result<FeedbackOutcome, GitHubCommandError> {
    let open = backend.open_issues()?;
    let body = feedback_group_body(group);
    let entries = group.entry_refs();
    if retry && let Some(previous) = open.iter().find(|issue| issue.body == body) {
        group.issue = Some(previous.number);
        return Ok(FeedbackOutcome::Filed {
            issue: previous.number,
            entries,
        });
    }
    let group_evidence = group_evidence_text(group);
    let probe = DiscoveryReport::new(&group.title, &group_evidence, "", "", "");
    let candidates = matching_issues(group.repository, &probe, &open, &[], mentioned_paths);
    if let Some((number, _)) = candidates.first().copied() {
        let comment = feedback_group_also_seen_comment(group);
        post_comment_once(backend, number, &comment, retry)?;
        group.issue = Some(number);
        return Ok(FeedbackOutcome::JoinedGroup {
            issue: number,
            entries,
        });
    }
    backend.ensure_label(
        SELF_IMPROVEMENT_LABEL,
        SELF_IMPROVEMENT_LABEL_COLOR,
        SELF_IMPROVEMENT_LABEL_DESCRIPTION,
    )?;
    let number = backend.create_issue(&group.title, &body, &[SELF_IMPROVEMENT_LABEL])?;
    group.issue = Some(number);
    Ok(FeedbackOutcome::Filed {
        issue: number,
        entries,
    })
}

fn matching_feedback_groups(
    destination: ReportRepository,
    report: &FeedbackReport,
    groups: &[FeedbackGroup],
    mentioned_paths: PathResolver<'_>,
) -> Vec<usize> {
    let incoming_evidence = report.evidence_text();
    let mut stored: Vec<(usize, String, String)> = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        if group.repository != destination {
            continue;
        }
        for item in &group.reports {
            stored.push((index, item.title.clone(), item.evidence_text()));
        }
    }
    if stored.is_empty() {
        return Vec::new();
    }
    let mut descriptions = Vec::with_capacity(stored.len() + 1);
    descriptions.push((report.title.as_str(), incoming_evidence.as_str()));
    for (_, title, evidence) in &stored {
        descriptions.push((title.as_str(), evidence.as_str()));
    }
    let mut paths = mentioned_paths(&descriptions).into_iter();
    let wanted = IssueProblem::new(
        &report.title,
        &incoming_evidence,
        paths.next().unwrap_or_default(),
    );
    let mut best_scores: BTreeMap<usize, f64> = BTreeMap::new();
    for (index, title, evidence) in &stored {
        let candidate = IssueProblem::new(title, evidence, paths.next().unwrap_or_default());
        if let Some(score) = wanted.match_score(&candidate) {
            let best = best_scores.entry(*index).or_default();
            *best = f64::max(*best, score);
        }
    }
    let mut ranked: Vec<_> = best_scores.into_iter().collect();
    ranked.sort_by(|left, right| right.1.total_cmp(&left.1).then(left.0.cmp(&right.0)));
    ranked.into_iter().map(|(index, _)| index).collect()
}

fn group_evidence_text(group: &FeedbackGroup) -> String {
    group
        .reports
        .iter()
        .map(FeedbackReport::evidence_text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Infers the destination tool repository for a friction or tool_request entry when the
/// agent did not specify an explicit `velocity_issue`, `rosen_issue` or `collapse_issue`.
pub fn infer_feedback_repository(
    text: &str,
    tool_or_step: Option<&str>,
    tool_name: Option<&str>,
) -> ReportRepository {
    let combined = format!(
        "{} {} {}",
        tool_or_step.unwrap_or_default(),
        tool_name.unwrap_or_default(),
        text
    )
    .to_ascii_lowercase();
    let mentions_rosen = combined.contains("rosen");
    let mentions_collapse = combined.contains("collapse");
    let mentions_velocity = combined.contains("velocity");
    if mentions_collapse && !mentions_rosen && !mentions_velocity {
        ReportRepository::Collapse
    } else if mentions_rosen && !mentions_collapse && !mentions_velocity {
        ReportRepository::Rosen
    } else {
        ReportRepository::Velocity
    }
}

/// Derives a concise single-line title (at most 200 chars) for a friction or tool_request
/// discovery when no explicit `*_issue` title was supplied.
pub fn default_feedback_title(
    kind: FeedbackKind,
    text: &str,
    tool_or_step: Option<&str>,
    tool_name: Option<&str>,
    would_do: Option<&str>,
) -> String {
    let raw = match kind {
        FeedbackKind::ToolRequest => match (tool_name, would_do) {
            (Some(name), Some(action)) => format!("{name}: {action}"),
            (Some(name), None) => name.to_owned(),
            _ => first_line(text).to_owned(),
        },
        FeedbackKind::Friction => match tool_or_step {
            Some(step) => format!("{step}: {}", first_line(text)),
            None => first_line(text).to_owned(),
        },
    };
    let clean = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    clean.chars().take(200).collect()
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(text.trim())
}

/// Extracts an estimated duration in minutes from free-form text such as `"15 minutes"`,
/// `"10m"`, or `"1.5 hours"`. Returns `None` when no explicit minute/hour unit appears.
pub fn parse_minutes_from_text(text: &str) -> Option<u32> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
            index += 1;
        }
        let number = text[start..index].parse::<f64>().ok()?;
        let mut unit_start = index;
        while unit_start < bytes.len() && bytes[unit_start].is_ascii_whitespace() {
            unit_start += 1;
        }
        let mut unit_end = unit_start;
        while unit_end < bytes.len() && bytes[unit_end].is_ascii_alphabetic() {
            unit_end += 1;
        }
        let unit = text[unit_start..unit_end].to_ascii_lowercase();
        let minutes = match unit.as_str() {
            "m" | "min" | "mins" | "minute" | "minutes" => Some(number),
            "h" | "hr" | "hrs" | "hour" | "hours" => Some(number * 60.0),
            _ => None,
        };
        if let Some(minutes) = minutes.filter(|value| value.is_finite() && *value >= 0.0) {
            return Some(minutes.ceil().min(f64::from(u32::MAX)) as u32);
        }
    }
    None
}

/// Links the issues a discovery names on its `Blocked by:` lines to the issue it was
/// reported on, as issue dependencies (#511). A bare `#12` is an issue of the
/// destination repository. Links that exist are kept, so a retried report is safe.
/// Returns the blockers the host refused, such as an issue that does not exist: those are
/// final, while any other failure is an error the next pass retries.
pub fn link_declared_blockers(
    destination: ReportRepository,
    issue: u32,
    report: &DiscoveryReport,
) -> Result<Vec<String>, GitHubCommandError> {
    link_declared_blockers_on(IssueHost::GitHub, None, destination, issue, report)
}

/// Links the issues a discovery names on its `Blocked by:` lines on `host` (#590).
pub fn link_declared_blockers_on(
    host: IssueHost,
    repository_override: Option<&str>,
    destination: ReportRepository,
    issue: u32,
    report: &DiscoveryReport,
) -> Result<Vec<String>, GitHubCommandError> {
    let repository = repository_override.unwrap_or_else(|| destination.repository());
    let blockers: Vec<_> = declared_blockers(&report.text)
        .into_iter()
        .filter(|blocker| !blocker.is_issue(repository, repository, issue))
        .collect();
    if blockers.is_empty() {
        return Ok(Vec::new());
    }
    if host == IssueHost::Collapse {
        return link_declared_blockers_collapse(repository, issue, &blockers);
    }
    let endpoint = format!("repos/{repository}/issues/{issue}/dependencies/blocked_by");
    let linked: HashSet<u64> = run(&["api", "--paginate", &endpoint, "--jq", ".[].id"])?
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect();
    let mut refused = Vec::new();
    for blocker in blockers {
        let name = blocker.relative_to(repository);
        match link_blocker(repository, &endpoint, &linked, &blocker) {
            Ok(true) => {}
            Ok(false) => refused.push(name),
            Err(error) if is_refusal(&error) => refused.push(name),
            Err(error) => return Err(error),
        }
    }
    Ok(refused)
}

fn link_declared_blockers_collapse(
    repository: &str,
    issue: u32,
    blockers: &[IssueReference],
) -> Result<Vec<String>, GitHubCommandError> {
    let collapse = CollapseIssueTransport::for_repository(repository)?;
    let existing = collapse.read_issue(issue)?;
    let linked: HashSet<(String, u32)> = existing
        .open_blockers
        .iter()
        .chain(existing.closed_blockers.iter())
        .filter_map(|item| IssueReference::parse(item))
        .map(|reference| {
            (
                reference.repository_or(repository).to_ascii_lowercase(),
                reference.number(),
            )
        })
        .collect();
    let mut refused = Vec::new();
    for blocker in blockers {
        let key = (
            blocker.repository_or(repository).to_ascii_lowercase(),
            blocker.number(),
        );
        if linked.contains(&key) {
            continue;
        }
        let name = blocker.relative_to(repository);
        match collapse.add_dependency(issue, blocker) {
            Ok(()) => {}
            Err(error) if is_refusal(&error) => refused.push(name),
            Err(error) => return Err(error),
        }
    }
    Ok(refused)
}

/// `false` when the blocker is a pull request, which GitHub cannot link.
fn link_blocker(
    repository: &str,
    endpoint: &str,
    linked: &HashSet<u64>,
    blocker: &IssueReference,
) -> Result<bool, GitHubCommandError> {
    // The dependency API takes the blocker's database ID, which reaches other repositories.
    let found = run(&[
        "api",
        &format!(
            "repos/{}/issues/{}",
            blocker.repository_or(repository),
            blocker.number()
        ),
        "--jq",
        "select(.pull_request == null) | .id",
    ])?;
    let Ok(identifier) = found.trim().parse::<u64>() else {
        return Ok(false);
    };
    if !linked.contains(&identifier) {
        run(&[
            "api",
            "--method",
            "POST",
            endpoint,
            "-F",
            &format!("issue_id={identifier}"),
            "--jq",
            ".id",
        ])?;
    }
    Ok(true)
}

/// GitHub answered that this link cannot exist: a missing or removed issue, or a
/// dependency it rejects. Retrying would fail the same way and hold up later reports.
fn is_refusal(error: &GitHubCommandError) -> bool {
    matches!(error, GitHubCommandError::Command { message, .. }
        if ["HTTP 404", "HTTP 410", "HTTP 422"].iter().any(|status| message.contains(status)))
}

fn matching_issues(
    destination: ReportRepository,
    report: &DiscoveryReport,
    open: &[OpenIssue],
    recent: &[RecentDiscovery],
    mentioned_paths: impl FnOnce(&[(&str, &str)]) -> Vec<BTreeSet<String>>,
) -> Vec<(u32, f64)> {
    let open_reports = open.iter().map(|issue| {
        let evidence = issue
            .body
            .split_once("\n\n> ")
            .map_or(issue.body.as_str(), |(_, text)| text);
        (issue.number, (issue.title.as_str(), evidence))
    });
    let recent_reports = recent
        .iter()
        .filter(|entry| entry.is_recent(destination, report))
        .map(|entry| {
            (
                entry.issue,
                (entry.report.title.as_str(), entry.report.text.as_str()),
            )
        });
    let mut numbers = Vec::new();
    let mut descriptions = vec![(report.title.as_str(), report.text.as_str())];
    for (number, description) in open_reports.chain(recent_reports) {
        numbers.push(number);
        descriptions.push(description);
    }
    let mut paths = mentioned_paths(&descriptions).into_iter();
    let wanted = IssueProblem::new(
        &report.title,
        &report.text,
        paths.next().unwrap_or_default(),
    );
    let mut matches = BTreeMap::new();
    for (number, (title, evidence)) in numbers.into_iter().zip(descriptions.into_iter().skip(1)) {
        let problem = IssueProblem::new(title, evidence, paths.next().unwrap_or_default());
        if let Some(score) = wanted.match_score(&problem) {
            let best = matches.entry(number).or_default();
            *best = f64::max(*best, score);
        }
    }
    let mut matches: Vec<_> = matches.into_iter().collect();
    matches.sort_by(|left, right| right.1.total_cmp(&left.1).then(left.0.cmp(&right.0)));
    matches
}

impl RecentDiscovery {
    fn is_recent(&self, repository: ReportRepository, report: &DiscoveryReport) -> bool {
        self.repository == repository
            && recorded_second(&self.report.time)
                .zip(recorded_second(&report.time))
                .is_some_and(|(left, right)| left.abs_diff(right) <= RECENT_SECONDS)
    }
}

fn recorded_second(time: &str) -> Option<i64> {
    time.parse::<jiff::Timestamp>()
        .ok()
        .map(|time| time.as_second())
}

fn join_problem_issue(
    backend: &dyn ReportBackend,
    number: u32,
    report: &DiscoveryReport,
    retry: bool,
) -> Result<ReportOutcome, GitHubCommandError> {
    let evidence = quote(&report.text);
    let body = format!(
        "Also seen by `{}` in `{}` ({}):\n\n{evidence}",
        report.nick, report.project, report.time
    );
    if retry {
        let previous = format!(
            "Seen again by `{}` in project `{}` ({}):\n\n{evidence}",
            report.nick, report.project, report.time
        );
        let comments = backend.issue_comments(number)?;
        if comments
            .iter()
            .any(|comment| comment == &body || comment == &previous)
        {
            return Ok(ReportOutcome::Joined(number));
        }
    }
    backend.add_comment(number, &body)?;
    Ok(ReportOutcome::Joined(number))
}

fn post_comment_once(
    backend: &dyn ReportBackend,
    number: u32,
    body: &str,
    retry: bool,
) -> Result<(), GitHubCommandError> {
    if retry {
        let comments = backend.issue_comments(number)?;
        if comments.iter().any(|comment| comment == body) {
            return Ok(());
        }
    }
    backend.add_comment(number, body)
}

fn feedback_group_body(group: &FeedbackGroup) -> String {
    let cost = format_total_cost(group.total_minutes(), group.total_tokens());
    let mut lines = vec![
        "Grouped self-improvement reports from AIRC discoveries.".to_owned(),
        String::new(),
        format!(
            "- **Reports:** {} ({} sessions)",
            group.reports.len(),
            group.distinct_sessions()
        ),
        format!("- **Total cost:** {cost}"),
        String::new(),
        "### Reports".to_owned(),
        String::new(),
    ];
    let per_report_limit = (QUOTED_TEXT_LIMIT / group.reports.len().max(1)).clamp(500, 4_000);
    for (index, report) in group.reports.iter().enumerate() {
        lines.push(format_feedback_item(index + 1, report, per_report_limit));
    }
    let body = lines.join("\n");
    truncate_body(body, QUOTED_TEXT_LIMIT)
}

fn feedback_group_also_seen_comment(group: &FeedbackGroup) -> String {
    let cost = format_total_cost(group.total_minutes(), group.total_tokens());
    let mut lines = vec![format!(
        "Also seen by {} agent sessions ({} reports, total cost: {cost}):",
        group.distinct_sessions(),
        group.reports.len()
    )];
    lines.push(String::new());
    let per_report_limit = (QUOTED_TEXT_LIMIT / group.reports.len().max(1)).clamp(500, 4_000);
    for (index, report) in group.reports.iter().enumerate() {
        lines.push(format_feedback_item(index + 1, report, per_report_limit));
    }
    truncate_body(lines.join("\n"), QUOTED_TEXT_LIMIT)
}

fn feedback_also_seen_comment(report: &FeedbackReport) -> String {
    let mut lines = vec![
        format!(
            "Also seen by `{}` in `{}` ({}):",
            report.nick, report.project, report.time
        ),
        String::new(),
    ];
    append_feedback_metadata(&mut lines, report);
    lines.push(String::new());
    lines.push(quote_limited(&report.text, 4_000));
    truncate_body(lines.join("\n"), QUOTED_TEXT_LIMIT)
}

fn format_feedback_item(number: usize, report: &FeedbackReport, text_limit: usize) -> String {
    let kind_header = match report.kind {
        FeedbackKind::Friction => match report.tool_or_step.as_deref() {
            Some(step) => format!("**Friction** (`{step}`)"),
            None => "**Friction**".to_owned(),
        },
        FeedbackKind::ToolRequest => match report.tool_name.as_deref() {
            Some(name) => format!("**Tool request** (`{name}`)"),
            None => "**Tool request**".to_owned(),
        },
    };
    let cost = format_report_cost(report.cost_minutes, report.cost_tokens);
    let mut lines = vec![format!(
        "{number}. {kind_header} · Agent: `{}` · Project: `{}` · Recorded: {} · Cost: {cost}",
        report.nick, report.project, report.time
    )];
    if let Some(would_do) = report.would_do.as_deref() {
        lines.push(format!("   - **Would do:** {would_do}"));
    }
    if let Some(would_replace) = report.would_replace.as_deref() {
        lines.push(format!("   - **Would replace:** {would_replace}"));
    }
    if let Some(saving) = report.saving_per_use.as_deref() {
        lines.push(format!("   - **Estimated saving per use:** {saving}"));
    }
    lines.push(quote_limited(&report.text, text_limit));
    lines.push(String::new());
    lines.join("\n")
}

fn append_feedback_metadata(lines: &mut Vec<String>, report: &FeedbackReport) {
    match report.kind {
        FeedbackKind::Friction => {
            lines.push(format!(
                "- **Kind:** friction{}",
                report
                    .tool_or_step
                    .as_deref()
                    .map_or_else(String::new, |step| format!(" (`{step}`)"))
            ));
        }
        FeedbackKind::ToolRequest => {
            lines.push(format!(
                "- **Kind:** tool_request{}",
                report
                    .tool_name
                    .as_deref()
                    .map_or_else(String::new, |name| format!(" (`{name}`)"))
            ));
        }
    }
    if let Some(would_do) = report.would_do.as_deref() {
        lines.push(format!("- **Would do:** {would_do}"));
    }
    if let Some(would_replace) = report.would_replace.as_deref() {
        lines.push(format!("- **Would replace:** {would_replace}"));
    }
    if let Some(saving) = report.saving_per_use.as_deref() {
        lines.push(format!("- **Estimated saving per use:** {saving}"));
    }
    lines.push(format!(
        "- **Cost:** {}",
        format_report_cost(report.cost_minutes, report.cost_tokens)
    ));
}

fn format_total_cost(minutes: u32, tokens: u64) -> String {
    match (minutes, tokens) {
        (mins, 0) => format!("{mins} agent-minutes"),
        (0, toks) => format!("~{toks} tokens"),
        (mins, toks) => format!("{mins} agent-minutes (~{toks} tokens)"),
    }
}

fn format_report_cost(minutes: u32, tokens: Option<u32>) -> String {
    format_total_cost(minutes, u64::from(tokens.unwrap_or(0)))
}

fn truncate_body(text: String, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((cut, _)) => format!("{}\n… (cut at {limit} characters)", &text[..cut]),
        None => text,
    }
}

fn issue_body(report: &DiscoveryReport) -> String {
    format!(
        "Reported by an agent from an AIRC discovery.\n\n- **Agent:** `{}`\n- **Project:** `{}`\n- **Recorded:** {}\n\n{}\n",
        report.nick,
        report.project,
        report.time,
        quote(&report.text)
    )
}

fn ensure_github_label(
    repository: &str,
    name: &str,
    color: &str,
    description: &str,
) -> Result<(), GitHubCommandError> {
    let endpoint = format!("repos/{repository}/labels/{name}");
    if run(&["api", &endpoint, "--jq", ".name"]).is_ok() {
        return Ok(());
    }
    let create = format!("repos/{repository}/labels");
    if let Err(error) = run(&[
        "api",
        "--method",
        "POST",
        &create,
        "-f",
        &format!("name={name}"),
        "-f",
        &format!("color={color}"),
        "-f",
        &format!("description={description}"),
        "--jq",
        ".name",
    ]) {
        // Another process may have created the label concurrently (HTTP 422 already_exists).
        run(&["api", &endpoint, "--jq", ".name"]).map_err(|_| error)?;
    }
    Ok(())
}

fn file_issue_with_labels(
    repository: &str,
    title: &str,
    body: &str,
    labels: &[&str],
) -> Result<u32, GitHubCommandError> {
    let endpoint = format!("repos/{repository}/issues");
    let title_arg = format!("title={title}");
    let body_arg = format!("body={body}");
    let label_args: Vec<String> = labels
        .iter()
        .map(|label| format!("labels[]={label}"))
        .collect();
    let mut args = vec!["api", &endpoint, "-f", &title_arg, "-f", &body_arg];
    for label_arg in &label_args {
        args.push("-f");
        args.push(label_arg);
    }
    args.extend(["--jq", ".number"]);
    let number = run(&args)?;
    number
        .trim()
        .parse()
        .map_err(|_| GitHubCommandError::Missing("the new issue's number".into()))
}

#[derive(Deserialize)]
struct OpenIssue {
    number: u32,
    title: String,
    body: String,
}

fn open_issues(repository: &str) -> Result<Vec<OpenIssue>, GitHubCommandError> {
    let output = run(&[
        "api",
        "--paginate",
        &format!("repos/{repository}/issues?state=open&per_page=100"),
        "--jq",
        ".[] | select(.pull_request == null) | {number,title,body:(.body // \"\")} | @json",
    ])?;
    output
        .lines()
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}

fn issue_is_open(repository: &str, number: u32) -> Result<bool, GitHubCommandError> {
    let state = run(&[
        "api",
        &format!("repos/{repository}/issues/{number}"),
        "--jq",
        ".state",
    ])?;
    Ok(state.trim() == "open")
}

/// Issue text and code references used by both filing and continuous dispatch.
pub struct IssueProblem {
    title: String,
    title_words: HashSet<String>,
    evidence_words: HashSet<String>,
    pub paths: BTreeSet<String>,
}

impl IssueProblem {
    pub fn new(title: &str, evidence: &str, paths: BTreeSet<String>) -> Self {
        Self {
            title: title.trim().to_lowercase(),
            title_words: significant_words(title),
            evidence_words: significant_words(evidence),
            paths,
        }
    }

    pub fn matches(&self, other: &Self) -> bool {
        self.match_score(other).is_some()
    }

    pub fn overlaps(&self, other: &Self) -> bool {
        !self.paths.is_disjoint(&other.paths) || self.matches(other)
    }

    fn match_score(&self, other: &Self) -> Option<f64> {
        if self.title == other.title && !self.title.is_empty() {
            return Some(2.0);
        }
        let title_shared = self.title_words.intersection(&other.title_words).count();
        let title_union = self.title_words.union(&other.title_words).count().max(1);
        let title_overlap = title_shared as f64 / title_union as f64;
        let evidence_shared = self
            .evidence_words
            .intersection(&other.evidence_words)
            .count();
        let evidence_union = self
            .evidence_words
            .union(&other.evidence_words)
            .count()
            .max(1);
        let evidence_overlap = evidence_shared as f64 / evidence_union as f64;
        let same_code = !self.paths.is_disjoint(&other.paths);
        let corroborated = (title_shared >= 2 && same_code && evidence_shared >= 2)
            || (title_shared >= 4 && evidence_shared >= 4 && evidence_overlap >= 0.10);
        (title_shared >= 2 && (title_overlap >= SAME_PROBLEM || corroborated))
            .then_some(title_overlap + evidence_overlap / 2.0)
    }
}

/// Returns the highest-scoring open issue matching `(title, body)` under the #471
/// problem matcher, using code paths resolved through the project graph when available.
pub fn find_matching_open_issue<'a>(
    title: &str,
    body: &str,
    open: &'a [GitHubIssue],
    mentioned_paths: impl FnOnce(&[(&str, &str)]) -> Vec<BTreeSet<String>>,
) -> Option<&'a GitHubIssue> {
    if open.is_empty() {
        return None;
    }
    let mut descriptions = Vec::with_capacity(open.len() + 1);
    descriptions.push((title, body));
    for issue in open {
        descriptions.push((issue.title.as_str(), issue.body.as_str()));
    }
    let mut paths = mentioned_paths(&descriptions).into_iter();
    let wanted = IssueProblem::new(title, body, paths.next().unwrap_or_default());
    let mut best: Option<(&'a GitHubIssue, f64)> = None;
    for issue in open {
        let candidate =
            IssueProblem::new(&issue.title, &issue.body, paths.next().unwrap_or_default());
        let Some(score) = wanted.match_score(&candidate) else {
            continue;
        };
        let replaces = best.is_none_or(|(current, current_score)| {
            score.total_cmp(&current_score).is_gt()
                || (score.total_cmp(&current_score).is_eq() && issue.number < current.number)
        });
        if replaces {
            best = Some((issue, score));
        }
    }
    best.map(|(issue, _)| issue)
}

/// Normalize inflections so the same failure can be described in different words.
fn significant_words(text: &str) -> HashSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.len() >= 3 && !word.bytes().any(|byte| byte.is_ascii_digit()))
        .map(str::to_lowercase)
        .filter(|word| !STOP_WORDS.contains(&word.as_str()))
        .map(|word| {
            if let Some(stem) = word.strip_suffix("ies").filter(|stem| stem.len() >= 3) {
                return format!("{stem}y");
            }
            let stem = ["ing", "ed", "s"]
                .into_iter()
                .find_map(|suffix| word.strip_suffix(suffix).filter(|stem| stem.len() >= 3))
                .unwrap_or(&word);
            // Strip a trailing silent e so e.g. "refused" and "refusing" meet "refuse".
            stem.strip_suffix('e')
                .filter(|stem| stem.len() >= 3)
                .unwrap_or(stem)
                .to_owned()
        })
        .collect()
}

fn quote(text: &str) -> String {
    quote_limited(text, QUOTED_TEXT_LIMIT)
}

fn quote_limited(text: &str, limit: usize) -> String {
    let shortened = match text.char_indices().nth(limit) {
        Some((cut, _)) => format!(
            "{}
… (cut at {limit} characters)",
            &text[..cut]
        ),
        None => text.to_owned(),
    };
    shortened
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn run(arguments: &[&str]) -> Result<String, GitHubCommandError> {
    let output = gh(arguments).output()?;
    if !output.status.success() {
        return Err(GitHubCommandError::Command {
            code: output.status.code(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeBackend {
        labels: RefCell<Vec<String>>,
        issues: RefCell<Vec<FakeIssue>>,
        comments: RefCell<Vec<(u32, String)>>,
    }

    struct FakeIssue {
        number: u32,
        title: String,
        body: String,
        labels: Vec<String>,
        open: bool,
    }

    impl ReportBackend for FakeBackend {
        fn open_issues(&self) -> Result<Vec<OpenIssue>, GitHubCommandError> {
            Ok(self
                .issues
                .borrow()
                .iter()
                .filter(|issue| issue.open)
                .map(|issue| OpenIssue {
                    number: issue.number,
                    title: issue.title.clone(),
                    body: issue.body.clone(),
                })
                .collect())
        }

        fn issue_is_open(&self, number: u32) -> Result<bool, GitHubCommandError> {
            Ok(self
                .issues
                .borrow()
                .iter()
                .find(|issue| issue.number == number)
                .is_some_and(|issue| issue.open))
        }

        fn issue_comments(&self, number: u32) -> Result<Vec<String>, GitHubCommandError> {
            Ok(self
                .comments
                .borrow()
                .iter()
                .filter(|(issue, _)| *issue == number)
                .map(|(_, body)| body.clone())
                .collect())
        }

        fn ensure_label(
            &self,
            name: &str,
            _color: &str,
            _description: &str,
        ) -> Result<(), GitHubCommandError> {
            let mut labels = self.labels.borrow_mut();
            if !labels.iter().any(|existing| existing == name) {
                labels.push(name.to_owned());
            }
            Ok(())
        }

        fn create_issue(
            &self,
            title: &str,
            body: &str,
            labels: &[&str],
        ) -> Result<u32, GitHubCommandError> {
            let mut issues = self.issues.borrow_mut();
            let number = u32::try_from(issues.len() + 1).unwrap_or(u32::MAX);
            issues.push(FakeIssue {
                number,
                title: title.to_owned(),
                body: body.to_owned(),
                labels: labels.iter().map(|label| (*label).to_owned()).collect(),
                open: true,
            });
            Ok(number)
        }

        fn add_comment(&self, number: u32, body: &str) -> Result<(), GitHubCommandError> {
            self.comments.borrow_mut().push((number, body.to_owned()));
            Ok(())
        }
    }

    fn tool_request(
        session: &str,
        project: &str,
        sequence: u64,
        title: &str,
        minutes: u32,
    ) -> FeedbackReport {
        FeedbackReport {
            kind: FeedbackKind::ToolRequest,
            title: title.into(),
            text: "Return only failing test output instead of full logs.".into(),
            session: session.into(),
            nick: session.into(),
            project: project.into(),
            project_id: 1,
            sequence,
            time: format!("2026-11-22T12:0{sequence}:00Z"),
            cost_minutes: minutes,
            cost_tokens: None,
            tool_or_step: None,
            tool_name: Some("failing_test_output".into()),
            would_do: Some("Return only the failing test output".into()),
            would_replace: Some("Scanning megabytes of integration logs".into()),
            saving_per_use: Some(format!("{minutes} minutes")),
        }
    }

    fn no_paths(_: &[(&str, &str)]) -> Vec<BTreeSet<String>> {
        Vec::new()
    }

    // Risk (#620): three similar tool requests from distinct sessions must file one
    // `self-improvement` issue (creating the label if needed), and a fourth must append
    // an **Also seen by** comment instead of opening another issue.
    #[test]
    fn three_distinct_sessions_file_self_improvement_and_fourth_comments()
    -> Result<(), GitHubCommandError> {
        let backend = FakeBackend::default();
        let mut groups = Vec::new();
        let first = tool_request(
            "issue-10-1",
            "velocity",
            1,
            "Return only failing test output",
            10,
        );
        let repeat_same_session = tool_request(
            "issue-10-1",
            "velocity",
            2,
            "Return only failing test output",
            10,
        );
        let second = tool_request(
            "issue-11-2",
            "rosen",
            3,
            "Return only failing test output quickly",
            12,
        );
        let third = tool_request(
            "issue-12-3",
            "collapse.sh",
            4,
            "Return only failing test output from runs",
            8,
        );
        let fourth = tool_request(
            "issue-13-4",
            "velocity",
            5,
            "Return only failing test output in runs",
            5,
        );

        assert_eq!(
            report_feedback_with_backend(
                &backend,
                ReportRepository::Velocity,
                &first,
                &mut groups,
                &no_paths,
                false,
            )?,
            FeedbackOutcome::Grouped
        );
        assert_eq!(
            report_feedback_with_backend(
                &backend,
                ReportRepository::Velocity,
                &repeat_same_session,
                &mut groups,
                &no_paths,
                false,
            )?,
            FeedbackOutcome::Grouped
        );
        assert_eq!(
            report_feedback_with_backend(
                &backend,
                ReportRepository::Velocity,
                &second,
                &mut groups,
                &no_paths,
                false,
            )?,
            FeedbackOutcome::Grouped
        );
        assert!(backend.issues.borrow().is_empty());

        let filed = report_feedback_with_backend(
            &backend,
            ReportRepository::Velocity,
            &third,
            &mut groups,
            &no_paths,
            false,
        )?;
        assert_eq!(
            filed,
            FeedbackOutcome::Filed {
                issue: 1,
                entries: vec![(1, 1), (1, 2), (1, 3), (1, 4)],
            }
        );
        assert_eq!(
            backend.labels.borrow().as_slice(),
            &[SELF_IMPROVEMENT_LABEL]
        );
        {
            let issues = backend.issues.borrow();
            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].labels, vec![SELF_IMPROVEMENT_LABEL]);
            assert!(issues[0].body.contains("40 agent-minutes"));
            assert!(issues[0].body.contains("`issue-10-1`"));
            assert!(issues[0].body.contains("`issue-11-2`"));
            assert!(issues[0].body.contains("`issue-12-3`"));
            assert!(issues[0].body.contains("`collapse.sh`"));
        }

        let joined = report_feedback_with_backend(
            &backend,
            ReportRepository::Velocity,
            &fourth,
            &mut groups,
            &no_paths,
            false,
        )?;
        assert_eq!(joined, FeedbackOutcome::Joined(1));
        let comments = backend.comments.borrow();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].0, 1);
        assert!(
            comments[0]
                .1
                .starts_with("Also seen by `issue-13-4` in `velocity`")
        );
        Ok(())
    }

    // Risk (#620): reaching 60 agent-minutes must file a `self-improvement` issue even
    // before three distinct agent sessions report it.
    #[test]
    fn sixty_agent_minutes_threshold_files_before_three_sessions() -> Result<(), GitHubCommandError>
    {
        let backend = FakeBackend::default();
        let mut groups = Vec::new();
        let first = FeedbackReport {
            kind: FeedbackKind::Friction,
            title: "Build queue wait times during clippy".into(),
            text: "Queued clippy waited 35 minutes.".into(),
            session: "issue-20-1".into(),
            nick: "issue-20-1".into(),
            project: "velocity".into(),
            project_id: 1,
            sequence: 10,
            time: "2026-11-22T12:10:00Z".into(),
            cost_minutes: 35,
            cost_tokens: Some(4_000),
            tool_or_step: Some("velocity-run preflight".into()),
            tool_name: None,
            would_do: None,
            would_replace: None,
            saving_per_use: None,
        };
        let mut second = first.clone();
        second.session = "issue-21-2".into();
        second.nick = "issue-21-2".into();
        second.sequence = 11;
        second.cost_minutes = 25;
        second.cost_tokens = Some(2_000);

        assert_eq!(
            report_feedback_with_backend(
                &backend,
                ReportRepository::Velocity,
                &first,
                &mut groups,
                &no_paths,
                false,
            )?,
            FeedbackOutcome::Grouped
        );
        let outcome = report_feedback_with_backend(
            &backend,
            ReportRepository::Velocity,
            &second,
            &mut groups,
            &no_paths,
            false,
        )?;
        assert_eq!(
            outcome,
            FeedbackOutcome::Filed {
                issue: 1,
                entries: vec![(1, 10), (1, 11)],
            }
        );
        let issues = backend.issues.borrow();
        assert_eq!(issues.len(), 1);
        assert!(issues[0].body.contains("60 agent-minutes (~6000 tokens)"));
        Ok(())
    }

    // Risk (#620): reports targeting different suite repositories must stay isolated,
    // Collapse discoveries must route to `mESmaC/collapse.sh` using GitHub by default,
    // and persisted records must remain backwards-compatible with older `velocity-reports.json`.
    #[test]
    fn repositories_stay_isolated_and_collapse_routes_to_collapse_repo()
    -> Result<(), GitHubCommandError> {
        let velocity_backend = FakeBackend::default();
        let collapse_backend = FakeBackend::default();
        let mut groups = Vec::new();

        let velocity_report = tool_request("s1", "velocity", 1, "List open claims on a path", 20);
        let rosen_report = tool_request("s2", "velocity", 2, "List open claims on a path", 20);
        let collapse_report = tool_request("s3", "velocity", 3, "List open claims on a path", 20);

        assert_eq!(
            report_feedback_with_backend(
                &velocity_backend,
                ReportRepository::Velocity,
                &velocity_report,
                &mut groups,
                &no_paths,
                false,
            )?,
            FeedbackOutcome::Grouped
        );
        assert_eq!(
            report_feedback_with_backend(
                &velocity_backend,
                ReportRepository::Rosen,
                &rosen_report,
                &mut groups,
                &no_paths,
                false,
            )?,
            FeedbackOutcome::Grouped
        );
        assert_eq!(
            report_feedback_with_backend(
                &collapse_backend,
                ReportRepository::Collapse,
                &collapse_report,
                &mut groups,
                &no_paths,
                false,
            )?,
            FeedbackOutcome::Grouped
        );
        assert_eq!(groups.len(), 3);
        assert!(velocity_backend.issues.borrow().is_empty());

        let problem = DiscoveryReport::new(
            "Collapse API 500 on empty label filter",
            "GET /issues?labels= returns 500.",
            "issue-620-369",
            "velocity",
            "2026-11-22T12:30:00Z",
        );
        let filed = report_problem_with_backend(
            &collapse_backend,
            ReportRepository::Collapse,
            &problem,
            &[],
            no_paths,
            false,
        )?;
        assert_eq!(filed, ReportOutcome::Filed(1));
        assert_eq!(ReportRepository::Collapse.repository(), COLLAPSE_REPOSITORY);
        assert_eq!(
            ReportRepository::Collapse.default_host(IssueHost::Collapse),
            IssueHost::GitHub
        );
        assert_eq!(
            infer_feedback_repository("Collapse release endpoint times out", None, None),
            ReportRepository::Collapse
        );
        assert_eq!(
            infer_feedback_repository("Rebase failed", Some("rosen rebase"), None),
            ReportRepository::Rosen
        );
        assert_eq!(
            parse_minutes_from_text("15 minutes and 3k tokens"),
            Some(15)
        );
        assert_eq!(parse_minutes_from_text("1.5 hours"), Some(90));
        assert_eq!(parse_minutes_from_text("~8k tokens"), None);

        let legacy = r#"{"projects":{"1":{"cursor":5,"issues":{"5":99}}}}"#;
        let mut parsed: ReportedDiscoveries = serde_json::from_str(legacy)?;
        parsed.feedback_groups = groups;
        let roundtrip: ReportedDiscoveries =
            serde_json::from_str(&serde_json::to_string(&parsed)?)?;
        assert_eq!(roundtrip.issue(1, 5), Some(99));
        assert_eq!(roundtrip.feedback_groups.len(), 3);
        Ok(())
    }
}
