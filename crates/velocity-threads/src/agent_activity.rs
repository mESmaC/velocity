//! Supervision state machine for SPEC.md §2.2, separate from the native timer.
use agent_client_protocol::v1::schema::SessionUpdate;
use velocity_acp::client::AgentEvent;
use velocity_core::agent_activity::{Availability, StallAction, stall_action};
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_store::{AgentActivity, AgentSettings, ProjectItem, ThreadItem, ThreadState};

/// Minutes a thread may sit idle in `working` with nothing outstanding before a nudge (#516).
pub const IDLE_WORKING_TIMEOUT_MINUTES: u64 = 20;
pub const IDLE_WORKING_TIMEOUT_SECS: u64 = IDLE_WORKING_TIMEOUT_MINUTES * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Nudge,
    IdleNudge,
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
    worked: bool,
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
        if batch.iter().any(is_substantive_work) {
            self.worked = true;
        }
        let checkpoint = batch.is_empty()
            || batch.iter().any(|event| {
                matches!(
                    event,
                    AgentEvent::PromptAccepted
                        | AgentEvent::TurnEnded(_)
                        | AgentEvent::QueuedRunAdded(_)
                        | AgentEvent::QueuedRunUpdated(_)
                        | AgentEvent::QueuedRunFinished(_)
                        | AgentEvent::QueuedRunsDelivered { .. }
                )
            })
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

    pub fn idle_nudge(&mut self, now: u64) {
        self.last_update = Some(now);
        self.worked = false;
    }

    pub fn take_worked(&mut self) -> bool {
        std::mem::take(&mut self.worked)
    }
}

fn is_substantive_work(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::FileWritten { .. }
            | AgentEvent::QueuedRunAdded(_)
            | AgentEvent::QueuedRunUpdated(_)
            | AgentEvent::QueuedRunFinished(_)
            | AgentEvent::Transcript {
                update: SessionUpdate::ToolCall(_),
                ..
            }
    )
}

fn proves_first_prompt_started(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::PromptAccepted
            | AgentEvent::FileWritten { .. }
            | AgentEvent::QueuedRunAdded(_)
            | AgentEvent::QueuedRunUpdated(_)
            | AgentEvent::QueuedRunFinished(_)
            | AgentEvent::Subagent { .. }
            | AgentEvent::SubagentSession { .. }
            | AgentEvent::TurnEnded(Ok(_))
            | AgentEvent::Transcript {
                update: SessionUpdate::AgentMessageChunk(_)
                    | SessionUpdate::AgentThoughtChunk(_)
                    | SessionUpdate::ToolCall(_)
                    | SessionUpdate::ToolCallUpdate(_)
                    | SessionUpdate::Plan(_),
                ..
            }
    )
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
    if proves_first_prompt_started(event) {
        checkpoint.first_prompt_started = true;
    }
    if !matches!(event, AgentEvent::Log(_) | AgentEvent::Queued) {
        checkpoint.last_update = now;
        checkpoint.nudged = false;
    }
    match event {
        AgentEvent::QueuedRunAdded(record) | AgentEvent::QueuedRunUpdated(record) => {
            checkpoint.last_wait = Some(format!("queued run: {}", record.title()));
            checkpoint.idle_nudged = false;
            checkpoint.idle_nudge_turns = None;
        }
        AgentEvent::QueuedRunFinished(_) | AgentEvent::QueuedRunsDelivered { .. } => {
            checkpoint.idle_nudged = false;
            checkpoint.idle_nudge_turns = None;
        }
        AgentEvent::Transcript {
            update: SessionUpdate::ToolCall(call),
            ..
        } if call
            .title
            .starts_with(velocity_core::workflow_dispatch::RUN_TITLE) =>
        {
            checkpoint.last_wait = Some(call.title.clone());
        }
        _ => {}
    }
    if let AgentEvent::TurnEnded(Err(error)) = event
        && velocity_acp::client::subscription_limit_error(error)
    {
        checkpoint.retry_after = Some(provider_retry_after(error.as_str(), now));
        thread.activity = AgentActivity::AtLimit;
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
    decide_with_turn(thread, settings, now, live, queued_build, false)
}

