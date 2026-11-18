use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_rosen::{Migration, ProjectServer};
use velocity_store::{ProjectItem, VersionControl, migration::migration_waiting};
use velocity_threads::ThreadRuntimes;

use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock, report};

pub(crate) fn initialize(application: &AppHandle) {
    let application = application.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let projects = lock(application.state::<ManagedStateStore>().inner(), "state")
                .map(|store| {
                    store
                        .state()
                        .projects
                        .iter()
                        .filter(|item| item.version_control == VersionControl::Git)
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for project in projects {
                if let Err(notice) = migrate(&application, &project).await {
                    report(save_notice(&application, project.id, notice));
                }
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
}

async fn migrate(application: &AppHandle, project: &ProjectItem) -> Result<(), String> {
    if migration_waiting(project) || running(application, project)? {
        return Err(
            "Migration to Rosen waits for this project's running threads to finish.".into(),
        );
    }
    let workspace_root = {
        let store = lock(application.state::<ManagedStateStore>().inner(), "state")?;
        velocity_threads::project_workspace_root(project, &store.worktree_preferences(project))
            .ok_or("Set a workspace location for this project to continue migration.")?
    };
    let repository = PathBuf::from(&project.repository_path);
    let branch = project.default_branch.clone();
    let plan =
        tauri::async_runtime::spawn_blocking(move || Migration::inspect(&repository, &branch))
            .await
            .map_err(display)?
            .map_err(display)?;
    let server = application
        .state::<crate::rosen::ManagedRosen>()
        .servers
        .ready(
            project.id,
            &plan.source,
            Some(workspace_root.join(".rosen-pool")),
        )
        .await
        .map_err(display)?;
    let plan = verify(plan, &server).await?;
    let threads = project
        .threads
        .iter()
        .filter_map(
            |thread| match (&thread.rosen_change, &thread.worktree_path) {
                (None, Some(path)) => Some((thread.id, PathBuf::from(path))),
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    let migrated = plan
        .import_changes(&server, &workspace_root, &threads)
        .await
        .map_err(display)?;
    verify(plan, &server).await?;
    let events = velocity_rosen::migration_event_cursor(&server)
        .await
        .map_err(display)?;
    if !finish(application, project, &migrated, events)? {
        return Ok(());
    }
    let layout = crate::thread_menu::update_layout(application, |_| {});
    let handle = application.clone();
    tauri::async_runtime::spawn_blocking(move || crate::state::save_now(&handle))
        .await
        .map_err(display)??;
    crate::rosen_events::follow(application, project.id);
    layout.map(|_| ())
}

async fn verify(plan: Migration, server: &ProjectServer) -> Result<Migration, String> {
    let server = server.clone();
    tauri::async_runtime::spawn_blocking(move || {
        plan.verify(&server).map_err(display)?;
        Ok(plan)
    })
    .await
    .map_err(display)?
}

fn running(application: &AppHandle, project: &ProjectItem) -> Result<bool, String> {
    let runtimes = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?;
    Ok(has_running_thread(&runtimes, project))
}

fn has_running_thread(runtimes: &ThreadRuntimes, project: &ProjectItem) -> bool {
    project.threads.iter().any(|thread| {
        let key = (project.id, thread.id);
        (runtimes.is_live(key) && !runtimes.is_parked(key)) || runtimes.is_stopping(key)
    })
}

fn finish(
    application: &AppHandle,
    project: &ProjectItem,
    migrated: &BTreeMap<u32, (String, PathBuf)>,
    events: u64,
) -> Result<bool, String> {
    let mut store = lock(application.state::<ManagedStateStore>().inner(), "state")?;
    let mut runtimes = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?;
    if has_running_thread(&runtimes, project) {
        return Ok(false);
    }
    // Resume and deletion share these leases. An in-flight action may have read the Git
    // path already, so let it finish and check the project again on the next pass.
    let leases = project
        .threads
        .iter()
        .map(|thread| {
            runtimes
                .action_lock((project.id, thread.id))
                .try_lock_owned()
        })
        .collect::<Result<Vec<_>, _>>();
    let Ok(_leases) = leases else {
        return Ok(false);
    };
    if !store
        .finish_rosen_migration(project, migrated, events)
        .map_err(display)?
    {
        return Ok(false);
    }
    for (thread, (_, path)) in migrated {
        runtimes.rename_worktree((project.id, *thread), &path.to_string_lossy());
    }
    Ok(true)
}

fn save_notice(application: &AppHandle, project: u32, notice: String) -> Result<(), String> {
    let mut store = lock(application.state::<ManagedStateStore>().inner(), "state")?;
    store
        .save_migration_notice(project, notice)
        .map_err(display)?;
    let notice = store
        .active_project()
        .and_then(|item| item.rosen_migration_notice.clone());
    lock(
        application.state::<crate::layout::MainLayout>().inner(),
        "layout",
    )?
    .set_migration_notice(notice);
    drop(store);
    crate::layout::publish_soon(application);
    Ok(())
}

fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}
