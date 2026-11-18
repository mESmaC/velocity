use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use chrono::{Days, Local, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::Error;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Role {
    Implementation,
    Review,
    Duty,
    Title,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Input {
    BasePrompt,
    ProjectInstructions,
    CodeReads,
    GraphCalls,
    Searches,
    Diffs,
    BuildOutput,
    Polls,
    McpTools,
    Conversation,
    Unattributed,
}

impl Input {
    fn label(self) -> &'static str {
        match self {
            Self::BasePrompt => "Base prompt",
            Self::ProjectInstructions => "Project instructions",
            Self::CodeReads => "Code reads",
            Self::GraphCalls => "Graph calls",
            Self::Searches => "Searches",
            Self::Diffs => "Diffs",
            Self::BuildOutput => "Build output",
            Self::Polls => "Polls",
            Self::McpTools => "MCP tools",
            Self::Conversation => "Conversation",
            Self::Unattributed => "Unattributed",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Subject {
    pub role: Role,
    pub thread: Option<u32>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Counts {
    /// Includes cached input.
    pub input: u64,
    pub cached: u64,
    pub output: u64,
    pub sources: BTreeMap<Input, u64>,
}

impl Counts {
    fn add(&mut self, other: &Self) {
        self.input = self.input.saturating_add(other.input);
        self.cached = self.cached.saturating_add(other.cached);
        self.output = self.output.saturating_add(other.output);
        for (&source, &tokens) in &other.sources {
            let total = self.sources.entry(source).or_default();
            *total = total.saturating_add(tokens);
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Call {
    pub timestamp: i64,
    pub counts: Counts,
}

#[derive(Deserialize, Serialize)]
struct Entry {
    subject: Subject,
    calls: Vec<Call>,
}

/// Store numbers and identifiers only. A day's lock serializes concurrent appends.
pub fn record(data: &Path, project: u32, subject: Subject, calls: Vec<Call>) -> Result<(), Error> {
    let folder = project_folder(data, project);
    std::fs::create_dir_all(&folder)?;
    let mut days = BTreeMap::<NaiveDate, Vec<Call>>::new();
    for call in calls {
        if let Some(day) = chrono::DateTime::from_timestamp(call.timestamp, 0) {
            days.entry(day.with_timezone(&Local).date_naive())
                .or_default()
                .push(call);
        }
    }
    for (day, calls) in days {
        append(&folder, day, Entry { subject, calls })?;
    }
    Ok(())
}

fn project_folder(data: &Path, project: u32) -> std::path::PathBuf {
    data.join("token-usage").join(project.to_string())
}

fn append(folder: &Path, day: NaiveDate, entry: Entry) -> Result<(), Error> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(folder.join(format!("{day}.jsonl")))?;
    file.lock()?;
    let mut line = serde_json::to_vec(&entry)?;
    // Start at a fresh boundary even if the last write was interrupted.
    line.insert(0, b'\n');
    (&file).write_all(&line)?;
    Ok(())
}

#[derive(Clone, Debug, Default, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct Report {
    pub groups: Vec<UsageGroup>,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct UsageGroup {
    pub name: String,
    pub rows: Vec<UsageRow>,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct UsageRow {
    pub key: String,
    pub label: String,
    pub detail: String,
    pub input: String,
    pub cached: String,
    pub output: String,
    pub total: String,
    pub calls: String,
    pub sources: Vec<SourceRow>,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct SourceRow {
    pub label: String,
    pub tokens: String,
}

#[derive(Default)]
struct Totals {
    calls: u64,
    counts: Counts,
}

impl Totals {
    fn add(&mut self, call: &Call) {
        self.calls += 1;
        self.counts.add(&call.counts);
    }

    fn row(self, key: String, label: String, detail: String) -> UsageRow {
        let mut sources: Vec<_> = self.counts.sources.into_iter().collect();
        sources.sort_by_key(|&(_, tokens)| std::cmp::Reverse(tokens));
        UsageRow {
            key,
            label,
            detail,
            input: self.counts.input.to_string(),
            cached: self.counts.cached.to_string(),
            output: self.counts.output.to_string(),
            total: self
                .counts
                .input
                .saturating_add(self.counts.output)
                .to_string(),
            calls: self.calls.to_string(),
            sources: sources
                .into_iter()
                .map(|(source, tokens)| SourceRow {
                    label: source.label().to_owned(),
                    tokens: tokens.to_string(),
                })
                .collect(),
        }
    }
}

type Groups<Key> = BTreeMap<Key, Totals>;

pub struct Project {
    pub id: u32,
    pub name: String,
    pub threads: BTreeMap<u32, String>,
}

#[derive(Clone, Copy, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub enum Period {
    Today,
    Week,
}

impl Period {
    fn start(self) -> chrono::NaiveDate {
        let today = Local::now().date_naive();
        match self {
            Self::Today => today,
            Self::Week => today.checked_sub_days(Days::new(6)).unwrap_or(today),
        }
    }
}

#[derive(Default)]
struct Summary {
    roles: Groups<Role>,
    projects: Groups<u32>,
    threads: Groups<(u32, u32)>,
}

pub fn report(
    data: &Path,
    projects: BTreeMap<u32, Project>,
    period: Period,
    thread: Option<(u32, u32)>,
) -> Result<Report, Error> {
    let start = period.start();
    let end = Local::now().date_naive();
    let mut summary = Summary::default();
    for project in projects.values() {
        let folder = project_folder(data, project.id);
        let files = match std::fs::read_dir(folder) {
            Ok(files) => files,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for file in files {
            let path = file?.path();
            let day = path
                .file_stem()
                .and_then(|name| name.to_str())
                .and_then(|name| name.parse::<NaiveDate>().ok());
            if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
                && day.is_some_and(|day| thread.is_some() || (start..=end).contains(&day))
            {
                summary.read(project.id, &path, thread)?;
            }
        }
    }
    Ok(summary.report(&projects, thread.is_some()))
}

impl Summary {
    fn read(&mut self, project: u32, path: &Path, thread: Option<(u32, u32)>) -> Result<(), Error> {
        let file = File::open(path)?;
        file.lock_shared()?;
        for line in BufReader::new(file).lines() {
            if let Ok(entry) = serde_json::from_str::<Entry>(&line?) {
                self.observe(project, entry, thread);
            }
        }
        Ok(())
    }

    fn observe(&mut self, project: u32, entry: Entry, thread: Option<(u32, u32)>) {
        let key = entry.subject.thread.map(|thread| (project, thread));
        if thread.is_some() && thread != key {
            return;
        }
        for call in &entry.calls {
            self.roles.entry(entry.subject.role).or_default().add(call);
            self.projects.entry(project).or_default().add(call);
            if let Some(key) = key {
                self.threads.entry(key).or_default().add(call);
            }
        }
    }

    fn report(self, projects: &BTreeMap<u32, Project>, thread_view: bool) -> Report {
        let roles = group("Agent roles", self.roles, |role| {
            (format!("{role:?}"), String::new())
        });
        let threads = group("Threads", self.threads, |(project, thread)| {
            let project = &projects[&project];
            let name = project
                .threads
                .get(&thread)
                .cloned()
                .unwrap_or_else(|| format!("Thread #{thread}"));
            (
                name,
                if thread_view {
                    String::new()
                } else {
                    project.name.clone()
                },
            )
        });
        let mut groups = vec![threads, roles];
        if !thread_view {
            groups.push(group("Projects", self.projects, |id| {
                (projects[&id].name.clone(), String::new())
            }));
        }
        Report { groups }
    }
}

fn group<Key: std::fmt::Debug>(
    name: &str,
    totals: Groups<Key>,
    label: impl Fn(Key) -> (String, String),
) -> UsageGroup {
    let mut totals: Vec<_> = totals.into_iter().collect();
    totals.sort_by_key(|(_, totals)| {
        std::cmp::Reverse(totals.counts.input.saturating_add(totals.counts.output))
    });
    UsageGroup {
        name: name.to_owned(),
        rows: totals
            .into_iter()
            .map(|(key, total)| {
                let id = format!("{key:?}");
                let (name, detail) = label(key);
                total.row(id, name, detail)
            })
            .collect(),
    }
}
/// A provider log's timestamp in milliseconds since the Unix epoch.
pub fn timestamp_millis(timestamp: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|time| time.timestamp_millis())
}