pub fn decide_with_turn(
    thread: &ThreadItem,
    settings: &AgentSettings,
    now: u64,
    live: bool,
    queued_build: bool,
    turn_in_progress: bool,
) -> Action {
    if !crate::parking::can_resume_build_session(thread.state) {
        return Action::Nothing;
    }
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
    if thread.provider_queued || thread.activity == AgentActivity::Parked || unavailable {
        return Action::Nothing;
    }
    if capped(thread, settings, now) {
        return if live { Action::Limit } else { Action::Nothing };
    }
    if idle_working_due(thread, settings, now, queued_build, turn_in_progress) {
        return Action::IdleNudge;
    }
    if !live {
        return Action::Nothing;
    }
    stalled(thread, settings, now, queued_build)
}

/// Whether `thread` is owned by continuous dispatch or works on a GitHub issue (#544).
pub fn is_dispatch_or_issue(thread: &ThreadItem) -> bool {
    thread.started_by_dispatch || thread.issue.is_some()
}

/// Idle working timeout in minutes: dispatch and issue threads use the stall timeout so a
/// stuck autonomous turn does not hold a slot for 20 minutes (#544).
pub fn idle_working_timeout_minutes(thread: &ThreadItem, settings: &AgentSettings) -> u64 {
    if is_dispatch_or_issue(thread) {
        u64::from(settings.stall_timeout_minutes)
    } else {
        IDLE_WORKING_TIMEOUT_MINUTES
    }
}

fn idle_working_due(
    thread: &ThreadItem,
    settings: &AgentSettings,
    now: u64,
    queued_build: bool,
    turn_in_progress: bool,
) -> bool {
    let checkpoint = &thread.supervision;
    let since = checkpoint.last_update.max(checkpoint.started);
    let timeout = idle_working_timeout_minutes(thread, settings) * 60;
    thread.state == ThreadState::Working
        && thread.activity == AgentActivity::Idle
        && thread.interview.is_none()
        && thread.worktree_path.is_some()
        && thread.dependency_wait.is_none()
        && !thread.has_rosen_wait()
        && !thread.startup_recovery_pending
        && !checkpoint.released
        && !checkpoint.idle_nudged
        && thread.queued_runs.is_empty()
        && !thread.queued_run_history.iter().any(|run| !run.delivered)
        && !queued_build
        && !turn_in_progress
        && since > 0
        && now.saturating_sub(since) >= timeout
}

/// Identifies what an idle `working` thread last waited on, for its recovery nudge (#516).
pub fn last_wait_description(thread: &ThreadItem, blocks: &[TranscriptBlock]) -> Option<String> {
    let blocks = if blocks.is_empty() {
        thread.transcript.as_slice()
    } else {
        blocks
    };
    let start = blocks
        .iter()
        .rposition(|block| matches!(block, TranscriptBlock::UserMessage { queued: false, .. }))
        .unwrap_or(0);
    let mut fallback_step = None;
    for block in blocks[start..].iter().rev() {
        let TranscriptBlock::ToolGroup { steps } = block else {
            continue;
        };
        for step in steps.iter().rev() {
            if step.verb == "Worktree" || step.verb == "Workspace" {
                continue;
            }
            if is_wait_step(&step.verb, &step.argument) {
                return Some(format_step_wait(&step.verb, &step.argument));
            }
            if fallback_step.is_none() {
                fallback_step = Some(format_step_wait(&step.verb, &step.argument));
            }
        }
    }
    if let Some(wait) = &thread.supervision.last_wait
        && !wait.trim().is_empty()
    {
        return Some(wait.clone());
    }
    if let Some(wait) = &thread.dependency_wait
        && !wait.trim().is_empty()
    {
        return Some(format!("dependency wait: {wait}"));
    }
    fallback_step
}

fn is_wait_step(verb: &str, argument: &str) -> bool {
    let normalized = argument.replace('\\', "/");
    normalized.contains("rosen run")
        || normalized.contains("build-queue.ps1")
        || normalized.contains("scripts/check.ps1")
        || argument.starts_with(velocity_core::workflow_dispatch::RUN_TITLE)
        || argument.starts_with(velocity_core::workflow_dispatch::ROSEN_CHECK_TITLE)
        || verb.contains("queue_run")
        || argument.contains("queue_run")
        || verb.contains("workflow_run")
        || argument.contains("workflow_run")
        || verb.contains("airc_wait")
        || argument.contains("airc_wait")
        || normalized.contains("rosen wait")
        || verb.contains("rosen__wait")
        || verb.contains("mcp_rosen_wait")
        || argument.contains("rosen__wait")
        || argument.contains("mcp_rosen_wait")
}

