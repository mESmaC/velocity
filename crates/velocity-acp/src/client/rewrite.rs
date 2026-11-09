//! A single F11 rewrite turn, using the ordinary unrestricted ACP client handlers.
use super::{
    AgentEvent, AgentLaunch, ClientError, SessionSetup, Shared, handlers, process, session,
    terminals::Terminals,
};
use crate::Transcript;
use agent_client_protocol::v1::{
    ConnectionTo,
    schema::{ContentBlock, PromptRequest, SessionUpdate},
};
use agent_client_protocol::{Agent, ByteStreams, Client};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::unbounded_channel;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[derive(Debug, thiserror::Error)]
pub enum RewriteError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("Rewrite timed out; your wording is kept")]
    Timeout,
}

pub async fn rewrite(
    mut launch: AgentLaunch,
    setup: SessionSetup,
    prompt: String,
) -> Result<String, RewriteError> {
    // The Claude SDK reads its model from the inherited launch environment, including
    // adapters predating ACP config options. Credentials remain in its OAuth store.
    launch.env.push(("ANTHROPIC_MODEL".into(), "haiku".into()));
    let mut agent = process::spawn_agent(&launch, &setup.worktree)?;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        request(&mut agent, setup, prompt),
    )
    .await;
    process::stop_agent(&mut agent)
        .await
        .map_err(ClientError::from)?;
    result
        .map_err(|_| RewriteError::Timeout)?
        .map_err(RewriteError::from)
}

async fn request(
    agent: &mut process::AgentProcess,
    setup: SessionSetup,
    prompt: String,
) -> Result<String, ClientError> {
    let stdin = agent
        .stdin()
        .take()
        .ok_or(ClientError::MissingPipe("stdin"))?;
    let stdout = agent
        .stdout()
        .take()
        .ok_or(ClientError::MissingPipe("stdout"))?;
    let (events, mut updates) = unbounded_channel();
    if let Some(stderr) = agent.stderr().take() {
        super::forward_log(stderr, events.clone());
    }
    let shared = Arc::new(Shared {
        worktree: setup.worktree.clone(),
        terminals: Terminals::new(setup.worktree.clone()),
        transcript: Mutex::new(Transcript::new()),
        events,
    });
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
    let result = handlers::register(Client.v1(), &shared)
        .connect_with(transport, async move |connection: ConnectionTo<Agent>| {
            let identifier = session::start_session(&connection, &setup).await?;
            connection
                .send_request(PromptRequest::new(
                    identifier,
                    vec![ContentBlock::from(prompt)],
                ))
                .block_task()
                .await?;
            let mut response = String::new();
            while let Ok(event) = updates.try_recv() {
                if let AgentEvent::Transcript {
                    update: SessionUpdate::AgentMessageChunk(chunk),
                    ..
                } = event
                    && let ContentBlock::Text(text) = chunk.content
                {
                    response.push_str(&text.text);
                }
            }
            Ok(response)
        })
        .await
        .map_err(ClientError::from);
    shared.terminals.stop_all().await?;
    result
}
