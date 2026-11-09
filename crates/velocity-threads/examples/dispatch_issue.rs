//! F9 without a window: list the saved active project's live issues, or dispatch
//! one through the real store, worktree setup and ACP runner. Stop after its first turn.
//!
//! cargo run -p velocity-threads --example dispatch_issue -- <state folder> [number] [--accept]
use std::path::PathBuf;
use velocity_acp::client::{AgentEvent, SessionSetup};
use velocity_github::IssueClient;
use velocity_store::{AgentActivity, StateStore, ThreadState};
use velocity_threads::{IssueDispatchPlan, ThreadRuntimes, plan_issue_dispatch, spawn_session};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let folder = PathBuf::from(arguments.next().ok_or("missing <state folder>")?);
    let mut store = StateStore::load_or_default(folder)?;
    let project = store.active_project().ok_or("no active project")?;
    let project_identifier = project.id;
    let client = IssueClient::discover(std::path::Path::new(&project.repository_path))?;
    let (repository, mut rows) = client.panel()?;
    velocity_threads::dispatch::associate_issue_threads(project, &repository, &mut rows);
    for row in rows {
        println!(
            "#{} {} · {} · {:?} · {:?}",
            row.number,
            row.title,
            row.labels.join(", "),
            row.thread_label,
            row.question
        );
    }
    let Some(number) = arguments.next() else {
        return Ok(());
    };
    let issue = client.get(number.parse()?)?;
    let accept = arguments.any(|argument| argument == "--accept");
    let plan = plan_issue_dispatch(&store, project_identifier, &repository, &issue, accept)?;
    let thread = plan.record(&mut store)?;
    println!(
        "thread {thread}: {} · {}",
        plan.launch.thread.title, plan.launch.thread.branch_name
    );
    let result = run(&plan, thread);
    store.update_thread(project_identifier, thread, |record| {
        record.activity = AgentActivity::Idle;
        if result.is_err() {
            record.state = ThreadState::Error;
        }
    })?;
    result
}

fn run(plan: &IssueDispatchPlan, thread: u32) -> Result<(), Box<dyn std::error::Error>> {
    let launch = &plan.launch;
    let key = (launch.project, thread);
    let mut runtimes = ThreadRuntimes::default();
    let inputs = runtimes.start(key, &launch.thread.worktree_path, &launch.prompt);
    let created = velocity_git::execute_worktree_setup(&launch.worktree)?;
    println!("{}", created.transcript_step);
    runtimes.worktree_ready(key);
    let setup = SessionSetup::new(
        launch.worktree.worktree_path.clone(),
        launch.bypass_modes.clone(),
    );
    let mut session = spawn_session(launch.agent.clone(), setup, inputs);
    while let Some(event) = session.events.blocking_recv() {
        runtimes.apply(key, &event);
        println!("{event:?}");
        if let AgentEvent::TurnEnded(outcome) = event {
            runtimes.stop(key);
            let stopped = session.outcome.blocking_recv()?;
            stopped.map_err(std::io::Error::other)?;
            return outcome
                .map(|_| ())
                .map_err(|error| std::io::Error::other(error).into());
        }
    }
    Err("The agent ended before completing a turn".into())
}
