//! App host for the background Rosen landing and verification workers.

use anyhow::Context;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_core::duty::{DutyEventKind, DutyFeedEvent, DutyRole};
use velocity_store::duty::LandingProject;
use velocity_threads::landing_runner::{BuildRepairs, LandingHost, RosenLanding};

use crate::duty::DutyEventPublished;
use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

pub fn initialize(application: &AppHandle) -> anyhow::Result<()> {
    let app_data = application
        .path()
        .app_data_dir()
        .context("Could not resolve application data")?;
    let runner = RosenLanding::new(app_data);
    application.manage(runner.clone());
    tauri::async_runtime::spawn(runner.run(TauriLandingHost(application.clone())));
    Ok(())
}

pub fn wake(application: &AppHandle) {
    if let Some(runner) = application.try_state::<RosenLanding>() {
        runner.wake();
    }
}

#[derive(Clone)]
struct TauriLandingHost(AppHandle);

impl LandingHost for TauriLandingHost {
    fn projects(&self) -> Vec<LandingProject> {
        lock(
            self.0.state::<ManagedStateStore>().inner(),
            "persisted state",
        )
        .map(|store| store.landing_projects())
        .unwrap_or_default()
    }

    fn integration_idle(&self, project: u32) -> bool {
        crate::continuous_dispatch::integration_idle(&self.0, project).unwrap_or(false)
    }

    fn change_landed(&self, project: u32, change: &str, generation: u64) -> Result<(), String> {
        crate::rosen_events::record_landed(&self.0, project, change, generation)
    }

    fn land_refused(
        &self,
        project: u32,
        change: &str,
        generation: u64,
        message: &str,
    ) -> Result<bool, String> {
        crate::rosen_events::record_land_refusal(&self.0, project, change, generation, message)
    }

    fn activity(&self, project: u32, actor: &str, kind: DutyEventKind, message: String) {
        report(self.publish_activity(project, actor, kind, message));
    }

    fn repair_builds(&self, project: u32, observation: BuildRepairs) -> Result<(), String> {
        lock(
            self.0.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .close_build_repairs(project, &observation.settled)
        .map_err(|error| error.to_string())?;
        for failure in observation.pending {
            // A failure launching one culprit must not hide a later, independent break.
            report(self.start_build_repair(project, &failure));
        }
        Ok(())
    }
}

impl TauriLandingHost {
    fn start_build_repair(
        &self,
        project: u32,
        failure: &velocity_rosen::build_repairs::BuildFailure,
    ) -> Result<(), String> {
        let planned = {
            let mut store = lock(
                self.0.state::<ManagedStateStore>().inner(),
                "persisted state",
            )?;
            let existing = store.project(project).and_then(|project| {
                project
                    .build_repairs
                    .iter()
                    .find(|repair| repair.commit == failure.commit)
                    .map(|repair| (project.id, repair.thread))
            });
            if let Some(key) = existing
                && lock(
                    self.0.state::<crate::threads::ManagedThreads>().inner(),
                    "thread sessions",
                )?
                .is_live(key)
            {
                return Ok(());
            }
            let planned =
                velocity_threads::build_repairs::prepare_repair(&mut store, project, failure)?;
            if let Some((_, thread)) = &planned {
                store
                    .update_thread(project, *thread, |thread| {
                        thread.state = velocity_store::ThreadState::Working;
                        thread.activity = velocity_store::AgentActivity::Executing;
                    })
                    .map_err(|error| error.to_string())?;
            }
            planned
        };
        if let Some((plan, thread)) = planned {
            // Persist the thread identity and opening prompt before any process starts.
            crate::state::save_now(&self.0)?;
            crate::threads::start_planned_with_selection(&self.0, plan, thread, false)?;
        }
        Ok(())
    }

    fn publish_activity(
        &self,
        project: u32,
        actor: &str,
        kind: DutyEventKind,
        line: String,
    ) -> Result<(), String> {
        let store_state = self.0.state::<ManagedStateStore>();
        let mut store = lock(store_state.inner(), "persisted state")?;
        let events = store
            .update_scouts(project, |_, timestamp| {
                vec![DutyFeedEvent::new(timestamp, actor, kind, line.clone())]
            })
            .map_err(|error| error.to_string())?;
        let Some(events) = events else {
            eprintln!("Rosen project {project}: {line}");
            return Ok(());
        };
        if store.active_project().map(|item| item.id) != Some(project) {
            return Ok(());
        }
        let mut layout = lock(self.0.state::<MainLayout>().inner(), "main-window state")?;
        store.refresh_layout(&mut layout);
        velocity_interview::overlay_live(
            self.0.state::<velocity_interview::Interviews>().inner(),
            &store,
            &mut layout,
        );
        crate::layout::publish(&self.0, &layout.snapshot(), None)?;
        for event in events {
            DutyEventPublished::new(DutyRole::Scouts, event)
                .emit(&self.0)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}
