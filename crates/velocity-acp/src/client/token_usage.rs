use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use agent_client_protocol::v1::schema::{ContentBlock, SessionUpdate, Usage};
use velocity_graph::token_usage::{Call, Counts, Input, Role, Subject, record};
use velocity_graph::usage::TokenCounts;

use super::airc::airc_project;
use super::provider_usage_log::ProviderLog;
use super::token_usage_context::{AcpContext, message_source};
use super::{AgentLaunch, SessionSetup, Shared, turn_usage};
use crate::Provider;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Ledger {
    pub data: PathBuf,
    pub project: u32,
    pub subject: Subject,
}

impl Ledger {
    pub fn for_setup(setup: &SessionSetup) -> Option<Self> {
        let (airc, data, project) = airc_project(setup.airc.as_ref()?)?;
        let member = airc.args.get(3)?;
        let role = if setup.review_usage {
            Role::Review
        } else if member.starts_with("duty-") {
            Role::Duty
        } else {
            Role::Implementation
        };
        let thread = member
            .strip_prefix("thread-")
            .or_else(|| member.strip_prefix("spec-"))
            .and_then(|value| value.parse().ok());
        Some(Self {
            data: PathBuf::from(data),
            project: project.parse().ok()?,
            subject: Subject { role, thread },
        })
    }
}

#[derive(Default)]
pub(super) struct Capture {
    active: bool,
    context: AcpContext,
    calls: Vec<Call>,
    seen: HashSet<String>,
    cumulative_response: bool,
    last_response: Option<TokenCounts>,
}

impl Capture {
    pub(super) fn open(&mut self, worktree: &Path, instructions: usize, launch: &AgentLaunch) {
        self.active = true;
        // These adapters report the current prompt; the ACP schema's default is session totals.
        self.cumulative_response = !matches!(
            launch.own_config.or(launch.concurrency_provider),
            Some(Provider::Codex | Provider::Anthropic)
        );
        let instructions = if instructions == 0 {
            ["AGENTS.md", "CLAUDE.md"]
                .iter()
                .filter_map(|name| std::fs::metadata(worktree.join(name)).ok())
                .map(|file| file.len() as usize)
                .sum()
        } else {
            instructions
        };
        self.context
            .context
            .replace(Input::ProjectInstructions, instructions);
    }

    pub(super) fn prompt(&mut self, prompt: &[ContentBlock]) {
        if self.active {
            for block in prompt {
                if let ContentBlock::Text(text) = block {
                    self.context
                        .context
                        .add(message_source(&text.text), text.text.len());
                }
            }
        }
    }

    pub(super) fn observe(&mut self, update: &SessionUpdate) {
        if !self.active {
            return;
        }
        if let SessionUpdate::UsageUpdate(update) = update
            && let Some(metadata) = &update.meta
            && let Some(usage) = metadata.get("usage")
            && let Ok(usage) = serde_json::from_value::<Usage>(usage.clone())
        {
            // ACP's used/size describe occupancy. Count only an explicit token payload.
            if let Some(id) = metadata
                .get("responseId")
                .and_then(serde_json::Value::as_str)
                && !self.seen.insert(id.to_owned())
            {
                return;
            }
            self.calls.push(self.call(turn_usage::counts(&usage)));
        }
        self.context.observe(update);
    }

    pub(super) fn finish(&mut self, usage: Option<&Usage>) -> (Vec<Call>, Option<Call>) {
        let total = usage.map(turn_usage::counts).map(|running| {
            if self.cumulative_response {
                let previous = self.last_response.replace(running).unwrap_or_default();
                running.since(previous)
            } else {
                running
            }
        });
        (
            std::mem::take(&mut self.calls),
            total.map(|counts| self.call(counts)),
        )
    }

    fn call(&self, usage: TokenCounts) -> Call {
        // The ACP breakdown lists uncached input, cache writes and cache reads separately.
        let counts = Counts {
            input: usage
                .input
                .saturating_add(usage.cached_read)
                .saturating_add(usage.cached_write),
            cached: usage.cached_read,
            output: usage.output,
            ..Counts::default()
        };
        Call {
            timestamp: now() / 1000,
            counts: self.context.snapshot().attribute(counts),
        }
    }
}

pub(super) struct SessionMeter {
    ledger: Ledger,
    shared: Arc<Shared>,
    log: Option<ProviderLog>,
    since: i64,
}

impl SessionMeter {
    pub(super) fn new(
        setup: &SessionSetup,
        launch: &AgentLaunch,
        session: &str,
        shared: &Arc<Shared>,
        since: i64,
    ) -> Option<Self> {
        let ledger = Ledger::for_setup(setup)?;
        let instructions = setup.instruction_bytes();
        shared
            .token_capture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .open(&setup.worktree, instructions, launch);
        Some(Self {
            ledger,
            shared: shared.clone(),
            log: log(launch, session, instructions),
            since,
        })
    }

    pub(super) async fn finish(&mut self, response: Option<&Usage>) {
        finish(
            &self.ledger,
            &self.shared.token_capture,
            response,
            &mut self.log,
            &mut self.since,
        )
        .await;
    }
}

impl Drop for SessionMeter {
    fn drop(&mut self) {
        let (calls, _) = self
            .shared
            .token_capture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .finish(None);
        let mut log = self.log.take();
        if calls.is_empty() && log.is_none() {
            return;
        }
        let ledger = self.ledger.clone();
        let since = self.since;
        tokio::task::spawn_blocking(move || {
            let log_calls = log.as_mut().map(|log| log.read(since)).unwrap_or_default();
            ledger.save(if calls.is_empty() { log_calls } else { calls });
        });
    }
}

pub(super) async fn finish(
    ledger: &Ledger,
    capture: &Mutex<Capture>,
    response: Option<&Usage>,
    log: &mut Option<ProviderLog>,
    since: &mut i64,
) {
    let (calls, aggregate) = capture
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .finish(response);
    let ledger = ledger.clone();
    let mut reader = log.take();
    let lower_boundary = *since;
    let turn_end = now();
    let result = tokio::task::spawn_blocking(move || {
        let log_calls = reader
            .as_mut()
            .map(|log| log.read(lower_boundary))
            .unwrap_or_default();
        let (calls, may_overlap) = if !calls.is_empty() {
            (calls, true)
        } else if !log_calls.is_empty() {
            (log_calls, false)
        } else {
            let calls: Vec<_> = aggregate
                .into_iter()
                .filter(|call| call.counts.input > 0 || call.counts.output > 0)
                .collect();
            let may_overlap = !calls.is_empty();
            (calls, may_overlap)
        };
        ledger.save(calls);
        (reader, may_overlap)
    })
    .await;
    if let Ok((reader, may_overlap)) = result {
        *log = reader;
        // A provider log can flush after the ACP result. Do not import the same
        // turn again after using explicit ACP counts for it.
        if may_overlap {
            *since = turn_end;
        }
    }
}

impl Ledger {
    fn save(&self, calls: Vec<Call>) {
        if !calls.is_empty()
            && let Err(error) = record(&self.data, self.project, self.subject, calls)
        {
            eprintln!("Could not save token usage: {error}");
        }
    }
}

pub(super) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub(super) fn log(launch: &AgentLaunch, session: &str, instructions: usize) -> Option<ProviderLog> {
    ProviderLog::new(launch, session, instructions)
}
