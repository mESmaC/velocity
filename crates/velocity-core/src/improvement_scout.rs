//! Improvement scout (SPEC.md §1, F7 Development speed, #622): pure scheduling,
//! one-at-a-time gating, signal digest formatting, `velocity.log` signal extraction,
//! and `Target metric:` validation for fleet self-improvement runs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::performance::BREACH_LOG_SOURCE;
use crate::text_budget::{preview, prompt_excerpt};

/// Issue label applied to every self-improvement issue and prioritized by dispatch (#622).
pub const SELF_IMPROVEMENT_LABEL: &str = "self-improvement";

/// Slots free with nothing dispatchable for this long triggers one improvement scout run.
pub const IDLE_TRIGGER_SECONDS: u64 = 15 * 60;

/// Lands accumulated since the last run that trigger a new improvement scout run.
pub const LANDS_TRIGGER_COUNT: u64 = 25;

/// Maximum issues an improvement scout may file in a single run.
pub const MAX_ISSUES_PER_RUN: usize = 3;

/// Scout identity stamped on issues filed by the machine-wide improvement scout.
pub const IMPROVEMENT_SCOUT_NICK: &str = "improvement-scout";

/// Byte cap on the compact digest handed to the improvement scout so each run stays cheap.
pub const DIGEST_BUDGET_BYTES: usize = 16_000;

const MAX_LINES_PER_LOG_CATEGORY: usize = 12;
const MAX_LINE_CHARS: usize = 220;
const MAX_TARGET_METRIC_CHARS: usize = 200;

const DIRECTION_WORDS: &[&str] = &[
    "down", "up", "lower", "higher", "fewer", "more", "faster", "shorter", "decrease", "increase",
    "reduce",
];

/// Tool-suite repositories targeted by the Development speed focus area (#622).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSuiteRepository {
    Velocity,
    Rosen,
    Collapse,
}

impl ToolSuiteRepository {
    pub const ALL: [Self; 3] = [Self::Velocity, Self::Rosen, Self::Collapse];

    pub fn repository(self) -> &'static str {
        match self {
            Self::Velocity => "mESmaC/velocity",
            Self::Rosen => "mESmaC/rosen",
            Self::Collapse => "mESmaC/collapse.sh",
        }
    }

    pub fn short_name(self) -> &'static str {
        match self {
            Self::Velocity => "velocity",
            Self::Rosen => "rosen",
            Self::Collapse => "collapse.sh",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "velocity" | "mesmac/velocity" => Some(Self::Velocity),
            "rosen" | "mesmac/rosen" => Some(Self::Rosen),
            "collapse" | "collapse.sh" | "mesmac/collapse.sh" => Some(Self::Collapse),
            _ => None,
        }
    }
}

/// Summary of the previous completed improvement scout run, persisted across restarts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImprovementRunRecord {
    #[serde(default)]
    pub started_at: u64,
    #[serde(default)]
    pub finished_at: u64,
    #[serde(default)]
    pub filed_count: u32,
    #[serde(default)]
    pub lands_at_run: u64,
    #[serde(default)]
    pub log_cursor_secs: u64,
    #[serde(default)]
    pub signal_fingerprint: String,
}

/// Machine-wide improvement scout state persisted in the app data folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImprovementScoutState {
    #[serde(default)]
    pub total_lands: u64,
    #[serde(default)]
    pub slots_idle_since: Option<u64>,
    #[serde(default)]
    pub last_seen_commit: BTreeMap<u32, String>,
    #[serde(default)]
    pub last_run: Option<ImprovementRunRecord>,
}

impl ImprovementScoutState {
    /// Records one completed land on this machine.
    pub fn record_land(&mut self) {
        self.total_lands = self.total_lands.saturating_add(1);
    }

