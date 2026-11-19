//! What an agent is told about a workflow run it requested (#508): the conclusion, each
//! job's result, what failed and why, and the artifacts. The same facts are written twice:
//! in full to a file beside the downloaded artifacts, and cut to a prompt's size for the
//! agent's next message, with the parts it most needs first.

use std::fmt::Write;
use std::path::Path;

use velocity_core::text_budget::{prefix, preview};

use crate::workflow_artifacts::DownloadedArtifact;
use crate::workflow_runs::{Annotation, WorkflowJob, WorkflowRun};

pub(crate) struct RunReport<'a> {
    pub workflow: &'a str,
    pub reference: &'a str,
    pub repository: &'a str,
    pub run: &'a WorkflowRun,
    pub jobs: &'a [JobReport],
    pub artifacts: &'a [DownloadedArtifact],
    /// The output of the run's failed steps, and the file it was saved to.
    pub failed_log: Option<(&'a str, &'a Path)>,
    pub folder: &'a Path,
    /// Parts of the run that could not be read, and why.
    pub notes: &'a [String],
}

pub(crate) struct JobReport {
    pub job: WorkflowJob,
    pub annotations: Vec<Annotation>,
}

/// How much of each part a rendering shows.
pub(crate) struct Detail {
    jobs: usize,
    annotations: usize,
    log_lines: usize,
    line_bytes: usize,
    artifacts: usize,
    /// Bytes of one text file, and of all text files together.
    text_bytes: usize,
    texts_bytes: usize,
}

impl Detail {
    /// Fits the 8,000 bytes a queued result may add to a prompt in the usual cases.
    pub const PROMPT: Self = Self {
        jobs: 12,
        annotations: 6,
        log_lines: 12,
        line_bytes: 200,
        artifacts: 8,
        text_bytes: 1_500,
        texts_bytes: 3_000,
    };
    pub const FULL: Self = Self {
        jobs: usize::MAX,
        annotations: usize::MAX,
        log_lines: 200,
        line_bytes: 2_000,
        artifacts: usize::MAX,
        text_bytes: usize::MAX,
        texts_bytes: usize::MAX,
    };
}

impl RunReport<'_> {
    pub fn render(&self, detail: &Detail) -> String {
        let sections = [
            self.heading(),
            self.jobs(detail),
            self.annotations(detail),
            self.failed_log(detail),
            self.artifacts(detail),
            self.notes.join("\n"),
        ];
        let shown: Vec<String> = sections
            .into_iter()
            .filter(|section| !section.is_empty())
            .collect();
        shown.join("\n\n")
    }

    fn heading(&self) -> String {
        let run = self.run;
        let conclusion = run.conclusion.as_deref().unwrap_or("no conclusion");
        let mut heading = format!(
            "Workflow {} on {}: {conclusion} · {}\nRun #{}",
            self.workflow, self.reference, run.html_url, run.run_number
        );
        if let Some(name) = &run.name {
            let _ = write!(heading, " \"{name}\"");
        }
        if !run.head_sha.is_empty() {
            let _ = write!(heading, " at {}", prefix(&run.head_sha, 7));
        }
        if let Some(elapsed) = elapsed(run.run_started_at.as_deref(), run.updated_at.as_deref()) {
            let _ = write!(heading, ", {elapsed}");
        }
        let _ = write!(heading, ". Report and files: {}", self.folder.display());
        heading
    }

    fn jobs(&self, detail: &Detail) -> String {
        if self.jobs.is_empty() {
            return String::new();
        }
        // Jobs that did not succeed come first, so a clipped list still shows them.
        let mut ordered: Vec<&WorkflowJob> = self.jobs.iter().map(|report| &report.job).collect();
        ordered.sort_by_key(|job| conclusion_rank(job.conclusion.as_deref()));
        let mut section = format!("Jobs ({}):", ordered.len());
        for job in ordered.iter().take(detail.jobs) {
            let _ = write!(section, "\n- {}", job_line(job));
        }
        if ordered.len() > detail.jobs {
            let _ = write!(section, "\n- and {} more", ordered.len() - detail.jobs);
        }
        section
    }

    fn annotations(&self, detail: &Detail) -> String {
        let mut lines: Vec<(u8, String)> = self
            .jobs
            .iter()
            .flat_map(|report| {
                report
                    .annotations
                    .iter()
                    .map(|annotation| annotation_line(&report.job.name, annotation, detail))
            })
            .collect();
        if lines.is_empty() {
            return String::new();
        }
        lines.sort_by_key(|(rank, _)| *rank);
        let mut section = format!("Annotations ({}):", lines.len());
        for (_, line) in lines.iter().take(detail.annotations) {
            let _ = write!(section, "\n- {line}");
        }
        if lines.len() > detail.annotations {
            let _ = write!(section, "\n- and {} more", lines.len() - detail.annotations);
        }
        section
    }

    fn failed_log(&self, detail: &Detail) -> String {
        let Some((log, saved)) = self.failed_log else {
            return String::new();
        };
        let lines: Vec<&str> = log.lines().filter(|line| !line.trim().is_empty()).collect();
        if lines.is_empty() {
            return String::new();
        }
        let shown = lines.len().min(detail.log_lines);
        let mut section = format!(
            "Failed steps, last {shown} of {} lines (all of them: {}):",
            lines.len(),
            saved.display()
        );
        for line in &lines[lines.len() - shown..] {
            let _ = write!(section, "\n{}", preview(&log_line(line), detail.line_bytes));
        }
        section
    }

    fn artifacts(&self, detail: &Detail) -> String {
        if self.artifacts.is_empty() {
            return String::new();
        }
        let mut section = format!("Artifacts ({}):", self.artifacts.len());
        let mut texts_left = detail.texts_bytes;
        for downloaded in self.artifacts.iter().take(detail.artifacts) {
            let artifact = &downloaded.artifact;
            let _ = write!(
                section,
                "\n- {} · {} · https://github.com/{}/actions/runs/{}/artifacts/{}",
                artifact.name,
                size(artifact.size_in_bytes),
                self.repository,
                self.run.id,
                artifact.id
            );
            match &downloaded.files {
                Ok(files) => {
                    let _ = write!(section, " · files: {}", files.display());
                }
                Err(reason) => {
                    let _ = write!(section, " · {reason}");
                }
            }
            for text in &downloaded.texts {
                let budget = detail.text_bytes.min(texts_left);
                if budget == 0 || text.text.is_empty() {
                    continue;
                }
                let shown = preview(&text.text, budget);
                texts_left = texts_left.saturating_sub(shown.len());
                let _ = write!(section, "\n  {}:\n{}", text.name, indented(&shown));
            }
        }
        if self.artifacts.len() > detail.artifacts {
            let more = self.artifacts.len() - detail.artifacts;
            let _ = write!(section, "\n- and {more} more");
        }
        section
    }
}

