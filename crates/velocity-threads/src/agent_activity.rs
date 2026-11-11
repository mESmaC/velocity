//! Supervision state machine for SPEC.md §2.2, separate from the native timer.
use velocity_acp::client::AgentEvent;
use velocity_core::agent_activity::{Availability, StallAction, stall_action};
use velocity_store::{AgentActivity, AgentSettings, ProjectItem, ThreadItem, ThreadState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Nudge,
    Restart,
    Limit,
    Release,
    Resume,
}

/// Live ACP silence clock. Only durable transitions request a store checkpoint.
#[derive(Default)]
pub struct Heartbeat {
    last_update: Option<u64>,
    nudged: bool,
}

impl Heartbeat {
    pub fn last_update(&self) -> Option<u64> {
        self.last_update
    }

    pub fn observe(&mut self, batch: &[AgentEvent], now: u64) -> bool {
        let active = batch.is_empty()
            || batch
                .iter()
                .any(|event| !matches!(event, AgentEvent::Log(_) | AgentEvent::Queued));
        let checkpoint = batch.is_empty()
            || batch
                .iter()
                .any(|event| matches!(event, AgentEvent::TurnEnded(_)))
            || (active && self.nudged);
        if active {
            self.last_update = Some(now);
            self.nudged = false;
        }
        checkpoint
    }

    pub fn nudge(&mut self, now: u64) {
        self.last_update = Some(now);
        self.nudged = true;
    }
}

pub fn observe(thread: &mut ThreadItem, event: &AgentEvent, now: u64) {
    if let AgentEvent::ProviderQueued(queued, _) = event {
        // Provider queue time is not time spent running this agent.
        if thread.provider_queued && !queued {
            thread.supervision.started = now;
        }
        thread.provider_queued = *queued;
    }
    let checkpoint = &mut thread.supervision;
    if checkpoint.started == 0 {
        checkpoint.started = now;
    }
    if !matches!(event, AgentEvent::Log(_) | AgentEvent::Queued) {
        checkpoint.last_update = now;
        checkpoint.nudged = false;
    }
    if let AgentEvent::TurnEnded(Err(error)) = event {
        let normalized = error.to_lowercase();
        if normalized.contains("usage limit")
            || normalized.contains("subscription limit")
            || normalized.contains("quota exceeded")
        {
            checkpoint.retry_after = Some(provider_retry_after(error.as_str(), now));
            thread.activity = AgentActivity::AtLimit;
        }
    }
    if matches!(event, AgentEvent::TurnEnded(_)) {
        checkpoint.turns = checkpoint.turns.saturating_add(1);
    }
}

/// The thread the supervisor decides for. Unlike follow-through it never requires a
/// worktree: setup can fail before one exists, and §2.2 still releases that thread's
/// claims once grace expires (#212). Restart and resume check the worktree themselves.
pub fn supervised_thread(project: &ProjectItem, thread: u32) -> Result<ThreadItem, &'static str> {
    if !project.spec_locked {
        return Err("This project's spec must be locked before its agent starts");
    }
    project
        .threads
        .iter()
        .find(|item| item.id == thread)
        .cloned()
        .ok_or("Missing supervision thread")
}

pub fn decide(
    thread: &ThreadItem,
    settings: &AgentSettings,
    now: u64,
    live: bool,
    queued_build: bool,
) -> Action {
    let checkpoint = &thread.supervision;
    let unavailable =
        thread.state == ThreadState::Error || thread.activity == AgentActivity::AtLimit;
    if availability(thread).release_due(
        checkpoint.unavailable_since,
        now,
        u64::from(settings.claim_release_grace_minutes) * 60,
        thread.pull_request_number.is_some() || thread.state == ThreadState::Closed,
        checkpoint.released,
    ) {
        return Action::Release;
    }
    if thread.activity == AgentActivity::AtLimit {
        if live {
            return Action::Limit;
        }
        if !capped(thread, settings, now) && checkpoint.retry_after.is_none_or(|retry| now >= retry)
        {
            return Action::Resume;
        }
    }
    if !live || thread.provider_queued || thread.activity == AgentActivity::Parked || unavailable {
        return Action::Nothing;
    }
    if capped(thread, settings, now) {
        return Action::Limit;
    }
    stalled(thread, settings, now, queued_build)
}