    /// Counts new commits at the head of a project's Rosen land history since the last seen tip.
    /// The first observation seeds the baseline commit without counting historical lands.
    pub fn observe_project_lands(
        &mut self,
        project_id: u32,
        newest_first_commits: &[String],
    ) -> u64 {
        let Some(tip) = newest_first_commits.first() else {
            return 0;
        };
        let Some(previous) = self.last_seen_commit.get(&project_id) else {
            self.last_seen_commit.insert(project_id, tip.clone());
            return 0;
        };
        if previous == tip {
            return 0;
        }
        let delta = newest_first_commits
            .iter()
            .take_while(|commit| commit.as_str() != previous.as_str())
            .count() as u64;
        self.last_seen_commit.insert(project_id, tip.clone());
        self.total_lands = self.total_lands.saturating_add(delta);
        delta
    }

    /// Tracks when free dispatch slots first had nothing dispatchable. Preserves an earlier
    /// persisted idle timestamp (`external_idle_since`) across Velocity restarts.
    pub fn observe_slots(
        &mut self,
        slots_free_and_nothing_dispatchable: bool,
        now: u64,
        external_idle_since: Option<u64>,
    ) {
        if !slots_free_and_nothing_dispatchable {
            self.slots_idle_since = None;
            return;
        }
        let candidate = external_idle_since
            .filter(|timestamp| *timestamp <= now)
            .unwrap_or(now);
        self.slots_idle_since = Some(match self.slots_idle_since {
            Some(existing) => existing.min(candidate),
            None => candidate,
        });
    }

    /// Lands accumulated since the previous completed improvement scout run.
    pub fn lands_since_last_run(&self) -> u64 {
        let baseline = self
            .last_run
            .as_ref()
            .map_or(0, |record| record.lands_at_run);
        self.total_lands.saturating_sub(baseline)
    }

    /// The `velocity.log` timestamp cursor from the previous completed run.
    pub fn log_cursor_secs(&self) -> u64 {
        self.last_run
            .as_ref()
            .map_or(0, |record| record.log_cursor_secs)
    }

    /// Records the outcome of a finished improvement scout run.
    pub fn record_run_finished(&mut self, record: ImprovementRunRecord) {
        self.last_run = Some(record);
    }

    /// Fast pre-check before building the signal digest: whether either trigger threshold
    /// has been met (ignoring the signal-fingerprint skip check).
    pub fn threshold_met(
        &self,
        running: bool,
        slots_free_and_nothing_dispatchable: bool,
        now: u64,
    ) -> bool {
        if running {
            return false;
        }
        if self.lands_since_last_run() >= LANDS_TRIGGER_COUNT {
            return true;
        }
        idle_elapsed_since_last_run(self, slots_free_and_nothing_dispatchable, now)
            .is_some_and(|seconds| seconds >= IDLE_TRIGGER_SECONDS)
    }
}

/// Why an improvement scout run is due.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImprovementTrigger {
    IdleSlots { idle_seconds: u64 },
    LandsReached { lands: u64 },
}

impl ImprovementTrigger {
    pub fn summary(self) -> String {
        match self {
            Self::IdleSlots { idle_seconds } => format!(
                "slots free with nothing dispatchable for {}m",
                idle_seconds / 60
            ),
            Self::LandsReached { lands } => {
                format!("{lands} lands since the last improvement scout run")
            }
        }
    }
}

/// Inputs checked to decide whether the machine should start an improvement scout now.
#[derive(Clone, Copy, Debug)]
pub struct ImprovementScheduleInput<'a> {
    pub running: bool,
    pub now: u64,
    pub slots_free_and_nothing_dispatchable: bool,
    pub signal_fingerprint: &'a str,
}

