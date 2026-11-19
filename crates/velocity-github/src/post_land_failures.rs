//! Confirmed post-land test failures become issues, never approval or landing gates (#380).
//! Stable markers make a retry after an interrupted GitHub request idempotent.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::gh_command::GitHubCommand;
use crate::{GitHubCommandError, RepositoryName};

const LABEL: &str = "post-land-failure";

/// One confirmed failure waiting to reach the project's issue tracker.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FailureReport {
    pub session: i64,
    pub repository: String,
    pub change: String,
    pub commit: String,
    pub thread: Option<u32>,
    pub thread_title: String,
    /// Command, exit status and the end of its output, as retained by Rosen.
    pub output: String,
}

impl FailureReport {
    pub fn new(
        session: i64,
        repository: String,
        change: String,
        commit: String,
        output: String,
    ) -> Self {
        Self {
            session,
            repository,
            change,
            commit,
            output,
            thread: None,
            thread_title: String::new(),
        }
    }

    pub fn with_thread(mut self, identifier: u32, title: String) -> Self {
        self.thread = Some(identifier);
        self.thread_title = title;
        self
    }

    fn body(&self, failure: &str, occurrence: &str) -> String {
        let thread = self.thread.map_or_else(
            || "No saved thread".to_owned(),
            |identifier| format!("{identifier} · {}", self.thread_title),
        );
        // Quote every output line: commands may print Markdown fences or marker-like text.
        let output = self
            .output
            .lines()
            .map(|line| format!("> {line}\n"))
            .collect::<String>();
        format!(
            "Post-land tests failed. This issue does not block landing.\n\nCulprit change: `{}`\nCulprit commit: `{}`\nVerification session: `{}`\nThread: {thread}\n\n{output}\n{failure}\n{occurrence}",
            self.change, self.commit, self.session
        )
    }
}

/// Durable outbox. Verification can continue while GitHub is unavailable, and a restart
/// retries unfinished reports. A damaged outbox is an error, never silently replaced.
#[derive(Default, Serialize, Deserialize)]
pub struct PendingReports {
    #[serde(default)]
    reports: Vec<FailureReport>,
    #[serde(default)]
    acknowledged: BTreeSet<i64>,
}

impl PendingReports {
    pub fn load(path: &Path) -> Result<Self, GitHubCommandError> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn enqueue(&mut self, report: FailureReport) {
        if !self.contains(report.session) {
            self.reports.push(report);
        }
    }

    pub fn contains(&self, session: i64) -> bool {
        self.acknowledged.contains(&session)
            || self.reports.iter().any(|report| report.session == session)
    }

    pub fn first(&self) -> Option<&FailureReport> {
        self.reports.first()
    }

    pub fn acknowledge(&mut self) {
        if !self.reports.is_empty() {
            self.acknowledged.insert(self.reports.remove(0).session);
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), GitHubCommandError> {
        let directory = path
            .parent()
            .ok_or_else(|| GitHubCommandError::Invalid("Outbox has no parent".into()))?;
        std::fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(&serde_json::to_vec(self)?)?;
        file.as_file().sync_all()?;
        file.persist(path)
            .map_err(|error| GitHubCommandError::Process(error.error))?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct Issue {
    number: u32,
    body: Option<String>,
}

#[derive(Deserialize)]
struct Comment {
    body: String,
}

/// File one issue per culprit and command/status. Diagnostic output can contain timings
/// and paths that vary between runs; repeated evidence belongs on that same open issue.
pub fn publish(report: &FailureReport) -> Result<u32, GitHubCommandError> {
    let repository = RepositoryName::parse(&report.repository)
        .map_err(|error| GitHubCommandError::Invalid(error.to_string()))?;
    let command = GitHubCommand::for_repository(&repository);
    let _lock = report_lock(&repository.full_name())?;
    let endpoint = format!("repos/{}", repository.full_name());
    let failure = failure_marker(report);
    let occurrence = marker(
        "occurrence",
        &format!("{}:{}:{}", report.session, report.change, report.commit),
    );
    let issues: Vec<Vec<Issue>> = command.read(&[
        "api",
        "--paginate",
        "--slurp",
        &format!("{endpoint}/issues?state=open&labels={LABEL}&per_page=100"),
    ])?;
    let body = report.body(&failure, &occurrence);
    if let Some(issue) = issues.into_iter().flatten().find(|issue| {
        issue
            .body
            .as_ref()
            .is_some_and(|body| body.lines().any(|line| line == failure))
    }) {
        comment_once(&command, &endpoint, &issue, &body, &occurrence)?;
        return Ok(issue.number);
    }
    ensure_label(&command, &endpoint)?;
    let title: String = report
        .output
        .lines()
        .next()
        .unwrap_or("Test command failed")
        .chars()
        .take(180)
        .collect();
    let issue: Issue = command.api(
        "POST",
        &format!("{endpoint}/issues"),
        &json!({"title":format!("Post-land test failure: {title}"),"body":body,"labels":[LABEL]}),
    )?;
    Ok(issue.number)
}

/// Two open projects may mirror the same repository. Serialise their read/create cycle
/// on this machine so simultaneous confirmations still create only one issue.
fn report_lock(repository: &str) -> Result<std::fs::File, GitHubCommandError> {
    let directory = std::env::temp_dir().join("velocity-post-land-report-locks");
    std::fs::create_dir_all(&directory)?;
    let name = repository.replace('/', "--").to_ascii_lowercase();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(name))?;
    file.lock()?;
    Ok(file)
}

fn failure_marker(report: &FailureReport) -> String {
    // failed_check_report separates its command/status header from output with a blank
    // line. Keep the whole header, including a command that spans several lines.
    let header = report
        .output
        .split_once("\n\n")
        .map_or(report.output.as_str(), |(header, _)| header);
    marker("failure", &format!("{}\n{header}", report.change))
}

fn comment_once(
    command: &GitHubCommand,
    endpoint: &str,
    issue: &Issue,
    body: &str,
    occurrence: &str,
) -> Result<(), GitHubCommandError> {
    if issue
        .body
        .as_ref()
        .is_some_and(|body| body.lines().any(|line| line == occurrence))
    {
        return Ok(());
    }
    let endpoint = format!("{endpoint}/issues/{}/comments", issue.number);
    let comments: Vec<Vec<Comment>> = command.read(&[
        "api",
        "--paginate",
        "--slurp",
        &format!("{endpoint}?per_page=100"),
    ])?;
    if !comments
        .into_iter()
        .flatten()
        .any(|comment| comment.body.lines().any(|line| line == occurrence))
    {
        let _: Value = command.api("POST", &endpoint, &json!({"body":body}))?;
    }
    Ok(())
}

fn ensure_label(command: &GitHubCommand, endpoint: &str) -> Result<(), GitHubCommandError> {
    #[derive(Deserialize)]
    struct Label {
        name: String,
    }
    let labels: Vec<Vec<Label>> = command.read(&[
        "api",
        "--paginate",
        "--slurp",
        &format!("{endpoint}/labels?per_page=100"),
    ])?;
    if !labels
        .into_iter()
        .flatten()
        .any(|label| label.name == LABEL)
    {
        let _: Value = command.api("POST", &format!("{endpoint}/labels"), &json!({"name":LABEL,"color":"d73a4a","description":"A confirmed post-land test failure; does not block landing"}))?;
    }
    Ok(())
}

fn marker(kind: &str, text: &str) -> String {
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("<!-- velocity-post-land-{kind}:{hash:016x} -->")
}