fn format_step_wait(verb: &str, argument: &str) -> String {
    if argument.starts_with("rosen run")
        || argument.starts_with(velocity_core::workflow_dispatch::RUN_TITLE)
        || argument.starts_with(velocity_core::workflow_dispatch::ROSEN_CHECK_TITLE)
        || verb.is_empty()
    {
        argument.to_owned()
    } else if argument.is_empty() {
        verb.to_owned()
    } else {
        format!("{verb} · {argument}")
    }
}

/// Prompt delivered when a thread stays idle in `working` with nothing outstanding (#516).
pub fn idle_nudge_prompt(last_wait: Option<&str>) -> String {
    match last_wait {
        Some(wait) => format!(
            "This thread has been idle in working for {IDLE_WORKING_TIMEOUT_MINUTES} minutes \
             with no queued run or turn in progress (last waited on: {wait}). Continue your \
             work, queue any check whose result never arrived, or finish and name your change."
        ),
        None => format!(
            "This thread has been idle in working for {IDLE_WORKING_TIMEOUT_MINUTES} minutes \
             with no queued run or turn in progress. Continue your work, say what is blocking \
             you, or finish and name your change."
        ),
    }
}

/// Prompt delivered when `thread` stays idle in `working`. Dispatch and issue threads get
/// explicit continue instructions for background shell commands and naming (#544).
pub fn idle_nudge_prompt_for(
    thread: &ThreadItem,
    settings: &AgentSettings,
    last_wait: Option<&str>,
) -> String {
    if !is_dispatch_or_issue(thread) {
        return idle_nudge_prompt(last_wait);
    }
    let minutes = idle_working_timeout_minutes(thread, settings);
    match last_wait {
        Some(wait) => format!(
            "This thread has been idle in working for {minutes} minutes with no queued run or \
             turn in progress (last waited on: {wait}). Any commands you started have finished \
             and their output is in the terminal. Check the results, finish the work, and name \
             the change with `rosen commit`, or report a blocker with `issue_comment`."
        ),
        None => format!(
            "This thread has been idle in working for {minutes} minutes with no queued run or \
             turn in progress. Any commands you started have finished and their output is in \
             the terminal. Check the results, finish the work, and name the change with \
             `rosen commit`, or report a blocker with `issue_comment`."
        ),
    }
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
/// `rosen run` is the machine-wide queue every agent is told to use (#308). A GitHub
/// workflow run Velocity waits on for the agent (#508) is the same kind of wait: nudging or
/// restarting the thread would only lose the result.
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
                && (command.contains("build-queue.ps1")
                    || command.contains("scripts/check.ps1")
                    || command.contains("rosen run")
                    || command.starts_with(velocity_core::workflow_dispatch::RUN_TITLE))
        }),
        _ => false,
    })
}

