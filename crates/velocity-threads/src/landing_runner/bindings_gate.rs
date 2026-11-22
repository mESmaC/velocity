//! Holds a named change that touches Tauri command or event registration until its IPC
//! bindings are checked (#503). The check runs off the landing path, so changes that need
//! none keep landing meanwhile, and a stale result goes back to the author like one of
//! Rosen's own regeneration refusals.

use std::path::{Path, PathBuf};

use velocity_core::duty::DutyEventKind;
use velocity_core::landing::{PreLandCheckPlan, pre_land_check_plan};
use velocity_core::rosen_checker::{
    BINDINGS_EXPORTER, bindings_unchecked_line, changes_bindings, checking_bindings_line,
};
use velocity_rosen::bindings_check::{self, BindingsVerdict};
use velocity_rosen::{ProjectServer, agent_run, change_details};
use velocity_store::ThreadItem;
use velocity_store::duty::LandingProject;

use super::{LANDER, LandingHost, RosenLanding};
use crate::launch::Location;

/// The project's folder the merge results are checked in, beside its agents' workspaces.
const CHECK_FOLDER: &str = "bindings-check";

/// What the runner knows about one named change's bindings.
pub(super) struct GateNote {
    /// The lifecycle generation the change was named at: naming it again checks it again.
    pub(super) generation: u64,
    /// `None` while the check runs.
    verdict: Option<BindingsVerdict>,
}

/// Whether a named change may be landed in this pass.
pub(super) enum Gate {
    Clear,
    /// Its check is running or waits for the project's check folder: a later pass lands it.
    Waiting,
    Stale,
}

impl RosenLanding {
    pub(super) async fn bindings_gate<Host: LandingHost>(
        &self,
        host: &Host,
        server: &ProjectServer,
        project: &LandingProject,
        thread: &ThreadItem,
        change: &str,
    ) -> Gate {
        let key = (project.project.id, thread.id);
        let generation = thread.rosen_progress_generation;
        let bindings_verified = match self.checked_gate(key, generation) {
            Some(Gate::Clear) => true,
            Some(gate) => return gate,
            None => false,
        };
        let touches = if bindings_verified {
            false
        } else {
            match touches_bindings(server, thread, change).await {
                Ok(touches) => touches,
                // The land needs the same server, so the next pass asks again.
                Err(_) => return Gate::Waiting,
            }
        };
        match pre_land_check_plan(true, touches, bindings_verified) {
            PreLandCheckPlan::LandNow => return Gate::Clear,
            PreLandCheckPlan::RefusePreflight => return Gate::Waiting,
            PreLandCheckPlan::RunBindingsCheck => {}
        }
        let Ok(root) = Location::new(&project.project, &project.worktrees).project_root() else {
            return Gate::Clear;
        };
        if !self.begin_check(key, generation) {
            return Gate::Waiting;
        }
        let line = checking_bindings_line(change);
        host.activity(key.0, LANDER, DutyEventKind::Wait, line);
        let (runner, host, server) = (self.clone(), host.clone(), server.clone());
        let change = change.to_owned();
        tokio::spawn(async move {
            let verdict = check(&server, &change, &root).await;
            runner.finish_check(&host, key, generation, &change, verdict);
        });
        Gate::Waiting
    }

    /// The gate of a change whose check was already started at this generation.
    fn checked_gate(&self, key: (u32, u32), generation: u64) -> Option<Gate> {
        let state = self.lock();
        let note = state.bindings.get(&key)?;
        (note.generation == generation).then_some(match &note.verdict {
            None => Gate::Waiting,
            Some(BindingsVerdict::Stale) => Gate::Stale,
            // No verdict is no reason to hold a change: it lands, and the feed says so.
            Some(BindingsVerdict::Current | BindingsVerdict::Unchecked(_)) => Gate::Clear,
        })
    }

    /// Takes the project's check folder for this change. `false` while another check has it.
    fn begin_check(&self, key: (u32, u32), generation: u64) -> bool {
        let mut state = self.lock();
        // One check at a time in a project: they share its folder and its target.
        if !state.checking_bindings.insert(key.0) {
            return false;
        }
        let verdict = None;
        state.bindings.insert(
            key,
            GateNote {
                generation,
                verdict,
            },
        );
        true
    }

    /// Whether a bindings check is running in `project`.
    pub(super) fn checking_bindings(&self, project: u32) -> bool {
        self.lock().checking_bindings.contains(&project)
    }

    fn finish_check<Host: LandingHost>(
        &self,
        host: &Host,
        key: (u32, u32),
        generation: u64,
        change: &str,
        verdict: BindingsVerdict,
    ) {
        if let BindingsVerdict::Unchecked(reason) = &verdict {
            let line = bindings_unchecked_line(change, reason);
            host.activity(key.0, LANDER, DutyEventKind::Wait, line);
        }
        {
            let mut state = self.lock();
            state.checking_bindings.remove(&key.0);
            // A change named again meanwhile lost its note, and is checked afresh.
            if let Some(note) = state.bindings.get_mut(&key)
                && note.generation == generation
            {
                note.verdict = Some(verdict);
            }
        }
        self.wake();
    }
}

/// Whether the change edits anything the exporter reads. The change's files are read from
/// its author's workspace, which holds what the named snapshot stored.
async fn touches_bindings(
    server: &ProjectServer,
    thread: &ThreadItem,
    change: &str,
) -> Result<bool, velocity_rosen::RosenError> {
    let workspace = thread
        .worktree_path
        .as_deref()
        .map(PathBuf::from)
        .filter(|root| root.is_dir());
    // A project without the exporter has no bindings; without a workspace the staged
    // merge result answers that instead.
    if let Some(root) = &workspace
        && !root.join(BINDINGS_EXPORTER).is_file()
    {
        return Ok(false);
    }
    let paths = change_details::changed_paths(server, change).await?;
    Ok(paths.iter().any(|path| {
        let source = workspace
            .as_ref()
            .and_then(|root| std::fs::read_to_string(root.join(path)).ok());
        changes_bindings(path, source.as_deref())
    }))
}

async fn check(server: &ProjectServer, change: &str, root: &Path) -> BindingsVerdict {
    let folder = root.join(CHECK_FOLDER);
    match bindings_check::stage(server, change, &folder).await {
        Ok(true) => {}
        // Rosen refuses this land itself and names the paths to repair.
        Ok(false) => return BindingsVerdict::Current,
        Err(error) => return BindingsVerdict::Unchecked(error.to_string()),
    }
    let targets = agent_run::project_targets(root);
    tokio::task::spawn_blocking(move || bindings_check::check_staged(&folder, &targets))
        .await
        .unwrap_or_else(|error| BindingsVerdict::Unchecked(error.to_string()))
}
