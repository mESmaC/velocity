//! Scout sweeps (SPEC.md F7 Scouts): when a scout sweeps again, what its agent is told, and
//! what its card and the Duty view's activity feed say about it.

use crate::duty::{ScoutCadence, ScoutFocusArea, ScoutRuleSettings};

/// "Every 30 m" (F7 Sweep).
pub const SWEEP_INTERVAL_SECONDS: u64 = 30 * 60;

/// The longest activity line a card shows; the agent's own step titles can be long.
const DOING_LIMIT: usize = 72;

/// A scout's last sweep, saved so its cadence survives a restart.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SweepRecord {
    /// Unix seconds when the sweep started.
    pub started_at: u64,
    /// The commit the sweep started from, when known. "After every merge" sweeps again when
    /// the base branch's tip moves past it.
    #[serde(default)]
    pub base_commit: Option<String>,
    /// The scout's sweep worktree (`<scout>-sweep`), which its next sweep reuses.
    #[serde(default)]
    pub worktree: String,
}

impl SweepRecord {
    pub fn new(started_at: u64, base_commit: Option<String>, worktree: String) -> Self {
        Self {
            started_at,
            base_commit,
            worktree,
        }
    }
}

/// Why a sweep starts. It heads the sweep's prompt and its feed line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SweepReason {
    First,
    Interval,
    NewCommits {
        base_commit: String,
    },
    Instruction,
    /// Velocity closed during the scout's last sweep, which stopped it: sweep again.
    Interrupted,
}

/// What decides whether a scout sweeps now.
#[derive(Clone, Copy, Debug)]
pub struct SweepSchedule<'a> {
    pub cadence: ScoutCadence,
    pub paused: bool,
    pub running: bool,
    pub last: Option<&'a SweepRecord>,
    pub now: u64,
    /// The base branch's tip now, when it could be read.
    pub base_commit: Option<&'a str>,
    /// The saved card says a sweep runs, but none does: the app closed during it (§3.2).
    pub interrupted: bool,
}

/// The sweep a scout starts now, if any. A paused role or a running sweep starts nothing
/// (F7: pausing finishes in-flight work and starts nothing new); every scout sweeps once
/// when it joins, then as its cadence says.
pub fn sweep_due(schedule: &SweepSchedule) -> Option<SweepReason> {
    if schedule.paused || schedule.running {
        return None;
    }
    if schedule.interrupted {
        return Some(SweepReason::Interrupted);
    }
    let Some(last) = schedule.last else {
        return Some(SweepReason::First);
    };
    match schedule.cadence {
        ScoutCadence::Once => None,
        ScoutCadence::EveryThirtyMinutes => (schedule.now
            >= last.started_at.saturating_add(SWEEP_INTERVAL_SECONDS))
        .then_some(SweepReason::Interval),
        ScoutCadence::AfterEveryMerge => {
            let current = schedule.base_commit?;
            (last.base_commit.as_deref() != Some(current)).then(|| SweepReason::NewCommits {
                base_commit: current.to_owned(),
            })
        }
    }
}

/// Everything a sweep's prompt is built from.
#[derive(Clone, Copy, Debug)]
pub struct SweepBrief<'a> {
    pub nickname: &'a str,
    /// The scout's place in the role, from 0, so several scouts split the repo.
    pub scout_index: usize,
    pub scout_count: usize,
    pub rules: &'a ScoutRuleSettings,
    pub reason: &'a SweepReason,
    pub base_branch: &'a str,
    /// What you told the scouts from the Duty view's composer, if this sweep follows it.
    pub instruction: Option<&'a str>,
    /// How many generated inputs to diff per feature against its Lean model (Settings →
    /// Proofs, F13, F17).
    pub stress_diff_input_count: u32,
}

