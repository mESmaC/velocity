//! The Main watcher (SPEC.md F7 Main watcher): when it watches the base branch again, what its
//! agent is told, and what its card and the Duty view's activity feed say about it.
//!
//! One watcher takes each new tip of the base branch. The others stand by, so a merge that
//! lands while a watch runs is taken by the next idle watcher instead of waiting its turn.

use crate::scout_sweep::{SweepRecord, step_doing};

/// How many merges a first watch bisects over when nothing was watched before it.
pub const FIRST_WATCH_DEPTH: usize = 20;

/// Why a watch starts. It heads the watch's prompt and its feed line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchReason {
    /// Nothing was watched before: check the tip as it is.
    First,
    /// The base branch moved past the last watched commit.
    Merged { previous: String },
    /// You instructed the watcher from the Duty view's composer.
    Instruction,
}

/// What decides whether the role starts a watch now.
#[derive(Clone, Copy, Debug)]
pub struct WatchSchedule<'a> {
    pub paused: bool,
    /// The base branch's tip now, when it could be read.
    pub tip: Option<&'a str>,
    /// The most recent watch any of the role's watchers started.
    pub last: Option<&'a SweepRecord>,
    /// The tips running watches are on.
    pub running: &'a [String],
    /// You sent the watcher an instruction it has not had yet.
    pub instructed: bool,
}

/// The watch to start now, and the tip it watches, if any. A paused role starts nothing
/// (F7: pausing finishes in-flight work and starts nothing new). A tip a running watch is
/// already on is never taken twice.
pub fn watch_due(schedule: &WatchSchedule) -> Option<(WatchReason, String)> {
    if schedule.paused {
        return None;
    }
    let tip = schedule.tip?;
    if schedule.running.iter().any(|running| running == tip) {
        return None;
    }
    if schedule.instructed {
        return Some((WatchReason::Instruction, tip.to_owned()));
    }
    let Some(last) = schedule.last else {
        return Some((WatchReason::First, tip.to_owned()));
    };
    match last.base_commit.as_deref() {
        Some(watched) if watched == tip => None,
        Some(watched) => Some((
            WatchReason::Merged {
                previous: watched.to_owned(),
            },
            tip.to_owned(),
        )),
        // The last attempt never read a tip: watch this one from scratch.
        None => Some((WatchReason::First, tip.to_owned())),
    }
}

/// The record of the role's most recent watch: the one that started last.
pub fn latest_watch<'a>(
    records: impl IntoIterator<Item = &'a SweepRecord>,
) -> Option<&'a SweepRecord> {
    records.into_iter().max_by_key(|record| record.started_at)
}

/// Everything a watch's prompt is built from.
#[derive(Clone, Copy, Debug)]
pub struct WatchBrief<'a> {
    pub nickname: &'a str,
    pub reason: &'a WatchReason,
    pub base_branch: &'a str,
    pub tip: &'a str,
    /// What you told the watcher from the Duty view's composer, if this watch follows it.
    pub instruction: Option<&'a str>,
}

/// The watch's prompt: F7's Main watcher duties, with the `broken` event, the PR record and
/// flaky-test issues going through Velocity's MCP tools rather than trusted to the prompt.
pub fn watch_prompt(brief: &WatchBrief) -> String {
    let base = brief.base_branch;
    let mut prompt = format!(
        "You are {}, the main watcher on this project (Velocity duty role: Main watcher). {}\n\nThis worktree is a detached checkout of `{base}` at {}. Pre-merge CI only builds and lints, so this watch is the first time the tests and stress scripts run on the merged code.\n",
        brief.nickname,
        reason_sentence(brief.reason, base),
        short_commit(brief.tip),
    );
    prompt.push_str(WATCH_STEPS);
    prompt.push_str(&bisect_steps(brief));
    prompt.push_str(&repair_steps(base));
    if let Some(instruction) = brief.instruction {
        prompt.push_str(&format!(
            "\nThe user asks the main watcher: {instruction}\n"
        ));
    }
    prompt.push_str("\nEnd your turn with a one-line summary: green, flaky, or broken by which merge and the PR you opened.\n");
    prompt
}

fn reason_sentence(reason: &WatchReason, base: &str) -> String {
    match reason {
        WatchReason::First => format!("Start your first watch of `{base}`."),
        WatchReason::Merged { previous } => format!(
            "`{base}` moved past {}, the last commit watched: check the merges since.",
            short_commit(previous)
        ),
        WatchReason::Instruction => {
            format!("The user instructed the main watcher; watch `{base}` with that in mind.")
        }
    }
}