/// Decides whether one improvement scout should start now on this machine (#622).
///
/// Enforces:
/// - at most one running at a time;
/// - skip when the previous run filed nothing and no new signal has arrived since;
/// - start on either 25 lands since the last run or 15 minutes with slots free and nothing
///   dispatchable (measured after any previous run finished so idle slots never hot-loop).
pub fn improvement_run_due(
    state: &ImprovementScoutState,
    input: &ImprovementScheduleInput<'_>,
) -> Option<ImprovementTrigger> {
    if input.running {
        return None;
    }
    if let Some(last) = &state.last_run
        && last.filed_count == 0
        && !last.signal_fingerprint.is_empty()
        && last.signal_fingerprint == input.signal_fingerprint
    {
        return None;
    }
    let lands = state.lands_since_last_run();
    if lands >= LANDS_TRIGGER_COUNT {
        return Some(ImprovementTrigger::LandsReached { lands });
    }
    let idle_seconds =
        idle_elapsed_since_last_run(state, input.slots_free_and_nothing_dispatchable, input.now)?;
    (idle_seconds >= IDLE_TRIGGER_SECONDS).then_some(ImprovementTrigger::IdleSlots { idle_seconds })
}

fn idle_elapsed_since_last_run(
    state: &ImprovementScoutState,
    slots_free_and_nothing_dispatchable: bool,
    now: u64,
) -> Option<u64> {
    if !slots_free_and_nothing_dispatchable {
        return None;
    }
    let idle_since = state.slots_idle_since?;
    let effective_since = state
        .last_run
        .as_ref()
        .map_or(idle_since, |last| idle_since.max(last.finished_at));
    Some(now.saturating_sub(effective_since))
}

/// One deduplicated signal entry extracted from `velocity.log`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogSignalLine {
    pub source: String,
    pub text: String,
    pub count: usize,
    pub last_secs: u64,
}

/// Compact categorized extract of `velocity.log` entries since the previous run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VelocityLogDigest {
    pub project_health_skips_and_idle: Vec<LogSignalLine>,
    pub perf_budget_overruns: Vec<LogSignalLine>,
    pub stale_claims: Vec<LogSignalLine>,
    pub failed_restarts: Vec<LogSignalLine>,
    pub post_land_failures: Vec<LogSignalLine>,
    pub latest_unix_secs: u64,
    pub total_matching_entries: usize,
}