/// The sweep's prompt: the scout rules of F7, with the locked rules enforced by the
/// `scout_issues` / `scout_file` tools rather than trusted to the prompt.
pub fn sweep_prompt(brief: &SweepBrief) -> String {
    let mut prompt = format!(
        "You are {}, a scout on this project (Velocity duty role: Scouts). {}\n\n",
        brief.nickname,
        reason_sentence(brief.reason, brief.base_branch)
    );
    if brief.rules.assume_every_part_fails {
        prompt.push_str(
            "Assume every part of the application is broken and will fail, then prove it.\n",
        );
    }
    if brief.scout_count > 1 {
        prompt.push_str(&format!(
            "You are scout {} of {}. List the repository's top-level folders in name order and take every {}th one starting with number {}, so each scout covers a different area.\n",
            brief.scout_index + 1,
            brief.scout_count,
            brief.scout_count,
            brief.scout_index + 1
        ));
    }
    prompt.push_str(&sweep_steps(brief.rules, brief.stress_diff_input_count));
    prompt.push_str(&filing_rules(brief.rules));
    if let Some(instruction) = brief.instruction {
        prompt.push_str(&format!("\nThe user asks the scouts: {instruction}\n"));
    }
    prompt.push_str(
        "\nThis worktree is a detached checkout that is reset before your next sweep: do not commit, push or open PRs, and do not fix what you find. End your turn with a one-line summary of what you filed.\n",
    );
    prompt
}

fn reason_sentence(reason: &SweepReason, base_branch: &str) -> String {
    match reason {
        SweepReason::First => format!("Start your first sweep of `{base_branch}`."),
        SweepReason::Interval => {
            format!(
                "Thirty minutes have passed since your last sweep; sweep `{base_branch}` again."
            )
        }
        SweepReason::NewCommits { base_commit } => format!(
            "`{base_branch}` moved to {} since your last sweep; sweep again and start with what changed.",
            short_commit(base_commit)
        ),
        SweepReason::Instruction => {
            format!("The user pointed the scouts somewhere; sweep `{base_branch}` with that focus.")
        }
        SweepReason::Interrupted => format!(
            "Velocity closed during your last sweep, which stopped it; sweep `{base_branch}` again. Issues you filed then are in `scout_issues`."
        ),
    }
}

fn sweep_steps(rules: &ScoutRuleSettings, stress_diff_input_count: u32) -> String {
    let mut steps = String::from(
        "\nSweep:\n- Read AGENTS.md and SPEC.md first; follow AGENTS.md's rules for scouts.\n- Run the project's checks and lint, and the existing tests and stress scripts.\n",
    );
    if rules.run_stress_against_models {
        steps.push_str("- Build the Lean reference models in `proofs/` where they exist. Use `scout_compare` with implementation and compiled-model driver scripts, feeding identical generated cases through stdin. Both must succeed; normalize their stdout in the adapters. Preserve every disagreeing input in a failing reproduction script and file it through `scout_file`. Missing models or toolchain failures are proof gaps, not output disagreements.\n");
        steps.push_str(&format!(
            "- Diff at least {stress_diff_input_count} generated inputs for each feature that has a compiled model, split across as many `scout_compare` calls as its input size limit needs.\n"
        ));
    }
    steps.push_str("- Write new tests and stress scripts that expose failures. Keep each failing script in the worktree; it is your evidence.\n");
    for area in &rules.focus_areas {
        steps.push_str(&format!("- {}: {}\n", area.label(), focus_guide(*area)));
    }
    steps
}

fn focus_guide(area: ScoutFocusArea) -> &'static str {
    match area {
        ScoutFocusArea::Bugs => "wrong results, crashes and hangs on the main paths.",
        ScoutFocusArea::Stress => {
            "load, concurrency, malformed input, resource exhaustion, crash and restart."
        }
        ScoutFocusArea::ProofGaps => {
            "features with no proof, statements weaker than SPEC.md, hypotheses that exclude real inputs, and languages stuck at model level when reference comparison is available."
        }
        ScoutFocusArea::SpecDrift => "behaviour that differs from what SPEC.md says.",
        ScoutFocusArea::Performance => "slow paths, unbounded growth and wasted work.",
        ScoutFocusArea::Security => {
            "injection, path traversal, secrets in logs or files, unsafe process launches."
        }
        ScoutFocusArea::DeadCode => "code nothing calls and features nothing reaches.",
        ScoutFocusArea::Ux => "the window disagreeing with the reference UI and SPEC.md §5.",
    }
}