/// Failures first, then what was neither a success nor skipped, then the rest.
fn conclusion_rank(conclusion: Option<&str>) -> u8 {
    match conclusion {
        Some("failure" | "timed_out" | "startup_failure") => 0,
        Some("success") => 2,
        Some("skipped") => 3,
        _ => 1,
    }
}

fn job_line(job: &WorkflowJob) -> String {
    let conclusion = job.conclusion.as_deref().unwrap_or("no conclusion");
    let mut line = format!("{conclusion} · {}", job.name);
    if let Some(elapsed) = elapsed(job.started_at.as_deref(), job.completed_at.as_deref()) {
        let _ = write!(line, " · {elapsed}");
    }
    if conclusion_rank(job.conclusion.as_deref()) < 2 {
        let failed: Vec<&str> = job
            .steps
            .iter()
            .filter(|step| conclusion_rank(step.conclusion.as_deref()) == 0)
            .map(|step| step.name.as_str())
            .collect();
        if !failed.is_empty() {
            let _ = write!(line, " · failed at: {}", failed.join(", "));
        }
        if let Some(url) = &job.html_url {
            let _ = write!(line, " · {url}");
        }
    }
    line
}

/// One annotation as a line, with a rank that puts failures before warnings and notices.
fn annotation_line(job: &str, annotation: &Annotation, detail: &Detail) -> (u8, String) {
    let level = annotation.annotation_level.as_deref().unwrap_or("notice");
    let rank = match level {
        "failure" => 0,
        "warning" => 1,
        _ => 2,
    };
    let mut line = format!("{level} · {job}");
    if let Some(path) = annotation.path.as_deref().filter(|path| !path.is_empty()) {
        let _ = write!(line, " · {path}");
        if let Some(number) = annotation.start_line {
            let _ = write!(line, ":{number}");
        }
    }
    let message = annotation.message.as_deref().unwrap_or_default();
    let message = message.split_whitespace().collect::<Vec<_>>().join(" ");
    let _ = write!(line, " · {}", preview(&message, detail.line_bytes));
    (rank, line)
}

/// `gh run view --log-failed` prints `job<TAB>step<TAB>timestamp message`. The job and
/// the timestamp are dropped: the jobs are listed above, and the order is the file's.
fn log_line(line: &str) -> String {
    let mut fields = line.splitn(3, '\t');
    let (Some(_job), Some(step), Some(output)) = (fields.next(), fields.next(), fields.next())
    else {
        return line.to_owned();
    };
    let message = match output.split_once(' ') {
        Some((stamp, message)) if stamp.ends_with('Z') && stamp.contains('T') => message,
        _ => output,
    };
    format!("{step}: {message}")
}

fn indented(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn size(bytes: u64) -> String {
    match bytes {
        0..1_024 => format!("{bytes} B"),
        1_024..1_048_576 => format!("{} KB", bytes / 1_024),
        _ => format!("{} MB", bytes / 1_048_576),
    }
}

/// The time between two of GitHub's timestamps, such as `8m 33s`.
fn elapsed(started: Option<&str>, finished: Option<&str>) -> Option<String> {
    let started = started?.parse::<jiff::Timestamp>().ok()?;
    let finished = finished?.parse::<jiff::Timestamp>().ok()?;
    let seconds = finished.duration_since(started).as_secs();
    if seconds < 0 {
        return None;
    }
    let (hours, minutes, seconds) = (seconds / 3_600, seconds % 3_600 / 60, seconds % 60);
    Some(if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    })
}
