//! Signing in through the agent's own login (F15 Sign in, §3.2): an ACP `authenticate`, or
//! the agent's terminal login run by Velocity. Either one opens the provider's OAuth page in
//! your browser; the tokens land in the agent's own credential store, never in Velocity.
//! Dropping the future cancels the sign-in and stops whatever it started.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::AuthenticateRequest;
use agent_client_protocol::{Agent, ByteStreams, Client};
use tokio::process::Command;
use tokio::sync::mpsc::unbounded_channel;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::program_path::find_program;
use super::{AgentLaunch, ClientError, Shared, forward_log, handlers, process, session};
use crate::{SignInKind, SignInMethod};

/// Runs `method` and returns when the login finished, failed, or `deadline` passed.
pub async fn sign_in(
    launch: &AgentLaunch,
    directory: &Path,
    method: &SignInMethod,
    deadline: Duration,
) -> Result<(), ClientError> {
    let login = async {
        match &method.kind {
            SignInKind::Terminal { args } => terminal_login(launch, directory, args).await,
            SignInKind::Agent => agent_login(launch, directory, &method.id).await,
        }
    };
    tokio::time::timeout(deadline, login)
        .await
        .map_err(|_| ClientError::TimedOut(deadline))?
}

/// The agent's own program with the method's arguments, with no console window: the login
/// opens the browser and waits for its callback.
async fn terminal_login(
    launch: &AgentLaunch,
    directory: &Path,
    args: &[String],
) -> Result<(), ClientError> {
    let mut command = Command::new(find_program(launch.program.as_os_str()));
    command
        .args(args)
        .envs(launch.env.iter().map(|(name, value)| (name, value)))
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut login = process::spawn_owned_command(command).map_err(|source| ClientError::Spawn {
        program: launch.program.display().to_string(),
        source,
    })?;
    let status = login.wait().await?;
    if status.success() {
        Ok(())
    } else {
        Err(ClientError::SignIn(format!(
            "the login exited with {status}"
        )))
    }
}

async fn agent_login(
    launch: &AgentLaunch,
    directory: &Path,
    method_id: &str,
) -> Result<(), ClientError> {
    let mut agent = process::spawn_agent(launch, directory)?;
    let stdin = agent
        .stdin()
        .take()
        .ok_or(ClientError::MissingPipe("stdin"))?;
    let stdout = agent
        .stdout()
        .take()
        .ok_or(ClientError::MissingPipe("stdout"))?;
    let (events, _received) = unbounded_channel();
    if let Some(stderr) = agent.stderr().take() {
        forward_log(stderr, events.clone());
    }
    let shared = Arc::new(Shared::new(directory.to_path_buf(), events));
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
    let method_id = method_id.to_owned();
    let result = handlers::register(Client.v1(), &shared)
        .connect_with(transport, async move |connection: ConnectionTo<Agent>| {
            connection
                .send_request(session::initialize_request())
                .block_task()
                .await?;
            connection
                .send_request(AuthenticateRequest::new(method_id))
                .block_task()
                .await?;
            Ok(())
        })
        .await;
    let stopped = process::stop_agent(&mut agent).await;
    result?;
    stopped?;
    Ok(())
}
