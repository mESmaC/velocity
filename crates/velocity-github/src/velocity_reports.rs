//! Tool issues from agents' discoveries (F14 Discoveries): marked discoveries are filed on
//! Velocity's or Rosen's repository, keeping each destination's issues separate. A report
//! that matches an open issue's problem is added to that issue as a comment instead of opening another.
//! Everything goes through GitHub's REST API, which keeps working when a swarm has spent the
//! GraphQL budget.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use velocity_core::issue_dependencies::{IssueReference, declared_blockers};

use crate::GitHubIssue;
use crate::command::gh;
use crate::gh_command::GitHubCommandError;

/// Where Velocity's own issues live.
pub const VELOCITY_REPOSITORY: &str = "mESmaC/velocity";
pub const ROSEN_REPOSITORY: &str = "mESmaC/rosen";

/// A discovery reports a problem in one of the tools, independently of the project using it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ReportRepository {
    Velocity,
    Rosen,
}

impl ReportRepository {
    pub fn repository(self) -> &'static str {
        match self {
            Self::Velocity => VELOCITY_REPOSITORY,
            Self::Rosen => ROSEN_REPOSITORY,
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
    "under", "own", "their", "same", "issue", "thread", "task", "tool",
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

/// What Velocity has reported from each project's discoveries, keyed by project identifier.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedDiscoveries {
    #[serde(default)]
    pub projects: BTreeMap<u32, ProjectReports>,
    #[serde(default)]
    pub recent: Vec<RecentDiscovery>,
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
    /// The discovery being reported, saved before GitHub is asked. Still set on the next pass
    /// means the last attempt may have reached GitHub without being recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<u64>,
}

impl ProjectReports {
    pub fn record_issue(&mut self, repository: ReportRepository, sequence: u64, issue: u32) {
        match repository {
            ReportRepository::Velocity => self.issues.insert(sequence, issue),
            ReportRepository::Rosen => self.rosen_issues.insert(sequence, issue),
        };
        self.cursor = sequence;
        self.pending = None;
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
        };
        issues.get(&sequence).copied()
    }
}

/// Files a discovery, or adds evidence to a matching open issue in its destination.
pub fn report_to(
    destination: ReportRepository,
    report: &DiscoveryReport,
    recent: &[RecentDiscovery],
    mentioned_paths: impl FnOnce(&[(&str, &str)]) -> Vec<BTreeSet<String>>,
    retry: bool,
) -> Result<ReportOutcome, GitHubCommandError> {
    let repository = destination.repository();
    let open = open_issues(repository)?;
    let body = issue_body(report);
    if retry && let Some(previous) = open.iter().find(|issue| issue.body == body) {
        return Ok(ReportOutcome::Filed(previous.number));
    }
    let candidates = matching_issues(destination, report, &open, recent, mentioned_paths);
    for (number, _) in candidates {
        if open.iter().any(|issue| issue.number == number) || issue_is_open(repository, number)? {
            return join_issue(repository, number, report, retry);
        }
    }
    file_issue(repository, report, &body).map(ReportOutcome::Filed)
}

/// Links the issues a discovery names on its `Blocked by:` lines to the issue it was
/// reported on, as GitHub issue dependencies (#511). A bare `#12` is an issue of the
/// destination repository. Links that exist are kept, so a retried report is safe.
/// Returns the blockers GitHub refused, such as an issue that does not exist: those are
/// final, while any other failure is an error the next pass retries.
pub fn link_declared_blockers(
    destination: ReportRepository,
    issue: u32,
    report: &DiscoveryReport,
) -> Result<Vec<String>, GitHubCommandError> {
    let repository = destination.repository();
    let blockers: Vec<_> = declared_blockers(&report.text)
        .into_iter()
        .filter(|blocker| !blocker.is_issue(repository, repository, issue))
        .collect();
    if blockers.is_empty() {
        return Ok(Vec::new());
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

fn join_issue(
    repository: &str,
    number: u32,
    report: &DiscoveryReport,
    retry: bool,
) -> Result<ReportOutcome, GitHubCommandError> {
    let endpoint = format!("repos/{repository}/issues/{number}/comments");
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
        let comments = run(&[
            "api",
            "--paginate",
            &endpoint,
            "--jq",
            ".[] | .body | @json",
        ])?;
        if comments.lines().any(|line| {
            serde_json::from_str::<String>(line)
                .is_ok_and(|comment| comment == body || comment == previous)
        }) {
            return Ok(ReportOutcome::Joined(number));
        }
    }
    run(&[
        "api",
        &endpoint,
        "-f",
        &format!("body={body}"),
        "--jq",
        ".id",
    ])?;
    Ok(ReportOutcome::Joined(number))
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

fn file_issue(
    repository: &str,
    report: &DiscoveryReport,
    body: &str,
) -> Result<u32, GitHubCommandError> {
    let number = run(&[
        "api",
        &format!("repos/{repository}/issues"),
        "-f",
        &format!("title={}", report.title),
        "-f",
        &format!("body={body}"),
        "--jq",
        ".number",
    ])?;
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
    let shortened = match text.char_indices().nth(QUOTED_TEXT_LIMIT) {
        Some((cut, _)) => format!(
            "{}
… (cut at {QUOTED_TEXT_LIMIT} characters)",
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
