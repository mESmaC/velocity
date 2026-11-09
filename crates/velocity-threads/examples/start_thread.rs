//! Runs one thread's launch path without the window: plans it for the saved active project,
//! creates and sets up its worktree, starts the provider's ACP agent there, sends the prompt
//! and prints the transcript until the agent's first turn ends.
//!
//! `cargo run -p velocity-threads --example start_thread -- <state folder> "<prompt>"`

use std::path::PathBuf;

use velocity_acp::client::{AgentEvent, SessionSetup};
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_store::StateStore;
use velocity_threads::{ThreadRuntimes, plan_launch, spawn_session};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let folder = PathBuf::from(arguments.next().ok_or("missing <state folder>")?);
    let prompt = arguments.next().ok_or("missing \"<prompt>\"")?;
    let store = StateStore::load_or_default(folder)?;
    let project = store.active_project().ok_or("no active project")?;
    let worktrees = store.worktree_context().preferences;
    let plan = plan_launch(project, &worktrees, &store.settings(), &prompt)?;
    println!("branch   {}", plan.thread.branch_name);
    println!("worktree {}", plan.thread.worktree_path);
    let key = (plan.project, 0);
    let mut runtimes = ThreadRuntimes::default();
    let inputs = runtimes.start(key, &plan.thread.worktree_path, &prompt);
    let created = velocity_git::execute_worktree_setup(&plan.worktree)?;
    println!("{}", created.transcript_step);
    runtimes.worktree_ready(key);
    let mut session = spawn_session(
        plan.agent,
        SessionSetup {
            worktree: plan.worktree.worktree_path,
            airc: None,
            shared_servers: Vec::new(),
            bypass_modes: plan.bypass_modes,
        },
        inputs,
    );
    while let Some(event) = session.events.blocking_recv() {
        if let Some(progress) = runtimes.apply(key, &event) {
            println!(
                "activity {:?} +{} -{}",
                progress.activity, progress.added, progress.deleted
            );
        }
        match &event {
            AgentEvent::TurnEnded(outcome) => {
                println!("turn ended: {outcome:?}");
                break;
            }
            AgentEvent::Log(line) => eprintln!("agent: {line}"),
            _ => {}
        }
    }
    if let Ok(outcome) = session.outcome.try_recv() {
        println!("session ended: {outcome:?}");
    }
    for block in runtimes
        .view(key)
        .map(|view| view.blocks)
        .unwrap_or_default()
    {
        println!("{}", describe(&block));
    }
    Ok(())
}

fn describe(block: &TranscriptBlock) -> String {
    match block {
        TranscriptBlock::UserMessage { text, queued } => {
            format!("you{}: {text}", if *queued { " (queued)" } else { "" })
        }
        TranscriptBlock::AgentMessage { paragraphs } => {
            format!("agent: {}", paragraphs.join(" / "))
        }
        TranscriptBlock::ToolGroup { steps } => steps
            .iter()
            .map(|step| {
                format!(
                    "  [{}] {} {} {}",
                    if step.running { "…" } else { "✓" },
                    step.verb,
                    step.argument,
                    step.result_text
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        TranscriptBlock::Diff { block } => format!(
            "  diff {} +{} -{}",
            block.file_path, block.added, block.deleted
        ),
        TranscriptBlock::StatusLine { text, .. } => format!("status: {text}"),
        TranscriptBlock::ErrorCard { card } => {
            format!("error: {} · {}", card.headline, card.detail)
        }
    }
}