const WATCH_STEPS: &str = "\nWatch:\n- Read AGENTS.md and SPEC.md first and use the commands AGENTS.md gives.\n- Run every scout test and stress script (the project's test suite and every script in its stress folder, such as `stress/`), then the project's local checks.\n- If everything passes, call `watch_green` and end your turn.\n- A test or script that fails, then passes when you re-run it up to three times on this same commit, is flaky: check `scout_issues`, then file it once with `scout_file` (its reproduction runs it until it fails), and count it as passing.\n";

fn bisect_steps(brief: &WatchBrief) -> String {
    let tip = brief.tip;
    let good = match brief.reason {
        WatchReason::Merged { previous } => previous.clone(),
        WatchReason::First | WatchReason::Instruction => format!("{tip}~{FIRST_WATCH_DEPTH}"),
    };
    format!(
        "\nWhen something fails every time:\n- Bisect the merges: `git bisect start --first-parent {tip} {good}`, run the failing command at each step, and find the first merge that fails. If `{good}` fails too, the break is older: say so and use the oldest failing merge you can reach. End with `git bisect reset`.\n- Call `watch_broken` with that merge commit, its PR number when it has one, the failing command and its output. Velocity posts the `broken` event, and only the agents whose branches contain that merge are told.\n",
    )
}

fn repair_steps(base: &str) -> String {
    format!(
        "\nThen open a fix or a revert as a PR (locked rule: never push to `{base}`; every fix or revert is a PR):\n- A small, clear fix: make it here, push it with `git push origin HEAD:refs/heads/vel/fix-<sha7>`, and open a PR against `{base}` with `gh pr create --base {base} --head vel/fix-<sha7>`. Name the broken merge, its PR and the failing command in the description.\n- Anything else: revert it here with `git revert --no-edit <sha>` (add `-m 1` when the merge commit has two parents), push it to `vel/revert-<sha7>` the same way, and open a revert PR.\n- Call `watch_pull_request` with the PR's number, `fix` or `revert`, and the broken merge commit. The PR goes through PR duty like any other: never approve or merge it yourself.\n- If `gh` can't open a PR because the project has no GitHub repository, leave the pushed branch and name it in your summary. Never push it to `{base}` instead.\n- Never force-push. Never skip, delete or weaken a test or stress script to make it pass.\n"
    )
}

/// The feed line a starting watch posts (F7 activity feed).
pub fn watch_started_message(reason: &WatchReason, base: &str, tip: &str) -> String {
    let tip = short_commit(tip);
    match reason {
        WatchReason::First => format!("Watching {base} at {tip} · first watch"),
        WatchReason::Merged { previous } => format!(
            "Watching {base} at {tip} · merged since {}",
            short_commit(previous)
        ),
        WatchReason::Instruction => {
            format!("Watching {base} at {tip} · following your instruction")
        }
    }
}

/// What a watch found, as the feed reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchVerdict {
    Green,
    Broken {
        commit: String,
        pull_request: Option<u32>,
        command: String,
    },
    Repair {
        number: u32,
        revert: bool,
        commit: String,
    },
}

/// The feed line for one verdict the watcher recorded.
pub fn verdict_message(base: &str, tip: &str, verdict: &WatchVerdict) -> String {
    match verdict {
        WatchVerdict::Green => format!("{base} green at {}", short_commit(tip)),
        WatchVerdict::Broken {
            commit,
            pull_request,
            command,
        } => {
            let merge = match pull_request {
                Some(number) => format!("{} (#{number})", short_commit(commit)),
                None => short_commit(commit).to_owned(),
            };
            step_doing(&format!("{base} broken by {merge} · {command}"))
        }
        WatchVerdict::Repair {
            number,
            revert,
            commit,
        } => format!(
            "Opened {} PR #{number} for {}",
            if *revert { "revert" } else { "fix" },
            short_commit(commit)
        ),
    }
}

/// The feed line a finished watch posts, when its verdicts didn't already say it all.
pub fn watch_finished_message(tip: &str, verdicts: usize, failure: Option<&str>) -> Option<String> {
    match failure {
        Some(failure) => Some(step_doing(&format!(
            "Watch of {} stopped · {failure}",
            short_commit(tip)
        ))),
        None if verdicts == 0 => Some(format!(
            "Watch of {} ended without a verdict",
            short_commit(tip)
        )),
        None => None,
    }
}

/// What an idle watcher's card says.
pub fn watch_idle_doing(paused: bool) -> String {
    if paused {
        "paused".into()
    } else {
        "idle · waiting for the next merge".into()
    }
}

fn short_commit(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}
