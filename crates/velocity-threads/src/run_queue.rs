//! Every agent's heavy builds go through the machine's one build queue (F5 "Build queue",
//! #308), whatever its project's AGENTS.md says: agents in projects without Velocity's
//! `scripts/build-queue.ps1` would otherwise all compile at once.

use velocity_acp::client::{AgentLaunch, SessionSetup};

/// Gives an agent outside a Rosen workspace `rosen` on its `PATH` and the note that tells
/// it to run builds through `rosen run`. An agent in a Rosen workspace already has both
/// (`velocity_rosen::agent_environment` and `workspace_note`), and nothing changes when
/// the `rosen` program isn't found.
pub(crate) fn with_run_queue(agent: &mut AgentLaunch, setup: &mut SessionSetup) {
    if agent.env.iter().any(|(name, _)| name == "ROSEN_CHANGE") {
        return;
    }
    // A launch that sets its own `PATH` keeps it, with Rosen's folder in front.
    let own_path = agent
        .env
        .iter()
        .rposition(|(name, _)| name.eq_ignore_ascii_case("PATH"));
    let existing = match own_path {
        Some(index) => agent.env[index].1.clone(),
        None => std::env::var_os("PATH")
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    };
    let Some(path) = velocity_rosen::path_with_rosen(&existing) else {
        return;
    };
    match own_path {
        Some(index) => agent.env[index].1 = path,
        None => agent.env.push((String::from("PATH"), path)),
    }
    let note = velocity_rosen::run_queue_note();
    setup.preface = Some(match setup.preface.take() {
        Some(preface) => format!("{note}\n\n{preface}"),
        None => note.to_owned(),
    });
}