impl VelocityLogDigest {
    pub fn is_empty(&self) -> bool {
        self.total_matching_entries == 0
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LogCategory {
    ProjectHealthIdle,
    PerfBudget,
    StaleClaim,
    FailedRestart,
    PostLandFailure,
}

/// Parses `velocity.log` text (in chronological order, e.g. `velocity.old.log` then
/// `velocity.log`) and extracts compact signal lines newer than `since_unix_secs`.
pub fn parse_velocity_log_since(text: &str, since_unix_secs: u64) -> VelocityLogDigest {
    let mut digest = VelocityLogDigest {
        latest_unix_secs: since_unix_secs,
        ..VelocityLogDigest::default()
    };
    let mut current: Option<(u64, String, String)> = None;
    for line in text.lines() {
        if let Some((secs, source, body)) = parse_log_header(line) {
            if let Some(entry) = current.take() {
                ingest_log_entry(&mut digest, since_unix_secs, entry);
            }
            current = Some((secs, source.to_owned(), body.to_owned()));
        } else if let Some((_, _, body)) = &mut current {
            let trimmed = line.trim();
            if !trimmed.is_empty() && body.len() < 400 {
                body.push(' ');
                body.push_str(trimmed);
            }
        }
    }
    if let Some(entry) = current {
        ingest_log_entry(&mut digest, since_unix_secs, entry);
    }
    trim_log_category(&mut digest.project_health_skips_and_idle);
    trim_log_category(&mut digest.perf_budget_overruns);
    trim_log_category(&mut digest.stale_claims);
    trim_log_category(&mut digest.failed_restarts);
    trim_log_category(&mut digest.post_land_failures);
    digest
}

fn parse_log_header(line: &str) -> Option<(u64, &str, &str)> {
    let rest = line.strip_prefix("unix ")?;
    let (timestamp, after_ts) = rest.split_once(' ')?;
    let secs_str = timestamp.split('.').next()?;
    let secs = secs_str.parse::<u64>().ok()?;
    let after_open = after_ts.strip_prefix('[')?;
    let (source, body) = after_open.split_once("] ")?;
    Some((secs, source.trim(), body.trim()))
}

fn ingest_log_entry(
    digest: &mut VelocityLogDigest,
    since_unix_secs: u64,
    (secs, source, body): (u64, String, String),
) {
    if secs > digest.latest_unix_secs {
        digest.latest_unix_secs = secs;
    }
    if secs <= since_unix_secs || body.is_empty() {
        return;
    }
    let Some(category) = classify_log_entry(&source, &body) else {
        return;
    };
    digest.total_matching_entries = digest.total_matching_entries.saturating_add(1);
    let compact = preview(&collapse_whitespace(&body), MAX_LINE_CHARS);
    let target = match category {
        LogCategory::ProjectHealthIdle => &mut digest.project_health_skips_and_idle,
        LogCategory::PerfBudget => &mut digest.perf_budget_overruns,
        LogCategory::StaleClaim => &mut digest.stale_claims,
        LogCategory::FailedRestart => &mut digest.failed_restarts,
        LogCategory::PostLandFailure => &mut digest.post_land_failures,
    };
    if let Some(existing) = target
        .iter_mut()
        .find(|item| item.source == source && item.text == compact)
    {
        existing.count = existing.count.saturating_add(1);
        existing.last_secs = existing.last_secs.max(secs);
    } else {
        target.push(LogSignalLine {
            source,
            text: compact,
            count: 1,
            last_secs: secs,
        });
    }
}

fn classify_log_entry(source: &str, body: &str) -> Option<LogCategory> {
    let source_lower = source.to_ascii_lowercase();
    let body_lower = body.to_ascii_lowercase();
    if source_lower == BREACH_LOG_SOURCE || body_lower.contains(BREACH_LOG_SOURCE) {
        return Some(LogCategory::PerfBudget);
    }
    if body_lower.contains("stale") && body_lower.contains("claim") {
        return Some(LogCategory::StaleClaim);
    }
    if is_failed_restart(&source_lower, &body_lower) {
        return Some(LogCategory::FailedRestart);
    }
    if is_post_land_failure(&source_lower, &body_lower) {
        return Some(LogCategory::PostLandFailure);
    }
    if is_project_health_or_idle(&source_lower, &body_lower) {
        return Some(LogCategory::ProjectHealthIdle);
    }
    None
}

fn is_failed_restart(source: &str, body: &str) -> bool {
    source == "panic"
        || source == "recovery"
        || ((source.contains("restart")
            || source.contains("local build")
            || body.contains("restart"))
            && (body.contains("fail")
                || body.contains("error")
                || body.contains("rollback")
                || body.contains("abort")
                || body.contains("crash")
                || body.contains("exited")))
}

fn is_post_land_failure(source: &str, body: &str) -> bool {
    (source.contains("post-land")
        || source.contains("base ci")
        || source.contains("landing")
        || body.contains("post-land")
        || body.contains("integration build")
        || body.contains("bisect")
        || body.contains("reverted"))
        && (body.contains("fail")
            || body.contains("revert")
            || body.contains("error")
            || body.contains("stopped")
            || body.contains("timeout")
            || body.contains("uncovered"))
}

fn is_project_health_or_idle(source: &str, body: &str) -> bool {
    source == "project health"
        || source == "continuous dispatch"
        || body.contains("idle")
        || body.contains("skipped")
        || body.contains("nothing dispatched")
}

fn trim_log_category(lines: &mut Vec<LogSignalLine>) {
    lines.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| right.last_secs.cmp(&left.last_secs))
            .then_with(|| left.text.cmp(&right.text))
    });
    lines.truncate(MAX_LINES_PER_LOG_CATEGORY);
}

/// All fleet signals combined into one compact digest for the improvement scout (#622).
#[derive(Clone, Debug)]
pub struct ImprovementDigestInput {
    pub trigger: ImprovementTrigger,
    pub total_lands: u64,
    pub lands_since_last_run: u64,
    pub fleet_stats: Option<String>,
    pub friction_groups: Option<String>,
    pub velocity_log: VelocityLogDigest,
    pub rosen_stats: Option<String>,
    pub prior_outcomes: Option<String>,
}

