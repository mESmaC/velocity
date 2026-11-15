//! Headless dispatch host: durable jobs, one Rosen credential, ordinary ACP sessions.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_client_protocol::v1::schema::{ContentBlock, EnvVariable, McpServer, McpServerStdio};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::unbounded_channel;
use velocity_acp::client::{
    AgentEvent, AgentInput, AgentLaunch, SessionResume, SessionSetup, run_session,
};
use velocity_rosen::machine_dispatch::{self, Job};
use velocity_rosen::machines::{self, Role};
use velocity_rosen::{ProjectServer, Watches};

type Failure = Box<dyn std::error::Error + Send + Sync>;

#[derive(Serialize, Deserialize)]
struct SavedJob {
    sequence: u64,
    job: Job,
    #[serde(default)]
    change: Option<String>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    reported: bool,
}

impl SavedJob {
    fn new(sequence: u64, job: Job) -> Self {
        Self {
            sequence,
            job,
            change: None,
            session: None,
            result: None,
            reported: false,
        }
    }
}

/// Joins once, then the same command runs the worker. The enrollment secret is never
/// saved or printed; restarting uses `machine run` and the OS-stored credential.
pub async fn enroll(app_data: &Path, key: &str, name: &str) -> Result<(), Failure> {
    let server = ProjectServer::new(app_data, 0);
    let path = server.data_dir().join("velocity-machine-id");
    if path.exists() {
        return Err("Already enrolled; use machine run".into());
    }
    let machine = machines::enroll(&server, key, name).await?;
    std::fs::write(path, machine)?;
    Ok(())
}

pub async fn run(app_data: PathBuf, slots: usize) -> Result<(), Failure> {
    if slots == 0 {
        return Err("--slots must be positive".into());
    }
    let server = ProjectServer::new(&app_data, 0);
    let machine = std::fs::read_to_string(server.data_dir().join("velocity-machine-id"))?;
    let directory = app_data.join("dispatch");
    std::fs::create_dir_all(&directory)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join("worker.lock"))?;
    lock.try_lock()
        .map_err(|_| "A dispatch worker already owns this data directory")?;
    worker_loop(server, machine, app_data, slots).await
}

