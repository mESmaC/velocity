//! The member's unrestricted agent evaluates the goal before Velocity locks any piece.

use std::path::Path;
use std::time::Duration;

use agent_client_protocol::v1::schema::{ContentBlock, SessionUpdate};
use serde::Deserialize;
use velocity_acp::client::{AgentEvent, AgentInput, AgentLaunch, SessionSetup};
use velocity_core::work_pool::WorkPool;
use velocity_store::goal_swarms::GoalAgentSlot;
use velocity_store::{Settings, ThreadItem};

use crate::goal_swarm::GoalSwarmError;

pub struct GoalSelector {
    launch: AgentLaunch,
    setup: SessionSetup,
    prompt: String,
}

#[derive(Deserialize)]
struct Selection {
    piece: SelectedPiece,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SelectedPiece {
    Key(String),
    None(()),
}

impl GoalSelector {
    pub fn new(launch: AgentLaunch, setup: SessionSetup, prompt: String) -> Self {
        Self {
            launch,
            setup,
            prompt,
        }
    }

    pub fn for_member(
        settings: &Settings,
        thread: &ThreadItem,
        slot: &GoalAgentSlot,
        worktree: &Path,
    ) -> Result<Self, GoalSwarmError> {
        let (launch, modes) =
            crate::launch::agent_launch(settings, slot.provider).map_err(selection_error)?;
        let setup = SessionSetup::new(worktree.to_owned(), modes)
            .with_run_settings(crate::thread_run_settings(thread));
        Ok(Self::new(launch, setup, slot.prompt.clone()))
    }

    /// Called on the choosing worker, never while holding the state or AIRC lock.
    pub fn choose(&self, pool: &WorkPool, flags: &[bool]) -> Result<Option<usize>, GoalSwarmError> {
        if !flags.iter().any(|open| *open) {
            return Ok(None);
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(selection_error)?;
        let reply = runtime.block_on(self.ask(selection_prompt(&self.prompt, pool, flags)))?;
        let selection = parse_selection(&reply)?;
        match selection.piece {
            SelectedPiece::None(()) => Ok(None),
            SelectedPiece::Key(key) => pool
                .pieces
                .iter()
                .enumerate()
                .find(|(index, piece)| {
                    flags.get(*index) == Some(&true) && piece.key.claim_piece() == key
                })
                .map(|(index, _)| Some(index))
                .ok_or_else(|| {
                    selection_error("The agent selected a piece outside the available pool")
                }),
        }
    }

    async fn ask(&self, prompt: String) -> Result<String, GoalSwarmError> {
        let (input, inputs) = tokio::sync::mpsc::unbounded_channel();
        input
            .send(AgentInput::Send(vec![ContentBlock::from(prompt)]))
            .map_err(selection_error)?;
        let mut session = crate::spawn_session(self.launch.clone(), self.setup.clone(), inputs);
        let reply =
            tokio::time::timeout(Duration::from_secs(300), collect_reply(&mut session.events))
                .await;
        let _ = input.send(AgentInput::Stop);
        session
            .outcome
            .await
            .map_err(selection_error)?
            .map_err(selection_error)?;
        reply.map_err(selection_error)?.map_err(selection_error)
    }
}

async fn collect_reply(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
) -> Result<String, String> {
    let mut reply = String::new();
    while let Some(event) = events.recv().await {
        match event {
            AgentEvent::Transcript {
                update: SessionUpdate::AgentMessageChunk(chunk),
                ..
            } => {
                if let ContentBlock::Text(text) = chunk.content {
                    reply.push_str(&text.text);
                }
            }
            AgentEvent::TurnEnded(result) => return result.map(|_| reply),
            _ => {}
        }
    }
    Err("The goal selection session ended without a choice".into())
}

fn selection_prompt(goal: &str, pool: &WorkPool, flags: &[bool]) -> String {
    let candidates: Vec<_> = pool.pieces.iter().zip(flags).filter(|(_, open)| **open)
        .map(|(piece, _)| serde_json::json!({
            "piece": piece.key.claim_piece(), "title": piece.title, "reference": piece.reference,
        })).collect();
    format!(
        "{goal}\n\n---\nVelocity goal swarm: choosing work, before claiming (F16).\n\
         Read the user's goal above first. It governs which work to choose. Read AGENTS.md and the project spec, \
         and inspect issue bodies/comments or referenced files as needed to choose work that satisfies it.\n\
         This turn is only for choosing: do not claim, edit, implement or name work yet. \
         Velocity will acquire the atomic claim and send the implementation prompt after your choice.\n\
         Choose only from this currently unclaimed inventory, already filtered to the user's work sources. \
         If another member takes your choice, you will be asked again with a refreshed inventory. \
         If none fits the user's goal, choose null. Never substitute unrelated work.\n\
         Return only a JSON object {{\"piece\":\"<exact piece key>\"}} or {{\"piece\":null}}.\n\
         Inventory: {}",
        serde_json::Value::Array(candidates)
    )
}

// Providers can emit introductory commentary before their final JSON answer.
fn parse_selection(reply: &str) -> Result<Selection, GoalSwarmError> {
    reply
        .char_indices()
        .rev()
        .filter(|(_, character)| *character == '{')
        .find_map(|(index, _)| {
            serde_json::Deserializer::from_str(&reply[index..])
                .into_iter::<Selection>()
                .next()
                .and_then(Result::ok)
        })
        .ok_or_else(|| selection_error("The agent did not return a goal work choice"))
}

fn selection_error(error: impl std::fmt::Display) -> GoalSwarmError {
    GoalSwarmError::Selection(error.to_string())
}