/// Deterministic signal fingerprint used to skip re-running when the previous run filed
/// nothing and no new signal has arrived since. Excludes volatile wall-clock durations.
pub fn signal_fingerprint(input: &ImprovementDigestInput) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |bytes: &[u8]| {
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    };
    mix(&input.total_lands.to_le_bytes());
    mix(input.fleet_stats.as_deref().unwrap_or("").as_bytes());
    mix(input.friction_groups.as_deref().unwrap_or("").as_bytes());
    mix(input.rosen_stats.as_deref().unwrap_or("").as_bytes());
    mix(input.prior_outcomes.as_deref().unwrap_or("").as_bytes());
    mix(&input.velocity_log.total_matching_entries.to_le_bytes());
    for section in [
        &input.velocity_log.project_health_skips_and_idle,
        &input.velocity_log.perf_budget_overruns,
        &input.velocity_log.stale_claims,
        &input.velocity_log.failed_restarts,
        &input.velocity_log.post_land_failures,
    ] {
        for line in section {
            mix(line.source.as_bytes());
            mix(line.text.as_bytes());
            mix(&line.count.to_le_bytes());
        }
    }
    format!("{hash:016x}")
}

/// Builds one compact Markdown digest covering all five signal sources from #622.
pub fn build_compact_digest(input: &ImprovementDigestInput) -> String {
    let mut out = format!(
        "# Fleet Improvement Signal Digest\n\nTrigger: {}\nTotal lands observed: {} ({} since last run)\n",
        input.trigger.summary(),
        input.total_lands,
        input.lands_since_last_run,
    );
    append_optional_section(
        &mut out,
        "Fleet stats rollups and top offenders (#619)",
        input.fleet_stats.as_deref(),
        "Unavailable locally (call the AIRC `fleet_stats` tool if available).",
        3_600,
    );
    append_optional_section(
        &mut out,
        "Friction and tool-request groups below filing threshold (#620)",
        input.friction_groups.as_deref(),
        "None recorded below threshold.",
        2_800,
    );
    append_velocity_log_section(&mut out, &input.velocity_log);
    append_optional_section(
        &mut out,
        "Rosen stats (mESmaC/rosen#449)",
        input.rosen_stats.as_deref(),
        "Unavailable (rosen stats command not supported or no server active).",
        2_400,
    );
    append_optional_section(
        &mut out,
        "Measured outcomes of earlier self-improvement issues (#621)",
        input.prior_outcomes.as_deref(),
        "None recorded yet.",
        2_400,
    );
    prompt_excerpt(&out, DIGEST_BUDGET_BYTES, "\n[Digest truncated to budget]")
}

fn append_optional_section(
    out: &mut String,
    heading: &str,
    content: Option<&str>,
    fallback: &str,
    limit: usize,
) {
    out.push_str("\n## ");
    out.push_str(heading);
    out.push_str("\n\n");
    match content.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => {
            out.push_str(&prompt_excerpt(text, limit, "\n[Section excerpted]"));
            out.push('\n');
        }
        None => {
            out.push_str(fallback);
            out.push('\n');
        }
    }
}

fn append_velocity_log_section(out: &mut String, log: &VelocityLogDigest) {
    out.push_str("\n## velocity.log since last run\n\n");
    if log.is_empty() {
        out.push_str("No matching health, perf-budget, stale-claim, restart or post-land signals since last run.\n");
        return;
    }
    let groups = [
        (
            "Project health skips and idle spells",
            &log.project_health_skips_and_idle,
        ),
        ("Perf-budget overruns", &log.perf_budget_overruns),
        ("Stale claims", &log.stale_claims),
        ("Failed restarts", &log.failed_restarts),
        ("Post-land failures", &log.post_land_failures),
    ];
    for (title, lines) in groups {
        if lines.is_empty() {
            continue;
        }
        out.push_str(&format!("### {title}\n"));
        for line in lines {
            if line.count > 1 {
                out.push_str(&format!(
                    "- (x{}) [{}] {}\n",
                    line.count, line.source, line.text
                ));
            } else {
                out.push_str(&format!("- [{}] {}\n", line.source, line.text));
            }
        }
    }
}

