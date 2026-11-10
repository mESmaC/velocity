//! Supervision state machine for SPEC.md §2.2, separate from the native timer.
use velocity_acp::client::AgentEvent;
use velocity_core::agent_activity::{StallAction, release_due, stall_action};
use velocity_store::{AgentActivity, AgentSettings, ThreadItem, ThreadState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Nudge,
    Restart,
    Limit,
    Release,
    Resume,
}

pub fn observe(thread: &mut ThreadItem, event: &AgentEvent, now: u64) {
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
    if release_due(
        now.saturating_sub(checkpoint.unavailable_since.unwrap_or(now)),
        u64::from(settings.claim_release_grace_minutes) * 60,
        unavailable && !checkpoint.released,
        thread.pull_request_number.is_some() || thread.state == ThreadState::Closed,
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
    if !live || thread.activity == AgentActivity::Parked || unavailable {
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
    let output = std::process::Command::new("git")
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

pub fn begin(thread: &mut ThreadItem, now: u64) {
    if thread.activity != AgentActivity::AtLimit {
        thread.activity = AgentActivity::Thinking;
    }
    if thread.supervision.started == 0 {
        thread.supervision.started = now;
    }
    thread.supervision.last_update = now;
}

pub fn checkpoint_unavailable(thread: &mut ThreadItem, now: u64) {
    if thread.state == ThreadState::Error || thread.activity == AgentActivity::AtLimit {
        thread.supervision.unavailable_since.get_or_insert(now);
    } else {
        thread.supervision.unavailable_since = None;
    }
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