/// Only offer commits known to have been pushed, never an unpushed working tree.
pub fn pushed_commit(thread: &ThreadItem) -> Option<String> {
    let path = thread.worktree_path.as_ref()?;
    // Git for Windows refuses paths over 260 characters without core.longpaths.
    let output = crate::process::hidden_command("git")
        .args([
            "-c",
            "core.longpaths=true",
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

pub fn capped(thread: &ThreadItem, settings: &AgentSettings, now: u64) -> bool {
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
        .filter(|claim| thread.matches_airc_member(&claim.member))
    {
        channel.release_with_note(&claim.member, claim.key(), &text)?;
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

    // Recovery must reset grace, PRs must protect claims, a completed release must not
    // repeat, and an undelivered queued-run result must exempt an idle-working thread from
    // an idle nudge (#536, #666). Exercise the actual supervisor, not just deadline guards.
    #[test]
    fn lean_supervision_trace() -> Result<(), Box<dyn std::error::Error>> {
        let settings = Settings::default().agents;
        let mut thread = ThreadItem::new(1, "Fixture".into());
        let mut trace = "";
        for line in include_str!("../../../proofs/fixtures/agent-activity.tsv").lines() {
            let fields: Vec<_> = line.split('\t').collect();
            match fields[0] {
                "stall" => check_stall(&fields)?,
                "idle_nudge" => check_idle_nudge(&fields, &settings)?,
                "trace" => {
                    if trace != fields[1] {
                        trace = fields[1];
                        thread = ThreadItem::new(1, "Fixture".into());
                    }
                    check_trace_step(&mut thread, trace, line, &fields, &settings)?;
                }
                _ => return Err(format!("Unknown fixture row: {line}").into()),
            }
        }
        Ok(())
    }

    fn check_trace_step(
        thread: &mut ThreadItem,
        trace: &str,
        line: &str,
        fields: &[&str],
        settings: &velocity_store::AgentSettings,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let now = fields[2].parse()?;
        apply_status(thread, fields[3]);
        thread.pull_request_number = fields[4].parse::<bool>()?.then_some(1);
        checkpoint_unavailable(thread, now);
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
        thread.worktree_path = (!setup_failed).then(|| r"C:\velocity-wt\fixture".into());
        let mut project = ProjectItem::new(1, "Fixture".into(), r"C:\fixture".into());
        project.spec_locked = true;
        project.threads.push(thread.clone());
        let supervised = supervised_thread(&project, thread.id)?;
        let action = decide(
            &supervised,
            settings,
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
        Ok(())
    }

    fn check_idle_nudge(
        fields: &[&str],
        settings: &velocity_store::AgentSettings,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use super::{decide_with_turn, idle_working_timeout_minutes};

        let (now, since, timeout): (u64, u64, u64) =
            (fields[2].parse()?, fields[3].parse()?, fields[4].parse()?);
        let status = fields[5];
        let (idle_working, nudged, other_exempt, expected_due): (bool, bool, bool, bool) = (
            fields[6].parse()?,
            fields[7].parse()?,
            fields[8].parse()?,
            fields[10].parse()?,
        );
        let mut thread = ThreadItem::new(1, "Fixture".into());
        apply_status(&mut thread, status);
        if matches!(status, "live" | "queue") && idle_working {
            thread.activity = AgentActivity::Idle;
        }
        thread.worktree_path = Some(r"Z:\velocity-work\velocity\fixture".into());
        thread.started_by_dispatch = timeout == u64::from(settings.stall_timeout_minutes) * 60;
        assert_eq!(
            idle_working_timeout_minutes(&thread, settings) * 60,
            timeout
        );
        thread.supervision.started = since;
        thread.supervision.last_update = since;
        thread.supervision.idle_nudged = nudged;
        if other_exempt {
            thread.dependency_wait = Some("waiting".into());
        }
        thread.queued_run_history = parse_run_history(fields[9])?;
        let queued_build = status == "queue";
        for live in [true, false] {
            let action = decide_with_turn(&thread, settings, now, live, queued_build, false);
            assert_eq!(
                action == Action::IdleNudge,
                expected_due,
                "live={live} {}",
                fields.join("\t")
            );
        }
        Ok(())
    }

    fn parse_run_history(
        encoded: &str,
    ) -> Result<Vec<velocity_core::queued_runs::QueuedRunRecord>, Box<dyn std::error::Error>> {
        use velocity_core::queued_runs::{QueuedRunClass, QueuedRunPhase, QueuedRunRecord};

        if encoded == "-" {
            return Ok(Vec::new());
        }
        encoded
            .split(',')
            .enumerate()
            .map(|(index, entry)| {
                let mut record = QueuedRunRecord::new(
                    format!("velocity-run-{index}"),
                    "cargo".into(),
                    vec!["check".into()],
                    None,
                    QueuedRunClass::Build,
                );
                record.phase = QueuedRunPhase::Completed;
                record.delivered = match entry {
                    "delivered" => true,
                    "undelivered" => false,
                    _ => return Err("Invalid queued_run_history fixture".into()),
                };
                Ok(record)
            })
            .collect()
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

    // Risk (#516): a thread orphaned in working/idle with no queued run and no turn in
    // progress must get a supervision nudge naming what it last waited on, even after a
    // restart when its session is no longer live.
    #[test]
    fn idle_working_thread_gets_supervision_nudge() {
        use super::{IDLE_WORKING_TIMEOUT_SECS, decide_with_turn};
        use velocity_core::queued_runs::{QueuedRunClass, QueuedRunRecord};

        let settings = Settings::default().agents;
        let mut thread = ThreadItem::new(7, "Review tab".into());
        thread.state = ThreadState::Working;
        thread.activity = AgentActivity::Idle;
        thread.worktree_path = Some(r"W:\workspaces\collapse\issue-103".into());
        thread.supervision.started = 1000;
        thread.supervision.last_update = 1000;
        let due = 1000 + IDLE_WORKING_TIMEOUT_SECS;

        assert_eq!(
            decide_with_turn(&thread, &settings, due - 1, false, false, false),
            Action::Nothing
        );
        thread.queued_runs.push(QueuedRunRecord::new(
            "velocity-run-1".into(),
            "npm".into(),
            vec!["run".into(), "check".into()],
            None,
            QueuedRunClass::Build,
        ));
        assert_eq!(
            decide_with_turn(&thread, &settings, due, false, false, false),
            Action::Nothing
        );
        thread.queued_runs.clear();
        assert_eq!(
            decide_with_turn(&thread, &settings, due, true, false, true),
            Action::Nothing
        );
        assert_eq!(
            decide_with_turn(&thread, &settings, due, false, false, false),
            Action::IdleNudge
        );
        assert_eq!(
            decide_with_turn(&thread, &settings, due, true, false, false),
            Action::IdleNudge
        );
        thread.supervision.idle_nudged = true;
        assert_eq!(
            decide_with_turn(
                &thread,
                &settings,
                due + IDLE_WORKING_TIMEOUT_SECS,
                false,
                false,
                false
            ),
            Action::Nothing
        );
    }

    // Risk (#516): an idle supervision nudge must name what the thread last waited on.
    #[test]
    fn idle_nudge_names_last_wait() {
        use super::{idle_nudge_prompt, last_wait_description};
        use velocity_core::thread_view_model::{ToolStepRow, TranscriptBlock};

        let thread = ThreadItem::new(7, "Review tab".into());
        let blocks = vec![TranscriptBlock::ToolGroup {
            steps: vec![ToolStepRow {
                running: false,
                verb: "Bash".into(),
                argument: "rosen run -- npm run check".into(),
                result_text: String::new(),
                tool_call_id: None,
            }],
        }];
        let wait = last_wait_description(&thread, &blocks);
        assert_eq!(wait.as_deref(), Some("rosen run -- npm run check"));
        let prompt = idle_nudge_prompt(wait.as_deref());
        assert!(prompt.contains("rosen run -- npm run check"), "{prompt}");
    }

    // Risk (#544): a dispatch thread that ends its turn idle with unnamed edits must get one
    // continue prompt after the 5-minute stall timeout, while staying exempt during a real
    // queue_run wait or dependency wait.
    #[test]
    fn dispatch_idle_thread_nudges_after_stall_timeout() {
        use super::{decide_with_turn, idle_nudge_prompt_for};
        use velocity_core::queued_runs::{QueuedRunClass, QueuedRunRecord};

        let settings = Settings::default().agents;
        let mut thread = ThreadItem::new(52, "Issue #479".into());
        thread.started_by_dispatch = true;
        thread.state = ThreadState::Working;
        thread.activity = AgentActivity::Idle;
        thread.worktree_path = Some(r"Z:\velocity-work\velocity\issue-479".into());
        thread.supervision.started = 1000;
        thread.supervision.last_update = 1000;
        let due = 1000 + u64::from(settings.stall_timeout_minutes) * 60;

        assert_eq!(
            decide_with_turn(&thread, &settings, due - 1, true, false, false),
            Action::Nothing
        );
        thread.dependency_wait = Some("waiting for maple".into());
        assert_eq!(
            decide_with_turn(&thread, &settings, due, true, false, false),
            Action::Nothing
        );
        thread.dependency_wait = None;
        thread.rosen_overlap_wait = Some(velocity_store::rosen::RosenOverlapWait::from_overlaps(
            &[velocity_core::preflight::PreflightOverlap {
                change: "abcdef1234567890".into(),
                owner: "issue-130-178".into(),
                paths: vec!["design/screens/README.md".into()],
            }],
            10,
        ));
        assert_eq!(
            decide_with_turn(&thread, &settings, due, true, false, false),
            Action::Nothing
        );
        thread.rosen_overlap_wait = None;
        thread.queued_runs.push(QueuedRunRecord::new(
            "velocity-run-1".into(),
            "cargo".into(),
            vec!["check".into()],
            None,
            QueuedRunClass::Build,
        ));
        assert_eq!(
            decide_with_turn(&thread, &settings, due, true, false, false),
            Action::Nothing
        );
        thread.queued_runs.clear();
        assert_eq!(
            decide_with_turn(&thread, &settings, due, true, false, false),
            Action::IdleNudge
        );
        let prompt = idle_nudge_prompt_for(&thread, &settings, Some("rosen run -- cargo check"));
        assert!(prompt.contains("rosen commit"), "{prompt}");
        assert!(prompt.contains("issue_comment"), "{prompt}");
        assert!(prompt.contains("terminal"), "{prompt}");
    }

    // Risk (#544): a shell `rosen run` tool row left running when the turn ends must be
    // closed so `in_build_queue` does not permanently exempt the idle thread from supervision.
    #[test]
    fn turn_end_closes_stale_running_rosen_run_tool_row() {
        use super::in_build_queue;
        use crate::transcript_view::LiveTranscript;
        use agent_client_protocol::v1::schema::{
            SessionUpdate, ToolCall, ToolCallId, ToolCallStatus, ToolKind,
        };
        use velocity_acp::Placement;

        let mut transcript = LiveTranscript::new(r"Z:\velocity-work\velocity\issue-479");
        let call = ToolCall::new(
            ToolCallId::new("call-1"),
            "rosen run -- cargo check -p velocity-app",
        )
        .kind(ToolKind::Execute)
        .status(ToolCallStatus::InProgress);
        transcript.record(Placement::Added(0), &SessionUpdate::ToolCall(call));
        assert!(in_build_queue(&transcript.blocks()));

        transcript.finish_running_tools();
        assert!(!in_build_queue(&transcript.blocks()));
    }

    // Risk (#658): `SessionStarted` and local user-message echoes must not mark a prompt as
    // started, while `PromptAccepted` must checkpoint immediately and retain issue ownership.
    #[test]
    fn prompt_accepted_checkpoints_and_retains_issue_ownership() {
        use super::{Heartbeat, observe};
        use agent_client_protocol::v1::schema::{
            ContentBlock, ContentChunk, SessionId, SessionUpdate,
        };
        use velocity_acp::Placement;
        use velocity_acp::client::AgentEvent;

        let mut heartbeat = Heartbeat::default();
        let mut thread = ThreadItem::new(658, "Issue #658".into());
        thread.started_by_dispatch = true;
        thread.agent_session = Some("opened-then-disconnected".into());
        thread.state = ThreadState::Error;

        let pre_prompt = [
            AgentEvent::SessionStarted {
                session_id: SessionId::new("opened-then-disconnected"),
                loaded: false,
            },
            AgentEvent::Transcript {
                placement: Placement::Added(0),
                update: SessionUpdate::UserMessageChunk(ContentChunk::new(ContentBlock::from(
                    "Implement #658".to_owned(),
                ))),
            },
            AgentEvent::Log("stderr before prompt".into()),
        ];
        assert!(!heartbeat.observe(&pre_prompt, 100));
        for event in &pre_prompt {
            observe(&mut thread, event, 100);
        }
        assert!(!thread.supervision.first_prompt_started);
        assert!(velocity_store::continuous_dispatch::failed_before_first_turn(&thread));

        assert!(heartbeat.observe(&[AgentEvent::PromptAccepted], 101));
        observe(&mut thread, &AgentEvent::PromptAccepted, 101);
        assert!(thread.supervision.first_prompt_started);
        assert!(!velocity_store::continuous_dispatch::failed_before_first_turn(&thread));
    }
}