fn stalled(thread: &ThreadItem, settings: &AgentSettings, now: u64, queued_build: bool) -> Action {
    let checkpoint = &thread.supervision;
    let timeout = if thread.activity == AgentActivity::Executing {
        u64::from(settings.tool_timeout_minutes) * 60
    } else {
        u64::from(settings.stall_timeout_minutes) * 60
    };
    match stall_action(
        now.saturating_sub(checkpoint.last_update.max(checkpoint.started)),
        timeout,
        checkpoint.nudged,
        queued_build || thread.activity == AgentActivity::Idle || checkpoint.started == 0,
    ) {
        StallAction::Nothing => Action::Nothing,
        StallAction::Nudge => Action::Nudge,
        StallAction::Restart => Action::Restart,
    }
}

/// Tool rows retain the queue command while it waits and runs; neither counts as silence.
pub fn in_build_queue(blocks: &[velocity_core::thread_view_model::TranscriptBlock]) -> bool {
    use velocity_core::thread_view_model::TranscriptBlock;
    let start = blocks
        .iter()
        .rposition(|block| matches!(block, TranscriptBlock::UserMessage { queued: false, .. }))
        .unwrap_or(0);
    blocks[start..].iter().any(|block| match block {
        TranscriptBlock::ToolGroup { steps } => steps.iter().any(|step| {
            let command = step.argument.replace('\\', "/");
            step.running
                && (command.contains("build-queue.ps1") || command.contains("scripts/check.ps1"))
        }),
        _ => false,
    })
}

