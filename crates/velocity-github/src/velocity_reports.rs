//! Velocity issues from agents' discoveries (F14 Discoveries): a discovery an agent marks as
//! being about Velocity itself is filed on Velocity's own repository. A report that matches an
//! open issue's problem is added to that issue as a comment instead of opening another.
//! Everything goes through GitHub's REST API, which keeps working when a swarm has spent the
//! GraphQL budget.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::command::gh;
use crate::pull_request_command::PullRequestError;

/// Where Velocity's own issues live.
pub const VELOCITY_REPOSITORY: &str = "mESmaC/velocity";

/// The app data file that records what Velocity has reported.
pub const REPORTS_FILE: &str = "velocity-reports.json";

/// Titles whose significant words overlap at least this much (shared words over all words in
/// either title) describe the same problem.
const SAME_PROBLEM: f64 = 0.5;

const STOP_WORDS: [&str; 22] = [
    "the", "and", "for", "with", "that", "this", "from", "into", "when", "are", "but", "not",
    "its", "has", "can", "after", "before", "every", "all", "any", "some", "velocity",
];

/// One discovery to report.
#[derive(Clone, Debug, PartialEq, Eq)]
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
}

impl ReportedDiscoveries {
    /// Reads the saved record. A missing file means nothing has been reported yet; a damaged
    /// one is an error, so reporting stops rather than filing every discovery again.
    pub fn load(app_data: &Path) -> Result<Self, PullRequestError> {
        match std::fs::read(app_data.join(REPORTS_FILE)) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    /// Writes the record through a temporary file, so a crash mid-write keeps the old one.
    pub fn save(&self, app_data: &Path) -> Result<(), PullRequestError> {
        std::fs::create_dir_all(app_data)?;
        let mut file = tempfile::NamedTempFile::new_in(app_data)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.as_file().sync_all()?;
        file.persist(app_data.join(REPORTS_FILE))
            .map_err(|error| PullRequestError::Process(error.error))?;
        Ok(())
    }

    /// The issue a project's discovery was reported on, if it has been.
    pub fn issue(&self, project: u32, sequence: u64) -> Option<u32> {
        self.projects.get(&project)?.issues.get(&sequence).copied()
    }
}

/// Files `report` on Velocity's repository, or adds it to the open issue about the same
/// problem. `filed` holds the issues the caller filed moments ago: GitHub's issue list lags
/// a few seconds behind a new issue, so two reports of one problem in quick succession would
/// otherwise open two issues. A newly filed issue is added to it.
pub fn report(
    report: &DiscoveryReport,
    filed: &mut Vec<(u32, String)>,
) -> Result<ReportOutcome, PullRequestError> {
    let mut open = open_issues()?;
    for issue in filed.iter() {
        if !open.iter().any(|(number, _)| *number == issue.0) {
            open.push(issue.clone());
        }
    }
    if let Some(number) = same_problem(&report.title, &open) {
        let body = format!(
            "Seen again by `{}` in project `{}` ({}):\n\n{}",
            report.nick,
            report.project,
            report.time,
            quote(&report.text)
        );
        run(&[
            "api",
            &format!("repos/{VELOCITY_REPOSITORY}/issues/{number}/comments"),
            "-f",
            &format!("body={body}"),
            "--jq",
            ".id",
        ])?;
        return Ok(ReportOutcome::Joined(number));
    }
    let body = format!(
        "Reported by an agent from an AIRC discovery.\n\n- **Agent:** `{}`\n- **Project:** `{}`\n- **Recorded:** {}\n\n{}\n",
        report.nick,
        report.project,
        report.time,
        quote(&report.text)
    );
    let number = run(&[
        "api",
        &format!("repos/{VELOCITY_REPOSITORY}/issues"),
        "-f",
        &format!("title={}", report.title),
        "-f",
        &format!("body={body}"),
        "--jq",
        ".number",
    ])?;
    let number: u32 = number
        .trim()
        .parse()
        .map_err(|_| PullRequestError::Missing("the new issue's number".into()))?;
    filed.push((number, report.title.clone()));
    Ok(ReportOutcome::Filed(number))
}

/// Velocity's open issues (not pull requests), as number and title.
fn open_issues() -> Result<Vec<(u32, String)>, PullRequestError> {
    let output = run(&[
        "api",
        "--paginate",
        &format!("repos/{VELOCITY_REPOSITORY}/issues?state=open&per_page=100"),
        "--jq",
        ".[] | select(.pull_request == null) | \"\\(.number)\\t\\(.title)\"",
    ])?;
    Ok(output
        .lines()
        .filter_map(|line| {
            let (number, title) = line.split_once('\t')?;
            Some((number.parse().ok()?, title.to_owned()))
        })
        .collect())
}

/// The open issue whose title overlaps `title` by at least half of their significant words.
pub fn same_problem(title: &str, open: &[(u32, String)]) -> Option<u32> {
    let wanted = significant_words(title);
    if wanted.len() < 2 {
        return None;
    }
    open.iter()
        .map(|(number, other)| {
            let other = significant_words(other);
            let shared = wanted.intersection(&other).count() as f64;
            let union = wanted.union(&other).count().max(1) as f64;
            (*number, shared / union)
        })
        .filter(|(_, overlap)| *overlap >= SAME_PROBLEM)
        .max_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(number, _)| number)
}

/// Lower-cased words of three or more letters, without common words, with a plural `s`
/// dropped so "paths" and "path" match.
fn significant_words(text: &str) -> HashSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| word.len() >= 3 && !STOP_WORDS.contains(&word.as_str()))
        .map(|word| match word.strip_suffix('s') {
            Some(stem) if stem.len() >= 3 && !word.ends_with("ss") => stem.to_owned(),
            _ => word,
        })
        .collect()
}

fn quote(text: &str) -> String {
    text.lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn run(arguments: &[&str]) -> Result<String, PullRequestError> {
    let output = gh(arguments).output()?;
    if !output.status.success() {
        return Err(PullRequestError::Command {
            code: output.status.code(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
