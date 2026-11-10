//! Goal swarm dialog policies (SPEC.md F16): one prompt, N agents and a model mix whose
//! first row is "the rest". Pure arithmetic and labels; the store supplies the models, the
//! pool and PR duty, and the window only holds the user's choices.

use std::collections::BTreeMap;

pub const DEFAULT_PROMPT: &str = "Read AGENTS.md and start dev.";
pub const MAXIMUM_AGENTS: u32 = 30;
pub const DEFAULT_AGENTS: u32 = 10;
pub const QUICK_PICKS: [u32; 4] = [3, 5, 10, 20];
/// A provider's limits line turns amber and names the reset from this share (F16 Limits).
const HOT_LIMIT_PERCENT: u8 = 60;

/// The open Goal swarm dialog and the prompt it starts with (F16).
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoalDialogRequest {
    pub prompt: String,
}

impl GoalDialogRequest {
    /// Text typed in the empty composer prefills the prompt; otherwise the default.
    pub fn new(typed: &str) -> Self {
        let typed = typed.trim();
        Self {
            prompt: if typed.is_empty() {
                DEFAULT_PROMPT.into()
            } else {
                typed.into()
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MixRow<Model> {
    pub model: Model,
    pub count: u32,
}

/// How many agents run on each model. Row 0 is "the rest": it absorbs the difference, so
/// the rows always add up to `agents`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mix<Model> {
    pub agents: u32,
    pub rows: Vec<MixRow<Model>>,
}

pub enum MixEdit<Model> {
    SetAgents(u32),
    StepAgents { up: bool },
    StepRow { index: usize, up: bool },
    RemoveRow(usize),
    AddRow(Model),
    EvenSplit,
}

impl<Model> Mix<Model> {
    pub fn apply(&mut self, edit: MixEdit<Model>) {
        match edit {
            MixEdit::SetAgents(count) => self.agents = count,
            MixEdit::StepAgents { up } => self.agents = step(self.agents, up),
            MixEdit::StepRow { index, up } => self.step_row(index, up),
            MixEdit::RemoveRow(index) => {
                if index > 0 && index < self.rows.len() {
                    self.rows.remove(index);
                }
            }
            MixEdit::AddRow(model) => {
                self.take_from_rest();
                self.rows.push(MixRow { model, count: 1 });
            }
            MixEdit::EvenSplit => self.split_evenly(),
        }
        self.settle();
    }

    /// Restores the invariant after any change: 1–30 agents, later rows trimmed from the
    /// end until they fit, emptied rows dropped, and the rest given what remains.
    pub fn settle(&mut self) {
        self.agents = self.agents.clamp(1, MAXIMUM_AGENTS);
        let mut others = self.others();
        for row in self.rows.iter_mut().skip(1).rev() {
            let cut = row.count.min(others.saturating_sub(self.agents));
            row.count -= cut;
            others -= cut;
        }
        let mut position = 0;
        self.rows.retain(|row| {
            position += 1;
            position == 1 || row.count > 0
        });
        let agents = self.agents;
        if let Some(rest) = self.rows.first_mut() {
            rest.count = agents - others;
        }
    }

    fn others(&self) -> u32 {
        self.rows
            .iter()
            .skip(1)
            .fold(0, |sum, row| sum.saturating_add(row.count))
    }

    fn rest(&self) -> u32 {
        self.rows.first().map_or(0, |row| row.count)
    }

    /// Another row's agent comes out of the rest; with the rest empty the total grows.
    fn take_from_rest(&mut self) {
        if self.rest() == 0 {
            self.agents = self.agents.saturating_add(1);
        }
    }

    fn step_row(&mut self, index: usize, up: bool) {
        if index == 0 {
            // "The rest" has no count of its own, so its stepper changes the total.
            if up || self.rest() > 0 {
                self.agents = step(self.agents, up);
            }
        } else if up && index < self.rows.len() {
            self.take_from_rest();
            self.rows[index].count = self.rows[index].count.saturating_add(1);
        } else if let Some(row) = self.rows.get_mut(index)
            && row.count > 1
        {
            row.count -= 1;
        }
    }

    fn split_evenly(&mut self) {
        let rows = u32::try_from(self.rows.len()).unwrap_or(u32::MAX).max(1);
        let agents = self.agents.clamp(1, MAXIMUM_AGENTS);
        for (index, row) in (0..).zip(self.rows.iter_mut()) {
            row.count = agents / rows + u32::from(index < agents % rows);
        }
    }
}

fn step(count: u32, up: bool) -> u32 {
    if up {
        count.saturating_add(1)
    } else {
        count.saturating_sub(1)
    }
}

/// A goal swarm agent's nick stem: its model's short name (`sol`, `argon`, `fable`), the
/// last plain word of the model name (F14 Nicks).
pub fn model_nick(model_name: &str) -> String {
    let plain = |word: &str| word.len() >= 3 && word.chars().all(char::is_alphabetic);
    let words: Vec<&str> = model_name.split_whitespace().collect();
    let word = words
        .iter()
        .rev()
        .copied()
        .find(|word| plain(word))
        // No plain word ("GPT-6", "K2"): keep the first word's letters and digits.
        .or_else(|| words.first().copied())
        .unwrap_or_default();
    let nick: String = word
        .chars()
        .filter(|letter| letter.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .take(12)
        .collect();
    if nick.is_empty() {
        "agent".into()
    } else {
        nick
    }
}

/// `sol-1…6 · argon-1…2`: the nicks a launch would hand out, numbered on from the highest
/// number each stem has used in the project, without spending those numbers.
pub fn nick_preview(rows: &[(String, u32)], used: &BTreeMap<String, u32>) -> String {
    let mut seen = used.clone();
    let mut parts = Vec::new();
    for (nick, count) in rows.iter().filter(|(_, count)| *count > 0) {
        let last = seen.entry(nick.clone()).or_insert(0);
        let first = *last + 1;
        *last += count;
        parts.push(if *count == 1 {
            format!("{nick}-{first}")
        } else {
            format!("{nick}-{first}\u{2026}{last}")
        });
    }
    parts.join(" · ")
}

/// Hands out one project-scoped nick per agent (`sol-1`, `sol-2`, `argon-1`), following
/// the exact numbering shown by [`nick_preview`].
pub fn allocate_nicks(rows: &[(String, u32)], used: &BTreeMap<String, u32>) -> Vec<String> {
    let mut seen = used.clone();
    let mut nicks = Vec::new();
    for (nick, count) in rows.iter().filter(|(_, count)| *count > 0) {
        let last = seen.entry(nick.clone()).or_insert(0);
        for _ in 0..*count {
            *last = last.saturating_add(1);
            nicks.push(format!("{nick}-{last}"));
        }
    }
    nicks
}

/// Sidebar/header title for a goal swarm (Goal · <prompt>, SPEC.md F16).
pub fn goal_swarm_title(prompt: &str) -> String {
    let clean = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(DEFAULT_PROMPT)
        .trim_end_matches('.');
    let clipped: String = clean.chars().take(42).collect();
    let suffix = if clean.chars().count() > 42 {
        "\u{2026}"
    } else {
        ""
    };
    format!("Goal \u{00b7} {clipped}{suffix}")
}

/// How much of a provider's subscription window is used, as the provider reports it (F15).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitUsage {
    pub percent_used: u8,
    pub window: String,
    pub resets_in: String,
}

/// One provider's limits line: its agents, the share of its window used, and the reset
/// time once 60% or more is used. The flag marks that amber state.
pub fn limit_line(provider_name: &str, agents: u32, usage: Option<&LimitUsage>) -> (String, bool) {
    let Some(usage) = usage else {
        return (format!("{provider_name} ×{agents}"), false);
    };
    let hot = usage.percent_used >= HOT_LIMIT_PERCENT;
    let mut line = format!(
        "{provider_name} ×{agents} · {}% of {} used",
        usage.percent_used, usage.window
    );
    if hot {
        line.push_str(&format!(" · resets in {}", usage.resets_in));
    }
    (line, hot)
}

/// Unclaimed pieces in the work pool, by where they come from (F16 Pool).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PoolCounts {
    pub tasks: u32,
    pub issues: u32,
    pub sections: u32,
}

impl PoolCounts {
    pub fn new(tasks: u32, issues: u32, sections: u32) -> Self {
        Self {
            tasks,
            issues,
            sections,
        }
    }

    pub fn total(self) -> u32 {
        self.tasks
            .saturating_add(self.issues)
            .saturating_add(self.sections)
    }

    /// PRs this launch can open: an agent that finds nothing to claim opens none.
    pub fn new_pull_requests(self, agents: u32) -> u32 {
        agents.min(self.total())
    }
}

/// "They choose from **N unclaimed pieces**: x Task List · y issues · z unbuilt SPEC.md
/// sections.", in the three parts the dialog styles differently.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolLine {
    pub pieces: String,
    pub sources: String,
    /// Set when there are more agents than pieces: how many will find nothing and leave.
    pub spare: Option<String>,
}

pub fn pool_line(pool: PoolCounts, agents: u32) -> PoolLine {
    let total = pool.total();
    let spare = agents.saturating_sub(total);
    PoolLine {
        pieces: format!("{total} unclaimed piece{}", plural(total)),
        sources: format!(
            "{} Task List · {} issue{} · {} unbuilt SPEC.md section{}.",
            pool.tasks,
            pool.issues,
            plural(pool.issues),
            pool.sections,
            plural(pool.sections)
        ),
        spare: (spare > 0).then(|| {
            format!(
                "{spare} agent{} will find nothing left to claim and leave.",
                plural(spare)
            )
        }),
    }
}

/// "10 worktrees on D:", or without the drive when the root has none.
pub fn worktrees_hint(agents: u32, worktree_root: &str) -> String {
    let drive: String = worktree_root.chars().take(2).collect();
    let place = if drive.ends_with(':') {
        format!(" on {drive}")
    } else {
        String::new()
    };
    format!("{agents} worktree{}{place}", plural(agents))
}

pub fn launch_label(agents: u32) -> String {
    format!("Launch {agents} agent{}", plural(agents))
}

/// Activity feed tags for a goal swarm (SPEC.md F16: claims, refusals, PRs, waits,
/// merges and leaves).
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalEventKind {
    You,
    Claim,
    Refused,
    Opened,
    Changes,
    Wait,
    Merged,
    Left,
    Failed,
    Finished,
}

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GoalFeedEvent {
    pub timestamp: String,
    pub actor: String,
    pub kind: GoalEventKind,
    pub message: String,
}

impl GoalFeedEvent {
    pub fn new(timestamp: &str, actor: &str, kind: GoalEventKind, message: String) -> Self {
        Self {
            timestamp: timestamp.to_owned(),
            actor: actor.to_owned(),
            kind,
            message,
        }
    }
}

fn plural(count: u32) -> &'static str {
    if count == 1 { "" } else { "s" }
}