fn filing_rules(rules: &ScoutRuleSettings) -> String {
    let labels = if rules.label_issues_for_dispatch {
        "Label each issue for dispatch with the repository's labels that say what kind of work it is (bug, ux, ...)."
    } else {
        "Give each issue at least one of the repository's existing labels."
    };
    format!(
        "\nFiling (locked rules):\n- Never file a duplicate. Call `scout_issues` first and compare each finding with every open and closed issue it returns, by meaning, not by title.\n- No issue without evidence. File only with `scout_file`: Velocity runs your reproduction script, which must exit with a failing status, and attaches the script and its output. Preview with publish false, then file with publish true.\n- {labels}\n- Never file through `gh` or the GitHub website.\n"
    )
}

/// The feed line a starting sweep posts (F7 activity feed).
pub fn sweep_started_message(reason: &SweepReason, rules: &ScoutRuleSettings) -> String {
    match reason {
        SweepReason::First => {
            let areas: Vec<&str> = rules
                .focus_areas
                .iter()
                .map(|area| area.summary_token())
                .collect();
            let lead = if rules.assume_every_part_fails {
                "Sweep started · assuming everything fails"
            } else {
                "Sweep started"
            };
            if areas.is_empty() {
                lead.into()
            } else {
                format!("{lead} · {}", areas.join(", "))
            }
        }
        SweepReason::Interval => "Sweep started · 30m since the last sweep".into(),
        SweepReason::NewCommits { base_commit } => format!(
            "Sweep started · new commits since the last sweep (now at {})",
            short_commit(base_commit)
        ),
        SweepReason::Instruction => "Sweep started · following your instruction".into(),
        SweepReason::Interrupted => {
            "Sweep started · again, after a restart stopped the last one".into()
        }
    }
}

/// The feed line a finished sweep posts.
pub fn sweep_finished_message(filed: usize, skipped: usize, failure: Option<&str>) -> String {
    let counts = format!("{filed} filed · {skipped} skipped as duplicates");
    match failure {
        Some(failure) => format!("Sweep stopped · {counts} · {}", clip(failure)),
        None => format!("Sweep finished · {counts}"),
    }
}

/// What an idle scout's card says: when it sweeps next.
pub fn sweep_idle_doing(cadence: ScoutCadence, paused: bool, last: &SweepRecord) -> String {
    if paused {
        return "paused".into();
    }
    match cadence {
        ScoutCadence::Once => "sweep done".into(),
        ScoutCadence::EveryThirtyMinutes => format!(
            "idle · next sweep at {}",
            clock_label(last.started_at.saturating_add(SWEEP_INTERVAL_SECONDS))
        ),
        ScoutCadence::AfterEveryMerge => "idle · waiting for the next merge".into(),
    }
}

/// A card line from the agent's current step title, kept to one short line. An MCP tool's
/// step (`mcp__<server>__<tool>`) shows just the tool's name.
pub fn step_doing(title: &str) -> String {
    let line = title.lines().next().unwrap_or_default().trim();
    let line = match line.strip_prefix("mcp__") {
        Some(qualified) => qualified.rsplit("__").next().unwrap_or(qualified),
        None => line,
    };
    if line.is_empty() {
        "sweeping".into()
    } else {
        clip(line)
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() <= DOING_LIMIT {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(DOING_LIMIT - 1).collect();
    clipped.push('…');
    clipped
}

fn short_commit(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

/// `HH:MM` the way the feed's timestamps are written (UTC, from the store's clock).
fn clock_label(seconds: u64) -> String {
    let day_seconds = seconds % 86_400;
    format!(
        "{:02}:{:02}",
        day_seconds / 3_600,
        (day_seconds % 3_600) / 60
    )
}