/// Extracts the value after `Target metric:` on any line (stripping Markdown bold and list markers).
pub fn parse_target_metric_line(text: &str) -> Option<String> {
    for line in text.lines() {
        let plain = line
            .trim()
            .trim_start_matches(['-', '*', '•'])
            .trim()
            .replace("**", "");
        let Some((head, rest)) = plain.split_once(':') else {
            continue;
        };
        if !head.trim().eq_ignore_ascii_case("target metric") {
            continue;
        }
        let value = rest.trim();
        if !value.is_empty() {
            return Some(collapse_whitespace(value));
        }
    }
    None
}

/// Validates a `Target metric:` specification naming a fleet metric and direction, e.g.
/// `idle slot-minutes per day, down`.
pub fn validate_target_metric(raw: &str) -> Result<String, String> {
    let stripped = raw
        .trim()
        .replace("**", "")
        .strip_prefix("Target metric:")
        .or_else(|| raw.trim().strip_prefix("target metric:"))
        .unwrap_or(raw)
        .trim()
        .to_owned();
    let cleaned = collapse_whitespace(&stripped);
    if cleaned.is_empty() || cleaned.chars().count() > MAX_TARGET_METRIC_CHARS {
        return Err(
            "Target metric must be one nonempty line of at most 200 characters (for example `idle slot-minutes per day, down`)"
                .into(),
        );
    }
    if raw.contains(['\r', '\n']) {
        return Err("Target metric must be a single line".into());
    }
    let lower = cleaned.to_ascii_lowercase();
    let has_direction = lower
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|word| DIRECTION_WORDS.contains(&word));
    if !has_direction {
        return Err(
            "Target metric must name both the fleet metric and its direction (for example `idle slot-minutes per day, down` or `lands per hour, up`)"
                .into(),
        );
    }
    Ok(cleaned)
}