async fn worker_loop(
    server: ProjectServer,
    machine: String,
    app_data: PathBuf,
    slots: usize,
) -> Result<(), Failure> {
    let mut tasks = tokio::task::JoinSet::new();
    let mut running = BTreeSet::new();
    let mut role = None;
    let mut cursor = 0;
    loop {
        let tick = tokio::time::timeout(Duration::from_secs(15), async {
            if role.is_none() {
                role = Some(machine_dispatch::register(&server, &machine).await?);
            }
            receive(&server, &machine, &app_data, &mut cursor).await?;
            if let Some(role) = role {
                schedule(
                    &server,
                    &machine,
                    &app_data,
                    role,
                    slots,
                    &mut tasks,
                    &mut running,
                )
                .await?;
            }
            Ok::<(), Failure>(())
        })
        .await;
        if !matches!(tick, Ok(Ok(()))) {
            // A cache cannot distinguish revocation from a disconnected upstream. Both
            // stop agent processes; their workspaces and durable jobs remain for recovery.
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
            running.clear();
            role = None;
        }
        while let Some(completed) = tasks.try_join_next() {
            running.remove(&completed?);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn receive(
    server: &ProjectServer,
    machine: &str,
    app_data: &Path,
    cursor: &mut u64,
) -> Result<(), Failure> {
    let client = server.connect().await?;
    let page = client
        .channel_messages(Some(&machines::dispatch_name(machine)), *cursor)
        .await?;
    for message in page.messages {
        // User dispatches are explicit jobs; ordinary AIRC messages never execute work.
        if message.member.is_some() || message.from != "you" {
            continue;
        }
        if let Some(job) = Job::decode(&message.text)? {
            let path = job_path(app_data, message.seq);
            if !path.exists() {
                save(&path, &SavedJob::new(message.seq, job))?;
            }
        }
    }
    *cursor = page.cursor;
    Ok(())
}

async fn schedule(
    server: &ProjectServer,
    machine: &str,
    app_data: &Path,
    role: Role,
    slots: usize,
    tasks: &mut tokio::task::JoinSet<u64>,
    running: &mut BTreeSet<u64>,
) -> Result<(), Failure> {
    for path in job_paths(app_data)? {
        let mut saved: SavedJob = serde_json::from_slice(&std::fs::read(&path)?)?;
        if running.contains(&saved.sequence) {
            continue;
        }
        if let Some(result) = &saved.result {
            if !saved.reported
                && machine_dispatch::report(server, machine, saved.sequence, result)
                    .await
                    .is_ok()
            {
                saved.reported = true;
                save(&path, &saved)?;
            }
        } else if running.len() < slots {
            let sequence = saved.sequence;
            running.insert(sequence);
            let (server, app_data, machine) =
                (server.clone(), app_data.to_owned(), machine.to_owned());
            tasks.spawn(async move {
                let outcome = execute(&server, &app_data, &machine, role, &mut saved).await;
                saved.result = match outcome {
                    Ok(result) => result,
                    Err(error) => Some(format!("failed: {error}")),
                };
                if let Err(error) = save(&path, &saved) {
                    eprintln!("Dispatch {sequence}: {error}");
                }
                sequence
            });
        }
    }
    Ok(())
}

async fn execute(
    server: &ProjectServer,
    app_data: &Path,
    machine: &str,
    role: Role,
    saved: &mut SavedJob,
) -> Result<Option<String>, Failure> {
    if saved.job.role != role {
        return Err("Dispatch role differs from the enrolled machine role".into());
    }
    let agent = format!("{}-{}", role.label(), saved.sequence);
    let root = app_data.join("workspaces").join(&agent);
    if let Some(change) = &saved.change
        && machine_dispatch::change_closed(server, change).await?
    {
        return Ok(Some(format!("change {change} is settled")));
    }
    let change = if let Some(change) = &saved.change {
        velocity_rosen::reopen_workspace(server, &agent, change, &root).await?;
        change.clone()
    } else if root.exists() {
        machine_dispatch::existing_change(&root)?
    } else {
        velocity_rosen::create_workspace(server, &agent, &root, None)
            .await?
            .change
    };
    saved.change = Some(change.clone());
    save(&job_path(app_data, saved.sequence), saved)?;
    let session = machine_dispatch::agent_session(server, &agent).await?;
    let member = velocity_acp::client::duty_member(&agent);
    session.join_channel(&member, machine).await?;
    let mut events = session.events(None).await?;
    let watch = Watches::default();
    watch.start(server, &agent, &root).await?;
    let (launch, setup) = agent_setup(server, app_data, &root, &agent, &change, saved)?;
    let outcome = run_agent(launch, setup, app_data, saved, &mut events).await;
    watch.stop(&root).await;
    let _ = session.client().channel_leave(&member).await;
    if outcome.as_ref().is_ok_and(|finished| *finished)
        && root.exists()
        && !machine_dispatch::change_closed(server, &change).await?
    {
        session.snapshot(&root, None).await?;
    }
    outcome.map(|finished| finished.then(|| format!("turn ended; change {change}")))
}

fn agent_setup(
    server: &ProjectServer,
    app_data: &Path,
    root: &Path,
    agent: &str,
    change: &str,
    saved: &SavedJob,
) -> Result<(AgentLaunch, SessionSetup), Failure> {
    let store = velocity_store::StateStore::load_or_default(app_data)?;
    let settings = store.settings();
    let (mut launch, bypass) = crate::agent_launch(&settings, settings.providers.default_provider)?;
    launch
        .env
        .extend(velocity_rosen::agent_environment(server, agent, change));
    let mut defaults = velocity_store::ThreadItem::new(0, agent.into());
    defaults.provider = settings.providers.default_provider;
    defaults.model_name = settings.provider_model(defaults.provider);
    defaults.effort = settings.providers.default_effort;
    defaults.preferred_speed = settings.providers.default_speed;
    let mut setup = SessionSetup::new(root.to_owned(), bypass)
        .with_run_settings(crate::titles::thread_run_settings(&defaults));
    setup.preface = Some(format!(
        "{}\nYour agent name is {agent}; your role is {}.\n",
        velocity_rosen::workspace_note(change),
        saved.job.role.label()
    ));
    if let Some(id) = &saved.session {
        setup.resume = Some(SessionResume::new(Some(id.clone()), String::new()));
    }
    setup.shared_servers.extend(rosen_server(root, &launch.env));
    setup.airc = Some(
        velocity_acp::client::AircLaunch::new(std::env::current_exe()?, app_data.to_owned())
            .duty(0, agent),
    );
    Ok((launch, setup))
}

fn rosen_server(root: &Path, environment: &[(String, String)]) -> Option<McpServer> {
    let command = velocity_rosen::mcp_command(root, environment)?;
    Some(McpServer::Stdio(
        McpServerStdio::new("rosen", command.program)
            .args(command.arguments)
            .env(
                command
                    .environment
                    .into_iter()
                    .map(|(name, value)| EnvVariable::new(name, value))
                    .collect(),
            ),
    ))
}

async fn run_agent(
    launch: AgentLaunch,
    setup: SessionSetup,
    app_data: &Path,
    saved: &mut SavedJob,
    rosen_events: &mut tokio::sync::mpsc::Receiver<velocity_rosen::ProjectEvent>,
) -> Result<bool, Failure> {
    let (inputs, receiver) = unbounded_channel();
    let (sender, mut events) = unbounded_channel();
    inputs.send(AgentInput::Send(vec![ContentBlock::from(
        saved.job.prompt.clone(),
    )]))?;
    let runner = run_session(launch, setup, receiver, sender);
    tokio::pin!(runner);
    loop {
        tokio::select! {
            result = &mut runner => return result.map(|()| true).map_err(Into::into),
            Some(event) = events.recv() => match event {
                AgentEvent::SessionStarted { session_id, .. } => {
                    saved.session = Some(session_id.to_string());
                    save(&job_path(app_data, saved.sequence), saved)?;
                }
                AgentEvent::TurnEnded(result) => {
                    inputs.send(AgentInput::Stop)?;
                    runner.await?;
                    return result.map(|_| true).map_err(Into::into);
                }
                _ => {}
            },
            event = rosen_events.recv() => if event.is_none() {
                inputs.send(AgentInput::Stop)?;
                runner.await?;
                return Ok(false);
            },
        }
    }
}

fn job_path(app_data: &Path, sequence: u64) -> PathBuf {
    app_data.join("dispatch").join(format!("{sequence}.json"))
}

fn job_paths(app_data: &Path) -> Result<Vec<PathBuf>, Failure> {
    let mut paths = std::fs::read_dir(app_data.join("dispatch"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    paths.sort_by_key(|path| {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse::<u64>().ok())
    });
    Ok(paths)
}

fn save(path: &Path, saved: &SavedJob) -> Result<(), Failure> {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, serde_json::to_vec(saved)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}
