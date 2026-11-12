//! The graph's per-project switch and its measurement (F19 "Measuring it"): every agent turn
//! adds its tokens and working time to the project's totals for the graph on or off, and the
//! usage modal compares the two sides.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Error;

/// Turns each side needs before the comparison draws a conclusion: fewer, and one long turn
/// decides it.
pub const MINIMUM_TURNS: u64 = 10;

/// What one turn cost, as the agent reported it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TurnUsage {
    /// `None` when the agent reports no token counts.
    pub tokens: Option<TokenCounts>,
    pub working_milliseconds: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    pub cached_read: u64,
    pub cached_write: u64,
    pub total: u64,
}

impl TokenCounts {
    /// What a turn added, from the agent's running totals before and after it. A total that
    /// went down means the agent started counting again (a new session).
    pub fn since(self, before: TokenCounts) -> TokenCounts {
        if self.total < before.total {
            return self;
        }
        TokenCounts {
            input: self.input.saturating_sub(before.input),
            output: self.output.saturating_sub(before.output),
            cached_read: self.cached_read.saturating_sub(before.cached_read),
            cached_write: self.cached_write.saturating_sub(before.cached_write),
            total: self.total - before.total,
        }
    }
}

/// One side's totals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    pub turns: u64,
    /// Turns whose agent reported tokens: token averages divide by this, not `turns`.
    pub turns_with_tokens: u64,
    pub tokens: TokenCounts,
    pub working_milliseconds: u64,
}

impl Totals {
    fn add(&mut self, turn: TurnUsage) {
        self.turns += 1;
        self.working_milliseconds += turn.working_milliseconds;
        if let Some(tokens) = turn.tokens {
            self.turns_with_tokens += 1;
            self.tokens.input += tokens.input;
            self.tokens.output += tokens.output;
            self.tokens.cached_read += tokens.cached_read;
            self.tokens.cached_write += tokens.cached_write;
            self.tokens.total += tokens.total;
        }
    }

    fn tokens_per_turn(&self) -> Option<f64> {
        (self.turns_with_tokens > 0)
            .then(|| self.tokens.total as f64 / self.turns_with_tokens as f64)
    }

    fn seconds_per_turn(&self) -> Option<f64> {
        (self.turns > 0).then(|| self.working_milliseconds as f64 / 1000.0 / self.turns as f64)
    }
}

/// A project's saved switch and totals.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectUsage {
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub on: Totals,
    #[serde(default)]
    pub off: Totals,
    /// When the totals started (RFC 3339): first use or the last reset.
    #[serde(default)]
    pub since: String,
}

fn enabled_by_default() -> bool {
    true
}

impl Default for ProjectUsage {
    fn default() -> Self {
        Self {
            enabled: true,
            on: Totals::default(),
            off: Totals::default(),
            since: String::new(),
        }
    }
}

fn folder(app_data: &Path, project: &str) -> PathBuf {
    app_data.join("graph").join(project)
}