/// Builds the prompt for one bounded, read-only improvement scout session (#622).
pub fn improvement_scout_prompt(digest: &str, allowed_repositories: &[&str]) -> String {
    let repositories = allowed_repositories.join(", ");
    let prompt = format!(
        "You are Velocity's Improvement Scout (focus area: Development speed, F7).\n\n\
         Your job is to inspect the fleet's signals below and file at most {MAX_ISSUES_PER_RUN} \
         high-leverage improvements that will speed up development across the tool suite \
         ({repositories}).\n\n\
         Rules:\n\
         - Prefer removing a step, a wait or a manual check over adding a feature.\n\
         - File at most {MAX_ISSUES_PER_RUN} issues this run, each in the right repository \
           ({repositories}). If nothing warrants filing, file 0 issues and explain why.\n\
         - Before filing in a repository, search open and closed issues with \
           `improvement_scout_issues` and inspect likely duplicates with `improvement_scout_issue`. \
           Never file a duplicate.\n\
         - File only through `improvement_scout_file` (preview with `publish: false`, then \
           `publish: true`). Velocity automatically applies the `{SELF_IMPROVEMENT_LABEL}` label \
           (creating it if needed).\n\
         - Every issue must cite concrete evidence from the digest or local code and supply a \
           `target_metric` naming the fleet metric it should move and in which direction, \
           for example `idle slot-minutes per day, down`.\n\
         - Read-only session: do not edit files, claim paths or name Rosen changes. \
           End your turn with a one-line summary of what you filed (or why you filed nothing).\n\n\
         {digest}\n"
    );
    prompt_excerpt(
        &prompt,
        crate::text_budget::PROMPT_BYTES,
        "\n[Digest excerpt]",
    )
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Risk (#622): the improvement scout's trigger conditions (15m idle or 25 lands),
    // one-at-a-time gate, and skip-when-nothing-filed-and-no-new-signal guard keep the
    // machine from either staying idle without scouting or hot-looping duplicate sessions.
    #[test]
    fn trigger_conditions_and_one_at_a_time_rule() {
        let mut state = ImprovementScoutState::default();

        // Idle for less than 15 minutes does not trigger.
        state.observe_slots(true, 1_000, None);
        assert_eq!(
            improvement_run_due(
                &state,
                &ImprovementScheduleInput {
                    running: false,
                    now: 1_000 + IDLE_TRIGGER_SECONDS - 1,
                    slots_free_and_nothing_dispatchable: true,
                    signal_fingerprint: "sig-1",
                },
            ),
            None
        );

        // At 15 minutes idle with nothing dispatchable, trigger fires.
        assert_eq!(
            improvement_run_due(
                &state,
                &ImprovementScheduleInput {
                    running: false,
                    now: 1_000 + IDLE_TRIGGER_SECONDS,
                    slots_free_and_nothing_dispatchable: true,
                    signal_fingerprint: "sig-1",
                },
            ),
            Some(ImprovementTrigger::IdleSlots {
                idle_seconds: IDLE_TRIGGER_SECONDS,
            })
        );

        // One-at-a-time rule: never starts a second scout while one is running.
        assert_eq!(
            improvement_run_due(
                &state,
                &ImprovementScheduleInput {
                    running: true,
                    now: 1_000 + IDLE_TRIGGER_SECONDS + 300,
                    slots_free_and_nothing_dispatchable: true,
                    signal_fingerprint: "sig-2",
                },
            ),
            None
        );

        // If the previous run filed nothing and no new signal arrived, skip even after 15m.
        state.record_run_finished(ImprovementRunRecord {
            started_at: 1_900,
            finished_at: 2_000,
            filed_count: 0,
            lands_at_run: 0,
            log_cursor_secs: 2_000,
            signal_fingerprint: "sig-1".into(),
        });
        assert_eq!(
            improvement_run_due(
                &state,
                &ImprovementScheduleInput {
                    running: false,
                    now: 2_000 + IDLE_TRIGGER_SECONDS + 60,
                    slots_free_and_nothing_dispatchable: true,
                    signal_fingerprint: "sig-1",
                },
            ),
            None
        );

        // Once a new signal arrives after 15m idle, trigger fires again.
        assert_eq!(
            improvement_run_due(
                &state,
                &ImprovementScheduleInput {
                    running: false,
                    now: 2_000 + IDLE_TRIGGER_SECONDS,
                    slots_free_and_nothing_dispatchable: true,
                    signal_fingerprint: "sig-2",
                },
            ),
            Some(ImprovementTrigger::IdleSlots {
                idle_seconds: IDLE_TRIGGER_SECONDS,
            })
        );

        // Even while slots are busy, 25 lands since the last run triggers a new run.
        state.observe_slots(false, 3_000, None);
        for _ in 0..(LANDS_TRIGGER_COUNT - 1) {
            state.record_land();
        }
        assert_eq!(
            improvement_run_due(
                &state,
                &ImprovementScheduleInput {
                    running: false,
                    now: 3_100,
                    slots_free_and_nothing_dispatchable: false,
                    signal_fingerprint: "sig-3",
                },
            ),
            None
        );
        state.record_land();
        assert_eq!(
            improvement_run_due(
                &state,
                &ImprovementScheduleInput {
                    running: false,
                    now: 3_100,
                    slots_free_and_nothing_dispatchable: false,
                    signal_fingerprint: "sig-4",
                },
            ),
            Some(ImprovementTrigger::LandsReached {
                lands: LANDS_TRIGGER_COUNT,
            })
        );

        // Still gated by one-at-a-time when 25 lands have happened.
        assert_eq!(
            improvement_run_due(
                &state,
                &ImprovementScheduleInput {
                    running: true,
                    now: 3_100,
                    slots_free_and_nothing_dispatchable: false,
                    signal_fingerprint: "sig-4",
                },
            ),
            None
        );
    }

    // Risk (#622): idle spells after a run that filed issues must wait a fresh 15m window
    // instead of hot-looping immediately while slots remain idle.
    #[test]
    fn post_run_idle_waits_fresh_fifteen_minutes() {
        let mut state = ImprovementScoutState::default();
        state.observe_slots(true, 1_000, None);
        state.record_run_finished(ImprovementRunRecord {
            started_at: 1_900,
            finished_at: 2_000,
            filed_count: 2,
            lands_at_run: 0,
            log_cursor_secs: 2_000,
            signal_fingerprint: "sig-1".into(),
        });
        assert_eq!(
            improvement_run_due(
                &state,
                &ImprovementScheduleInput {
                    running: false,
                    now: 2_005,
                    slots_free_and_nothing_dispatchable: true,
                    signal_fingerprint: "sig-1",
                },
            ),
            None
        );
        assert_eq!(
            improvement_run_due(
                &state,
                &ImprovementScheduleInput {
                    running: false,
                    now: 2_000 + IDLE_TRIGGER_SECONDS,
                    slots_free_and_nothing_dispatchable: true,
                    signal_fingerprint: "sig-1",
                },
            ),
            Some(ImprovementTrigger::IdleSlots {
                idle_seconds: IDLE_TRIGGER_SECONDS,
            })
        );
    }

    // Risk (#622): velocity.log extraction must skip entries at/before the previous cursor
    // and classify all five operational signal classes compactly.
    #[test]
    fn parses_velocity_log_categories_since_cursor() {
        let log = "\
unix 100.000 [project health] old skip line\n\
unix 200.123 [project health] velocity: slots idle for 20m; skipped #10 (blocked)\n\
unix 201.000 [perf-budget] ui_command p95 340ms > 200ms\n\
unix 202.000 [project health] released stale AIRC claim by issue-42\n\
unix 203.000 [recovery] restart failed after panic\n\
unix 204.000 [post-land] integration build failed and reverted change abc\n";
        let digest = parse_velocity_log_since(log, 100);
        assert_eq!(digest.latest_unix_secs, 204);
        assert_eq!(digest.total_matching_entries, 5);
        assert_eq!(digest.project_health_skips_and_idle.len(), 1);
        assert_eq!(digest.perf_budget_overruns.len(), 1);
        assert_eq!(digest.stale_claims.len(), 1);
        assert_eq!(digest.failed_restarts.len(), 1);
        assert_eq!(digest.post_land_failures.len(), 1);
    }

    // Risk (#622): every self-improvement issue must state a concrete metric and direction.
    #[test]
    fn validates_target_metric_lines() {
        assert_eq!(
            validate_target_metric("idle slot-minutes per day, down").as_deref(),
            Ok("idle slot-minutes per day, down")
        );
        assert_eq!(
            validate_target_metric("Target metric: lands per hour, up").as_deref(),
            Ok("lands per hour, up")
        );
        assert!(validate_target_metric("make dispatch better").is_err());
        assert!(validate_target_metric("").is_err());
        assert_eq!(
            parse_target_metric_line(
                "## Why\n\nToo slow.\n\n**Target metric:** idle slot-minutes per day, down\n"
            )
            .as_deref(),
            Some("idle slot-minutes per day, down")
        );
        assert_eq!(
            parse_target_metric_line("Intro line\n- Target metric: lands per hour, up").as_deref(),
            Some("lands per hour, up")
        );
    }

    // Risk (#622): Rosen land counting must seed each project's initial tip without
    // counting pre-existing history and accumulate only newly landed commits.
    #[test]
    fn observes_new_lands_across_projects_from_baseline() {
        let mut state = ImprovementScoutState::default();
        assert_eq!(
            state.observe_project_lands(1, &["c2".into(), "c1".into()]),
            0
        );
        assert_eq!(state.total_lands, 0);
        assert_eq!(
            state.observe_project_lands(1, &["c4".into(), "c3".into(), "c2".into(), "c1".into()]),
            2
        );
        assert_eq!(state.total_lands, 2);
        assert_eq!(
            state.observe_project_lands(1, &["c4".into(), "c3".into()]),
            0
        );
        assert_eq!(state.total_lands, 2);
    }
}
