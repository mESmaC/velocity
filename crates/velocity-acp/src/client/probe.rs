//! Reading a provider's state from its own agent (F15): whether its login is signed in, the
//! sign-in methods it offers, and the models it lists with which of them have a fast mode.
//! One short session that sends no prompt, so it uses none of the subscription.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{AuthMethod, NewSessionRequest, SessionId};
use agent_client_protocol::{Agent, ByteStreams, Client, ErrorCode};
use tokio::sync::mpsc::unbounded_channel;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::{AgentLaunch, ClientError, Shared, config, forward_log, handlers, process, session};
use crate::session_config::{ConfigValue, model_values, offers_fast_mode};
use crate::{SignInKind, SignInMethod};

/// Agents list a handful of models; a list far longer than that is cut rather than probed
/// one by one.
const MOST_MODELS: usize = 32;

/// One model an agent offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbedModel {
    /// The agent's own value for it.
    pub id: String,
    pub name: String,
    /// Whether the agent offers a fast mode on it (F15 Speed).
    pub fast: bool,
}

/// What the agent reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProbe {
    /// `session/new` succeeded. An agent that offers no sign-in method counts as signed in
    /// once it starts a session (F15 Custom ACP agents).
    pub signed_in: bool,
    pub sign_in_methods: Vec<SignInMethod>,
    pub models: Vec<ProbedModel>,
}

/// Starts the agent in `directory`, reads its state and stops it. The whole probe is bounded
/// by `deadline`; the agent's process tree is stopped either way.
pub async fn probe_agent(
    launch: &AgentLaunch,
    directory: &Path,
    deadline: Duration,
) -> Result<AgentProbe, ClientError> {
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
    let directory = directory.to_path_buf();
    let probe = handlers::register(Client.v1(), &shared)
        .connect_with(transport, async move |connection: ConnectionTo<Agent>| {
            read_state(&connection, directory).await
        });
    let outcome = tokio::time::timeout(deadline, probe).await;
    // Stopping a process tree can take seconds; nobody waits on it.
    tokio::spawn(async move {
        let _ = process::stop_agent(&mut agent).await;
    });
    Ok(outcome.map_err(|_| ClientError::TimedOut(deadline))??)
}

async fn read_state(
    connection: &ConnectionTo<Agent>,
    directory: PathBuf,
) -> Result<AgentProbe, agent_client_protocol::Error> {
    let initialized = connection
        .send_request(session::initialize_request())
        .block_task()
        .await?;
    let sign_in_methods = initialized
        .auth_methods
        .iter()
        .map(sign_in_method)
        .collect();
    let session = match connection
        .send_request(NewSessionRequest::new(directory))
        .block_task()
        .await
    {
        Ok(session) => session,
        Err(error) if error.code == ErrorCode::AUTH_REQUIRED => {
            return Ok(AgentProbe {
                signed_in: false,
                sign_in_methods,
                models: Vec::new(),
            });
        }
        Err(error) => return Err(error),
    };
    let options = session.config_options.unwrap_or_default();
    let models = read_models(connection, &session.session_id, options).await?;
    Ok(AgentProbe {
        signed_in: true,
        sign_in_methods,
        models,
    })
}

/// Selects each listed model in turn, since a model's fast mode shows only while it is the
/// session's model. The session is thrown away afterwards, so nothing is left switched.
async fn read_models(
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
    options: Vec<agent_client_protocol::v1::schema::SessionConfigOption>,
) -> Result<Vec<ProbedModel>, agent_client_protocol::Error> {
    let views = config::option_views(&options);
    let Some((option_id, values)) = model_values(&views) else {
        return Ok(Vec::new());
    };
    let mut models = Vec::new();
    for (id, name) in values.into_iter().take(MOST_MODELS) {
        let fast = match config::set_option(
            connection,
            session_id,
            Arc::clone(&option_id),
            ConfigValue::Select(Arc::clone(&id)),
        )
        .await
        {
            Ok(options) => offers_fast_mode(&config::option_views(&options)),
            // A model the agent lists but won't switch to is still listed, without Fast.
            Err(_) => false,
        };
        models.push(ProbedModel {
            id: id.to_string(),
            name: name.to_string(),
            fast,
        });
    }
    Ok(models)
}

fn sign_in_method(method: &AuthMethod) -> SignInMethod {
    let kind = match method {
        AuthMethod::Terminal(terminal) => SignInKind::Terminal {
            args: terminal.args.clone(),
        },
        _ => SignInKind::Agent,
    };
    SignInMethod {
        id: method.id().0.to_string(),
        name: method.name().to_owned(),
        description: method.description().unwrap_or_default().to_owned(),
        kind,
    }
}