/// The project's saved switch and totals; a missing or unreadable file is the defaults (the
/// graph on, nothing counted).
pub fn read(app_data: &Path, project: &str) -> ProjectUsage {
    std::fs::read(folder(app_data, project).join("usage.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Whether sessions in `project` get the graph (the switch; on by default).
pub fn enabled(app_data: &Path, project: &str) -> bool {
    read(app_data, project).enabled
}

pub fn set_enabled(app_data: &Path, project: &str, enabled: bool) -> Result<ProjectUsage, Error> {
    update(app_data, project, |usage| usage.enabled = enabled)
}

/// Adds one turn to the side it ran on.
pub fn record(
    app_data: &Path,
    project: &str,
    graph_on: bool,
    turn: TurnUsage,
) -> Result<(), Error> {
    update(app_data, project, |usage| {
        if usage.since.is_empty() {
            usage.since = now();
        }
        if graph_on {
            usage.on.add(turn);
        } else {
            usage.off.add(turn);
        }
    })
    .map(drop)
}

/// Clears both sides' totals and starts counting again now. The switch stays as it is.
pub fn reset(app_data: &Path, project: &str) -> Result<ProjectUsage, Error> {
    update(app_data, project, |usage| {
        usage.on = Totals::default();
        usage.off = Totals::default();
        usage.since = now();
    })
}

fn now() -> String {
    chrono::Local::now().to_rfc3339()
}

/// "Counting since 12 Nov 14:05" in local time, or the first turn's wording before any.
fn since_text(since: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(since) {
        Ok(time) => format!(
            "Counting since {}",
            time.with_timezone(&chrono::Local).format("%-d %b %H:%M")
        ),
        Err(_) => "Counting starts with the next turn".to_owned(),
    }
}

/// Read, change and write under the project's usage lock: agents finish turns at the same
/// time, and two unlocked writes would lose one of them.
fn update(
    app_data: &Path,
    project: &str,
    change: impl FnOnce(&mut ProjectUsage),
) -> Result<ProjectUsage, Error> {
    let folder = folder(app_data, project);
    std::fs::create_dir_all(&folder)?;
    let lock = lock(&folder)?;
    let mut usage = read(app_data, project);
    change(&mut usage);
    let temporary = folder.join("usage.json.tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(&usage)?)?;
    std::fs::rename(&temporary, folder.join("usage.json"))?;
    drop(lock);
    Ok(usage)
}

fn lock(folder: &Path) -> Result<File, Error> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(folder.join("usage.lock"))?;
    lock.lock()?;
    Ok(lock)
}

/// The usage modal's code graph section, worked out in Rust (AGENTS.md: no logic in the UI).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct GraphComparison {
    pub enabled: bool,
    /// "Counting since 12 Nov 14:05", or "Counting since the first turn".
    pub since: String,
    pub sides: Vec<ComparisonSide>,
    /// The conclusion, or what's still missing before there can be one.
    pub verdict: String,
    /// Whether the verdict compares the two sides (both have enough turns).
    pub conclusive: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct ComparisonSide {
    /// "Graph on" or "Graph off".
    pub label: String,
    /// "42".
    pub turns: String,
    /// Average tokens per turn: "61.2k", or "–" before any report.
    pub tokens_per_turn: String,
    /// Average working time per turn: "48 s".
    pub time_per_turn: String,
    pub total_tokens: String,
    pub total_time: String,
    /// "Tokens reported for 40 of 42 turns", when some turns came without counts.
    pub note: String,
}

impl GraphComparison {
    pub fn from_usage(usage: &ProjectUsage) -> Self {
        let sides = vec![side("Graph on", &usage.on), side("Graph off", &usage.off)];
        let (verdict, conclusive) = verdict(&usage.on, &usage.off);
        Self {
            enabled: usage.enabled,
            since: since_text(&usage.since),
            sides,
            verdict,
            conclusive,
        }
    }
}

fn side(label: &str, totals: &Totals) -> ComparisonSide {
    let note = if totals.turns_with_tokens < totals.turns {
        format!(
            "Tokens reported for {} of {} turns",
            totals.turns_with_tokens, totals.turns
        )
    } else {
        String::new()
    };
    ComparisonSide {
        label: label.to_owned(),
        turns: totals.turns.to_string(),
        tokens_per_turn: totals
            .tokens_per_turn()
            .map_or("–".into(), |tokens| count(tokens as u64)),
        time_per_turn: totals.seconds_per_turn().map_or("–".into(), duration),
        total_tokens: count(totals.tokens.total),
        total_time: duration(totals.working_milliseconds as f64 / 1000.0),
        note,
    }
}

/// "With the graph, a turn used 23% fewer tokens and took 18% less time", once each side has
/// [`MINIMUM_TURNS`].
fn verdict(on: &Totals, off: &Totals) -> (String, bool) {
    if on.turns < MINIMUM_TURNS || off.turns < MINIMUM_TURNS {
        return (
            format!(
                "A comparison needs at least {MINIMUM_TURNS} turns on each side: {} with the graph and {} without so far. Run some work with the graph on and some with it off.",
                on.turns, off.turns
            ),
            false,
        );
    }
    let time = change(
        on.seconds_per_turn(),
        off.seconds_per_turn(),
        "less time",
        "more time",
    );
    let tokens = change(
        on.tokens_per_turn(),
        off.tokens_per_turn(),
        "fewer tokens",
        "more tokens",
    );
    let parts: Vec<String> = [
        tokens.map(|text| format!("used {text}")),
        time.map(|text| format!("took {text}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        return ("No agent reported tokens or time yet.".into(), false);
    }
    (
        format!("With the graph, a turn {}.", parts.join(" and ")),
        true,
    )
}

/// "23% fewer tokens" for `with` against `without`; `None` when either is unknown.
fn change(with: Option<f64>, without: Option<f64>, less: &str, more: &str) -> Option<String> {
    let (with, without) = (with?, without?);
    if without <= 0.0 {
        return None;
    }
    let percent = ((without - with) / without * 100.0).round();
    Some(if percent >= 0.0 {
        format!("{percent}% {less}")
    } else {
        format!("{}% {more}", -percent)
    })
}

/// `950`, `61.2k`, `2.6M`.
fn count(value: u64) -> String {
    match value {
        0..=999 => value.to_string(),
        1_000..=999_999 => format!("{:.1}k", value as f64 / 1_000.0),
        _ => format!("{:.1}M", value as f64 / 1_000_000.0),
    }
}

/// `48 s`, `12 min`, `3.4 h`.
fn duration(seconds: f64) -> String {
    if seconds < 90.0 {
        format!("{} s", seconds.round())
    } else if seconds < 90.0 * 60.0 {
        format!("{} min", (seconds / 60.0).round())
    } else {
        format!("{:.1} h", seconds / 3600.0)
    }
}
