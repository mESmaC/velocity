//! Velocity issues from agents' discoveries (F14 Discoveries): a discovery an agent marks as
//! being about Velocity itself is filed on Velocity's own repository. A report that matches an
//! open issue's problem is added to that issue as a comment instead of opening another.
//! Everything goes through GitHub's REST API, which keeps working when a swarm has spent the
//! GraphQL budget.

use std::collections::HashSet;

use crate::command::gh;
use crate::pull_request_command::PullRequestError;

/// Where Velocity's own issues live.
pub const VELOCITY_REPOSITORY: &str = "mESmaC/velocity";

/// Titles sharing at least this share of their significant words describe the same problem.
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

/// Files `report` on Velocity's repository, or adds it to the open issue about the same
/// problem.
pub fn report(report: &DiscoveryReport) -> Result<ReportOutcome, PullRequestError> {
    let open = open_issues()?;
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
    number
        .trim()
        .parse()
        .map(ReportOutcome::Filed)
        .map_err(|_| PullRequestError::Missing("the new issue's number".into()))
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

/// The open issue whose title shares at least half its significant words with `title`.
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