/// Only offer commits known to have been pushed, never an unpushed working tree.
pub fn pushed_commit(thread: &ThreadItem) -> Option<String> {
    let path = thread.worktree_path.as_ref()?;
    let output = crate::process::hidden_command("git")
        .args([
            "-C",
            path,
            "rev-parse",
            "--verify",
            &format!("refs/remotes/origin/{}", thread.branch_name),
        ])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn capped(thread: &ThreadItem, settings: &AgentSettings, now: u64) -> bool {
    settings
        .max_turns
        .is_some_and(|limit| thread.supervision.turns >= limit)
        || settings.max_duration_minutes.is_some_and(|limit| {
            thread.supervision.started != 0
                && now.saturating_sub(thread.supervision.started) >= u64::from(limit) * 60
        })
}

/// Describe the cause before shutdown changes the activity to `AtLimit` for both causes.
pub fn limit_message(thread: &ThreadItem, settings: &AgentSettings) -> String {
    let cause = if thread.activity == AgentActivity::AtLimit
        && let Some(retry_after) = thread.supervision.retry_after
    {
        let retry = i64::try_from(retry_after)
            .ok()
            .and_then(|epoch| chrono::DateTime::from_timestamp(epoch, 0))
            .map(|time| time.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| format!("Unix timestamp {retry_after}"));
        format!(
            "Provider {} usage limit reached; retry at {retry}",
            thread.provider.key()
        )
    } else if settings
        .max_turns
        .is_some_and(|limit| thread.supervision.turns >= limit)
    {
        "Per-agent max turns cap reached".into()
    } else {
        "Per-agent max duration cap reached".into()
    };
    format!("{cause}; session stopped and worktree kept")
}

pub fn begin(thread: &mut ThreadItem, now: u64) {
    if thread.activity != AgentActivity::AtLimit {
        thread.activity = AgentActivity::Thinking;
    }
    if thread.supervision.started == 0 {
        thread.supervision.started = now;
    }
    thread.supervision.last_update = now;
    thread.supervision.nudged = false;
}

fn availability(thread: &ThreadItem) -> Availability {
    Availability::from_status(
        thread.state == ThreadState::Error,
        thread.activity == AgentActivity::AtLimit,
    )
}

pub fn checkpoint_unavailable(thread: &mut ThreadItem, now: u64) {
    thread.supervision.unavailable_since =
        availability(thread).checkpoint(thread.supervision.unavailable_since, now);
}

pub fn release_claims(
    channel: &velocity_airc::Channel,
    thread: &ThreadItem,
    pushed: Option<&str>,
) -> Result<(), velocity_airc::Error> {
    let member = format!("thread-{}", thread.id);
    let text = match pushed {
        Some(commit) => format!(
            "Grace period expired; optional prior work on {} at {commit}",
            thread.branch_name
        ),
        None if thread.worktree_path.is_none() => {
            "Grace period expired; setup left no worktree".into()
        }
        None => "Grace period expired; worktree kept".into(),
    };
    for claim in channel
        .claims()?
        .into_iter()
        .filter(|claim| claim.member == member)
    {
        channel.release_with_note(&member, claim.key(), &text)?;
    }
    Ok(())
}

/// ACP prompt failures retain the provider's error text. Prefer an explicit epoch or
/// RFC3339 reset; relative Retry-After values are seconds. Unknown reports retry later.
fn provider_retry_after(error: &str, now: u64) -> u64 {
    let tokens: Vec<_> = error
        .split(|character: char| {
            character.is_whitespace() || matches!(character, '"' | '\'' | '{' | '}' | ',' | '=')
        })
        .filter(|token| !token.is_empty())
        .collect();
    for pair in tokens.windows(2) {
        let key = pair[0].trim_end_matches(':').to_ascii_lowercase();
        let value = pair[1].trim_start_matches(':');
        if matches!(
            key.as_str(),
            "reset_at" | "resetat" | "resets_at" | "resetstime" | "resetsat"
        ) {
            if let Ok(epoch) = value.parse::<u64>() {
                return epoch.max(now);
            }
            if let Ok(reset) = chrono::DateTime::parse_from_rfc3339(value)
                && let Ok(epoch) = u64::try_from(reset.timestamp())
            {
                return epoch.max(now);
            }
        }
        if matches!(key.as_str(), "retry_after" | "retry-after" | "retryafter")
            && let Ok(seconds) = value.parse::<u64>()
        {
            return now.saturating_add(seconds);
        }
    }
    now.saturating_add(15 * 60)
}

#[cfg(test)]
mod trace_fixtures {
    use super::{Action, checkpoint_unavailable, decide, supervised_thread};
    use velocity_core::agent_activity::{StallAction, stall_action};
    use velocity_store::{AgentActivity, ProjectItem, Settings, ThreadItem, ThreadState};

    // Recovery must reset grace, PRs must protect claims, and a completed release
    // must not repeat. Exercise the actual supervisor, not just deadline guards.
    #[test]
    fn lean_supervision_trace() -> Result<(), Box<dyn std::error::Error>> {
        let settings = Settings::default().agents;
        let mut thread = ThreadItem::new(1, "Fixture".into());
        let mut trace = "";
        for line in include_str!("../../../proofs/fixtures/agent-activity.tsv").lines() {
            let fields: Vec<_> = line.split('\t').collect();
            if fields[0] == "stall" {
                check_stall(&fields)?;
                continue;
            }
            if trace != fields[1] {
                trace = fields[1];
                thread = ThreadItem::new(1, "Fixture".into());
            }
            let now = fields[2].parse()?;
            apply_status(&mut thread, fields[3]);
            thread.pull_request_number = fields[4].parse::<bool>()?.then_some(1);
            checkpoint_unavailable(&mut thread, now);
            let expected_since = match fields[5] {
                "none" => None,
                value => Some(value.parse()?),
            };
            assert_eq!(
                thread.supervision.unavailable_since, expected_since,
                "{line}"
            );
            // The "setup" trace failed before a worktree or session existed (#212):
            // decide through the supervisor's own lookup, as the native timer does.
            let setup_failed = trace == "setup";
            thread.worktree_path = (!setup_failed).then(|| r"C:elocity-wtixture".into());
            let mut project = ProjectItem::new(1, "Fixture".into(), r"C:ixture".into());
            project.spec_locked = true;
            project.threads.push(thread.clone());
            let supervised = supervised_thread(&project, thread.id)?;
            let action = decide(
                &supervised,
                &settings,
                now,
                !setup_failed,
                fields[3] == "queue",
            );
            assert_eq!(
                action == Action::Release,
                fields[6].parse::<bool>()?,
                "{line}"
            );
            if action == Action::Release {
                thread.supervision.released = true;
            }
            assert_eq!(
                thread.supervision.released,
                fields[7].parse::<bool>()?,
                "{line}"
            );
        }
        Ok(())
    }

    fn apply_status(thread: &mut ThreadItem, status: &str) {
        thread.state = match status {
            "failed" => ThreadState::Error,
            "closed" => ThreadState::Closed,
            _ => ThreadState::Working,
        };
        thread.activity = match status {
            "limit" => AgentActivity::AtLimit,
            "parked" => AgentActivity::Parked,
            _ => AgentActivity::Thinking,
        };
    }

    fn check_stall(fields: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
        let expected = match fields[5] {
            "nothing" => StallAction::Nothing,
            "nudge" => StallAction::Nudge,
            "restart" => StallAction::Restart,
            _ => return Err("Invalid stall fixture".into()),
        };
        assert_eq!(
            stall_action(
                fields[1].parse()?,
                fields[2].parse()?,
                fields[3].parse()?,
                fields[4].parse()?
            ),
            expected
        );
        Ok(())
    }
}
