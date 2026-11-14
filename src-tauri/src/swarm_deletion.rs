//! Deleting a swarm or goal swarm from its sidebar row (SPEC.md F10, F12, F16).
//!
//! It is the thread delete for every member at once: the swarm row and its members leave
//! the sidebar and a toast offers Undo for 5 s. Then the swarm's record leaves `state.json`
//! and each member is deleted as a thread is (agent stopped, worktree removed, PR closed,
//! task back to todo). No confirmation, as for a thread (Rule 1).

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use tauri::{AppHandle, Manager};
use velocity_core::layout_view::LayoutSnapshot;
use velocity_core::thread_menu::{
    SwarmMenuFacts, ThreadMenuEntry, has_open_pull_request, owns_worktree,
};
use velocity_store::ThreadItem;
use velocity_store::swarms::swarm_member_threads;
use velocity_threads::Deletion;

use crate::state::ManagedStateStore;
use crate::thread_menu::{begin_deletion, land_thread, show_problems};
use crate::threads::{ManagedThreads, lock};

/// A swarm's title and its members' saved threads.
struct FoundSwarm {
    title: String,
    members: Vec<ThreadItem>,
}

/// The swarm `identifier` names on `project`, or `None` when it names no swarm.
fn find_swarm(
    application: &AppHandle,
    project: u32,
    identifier: u32,
) -> Result<Option<FoundSwarm>, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let Some(item) = store.project(project) else {
        return Ok(None);
    };
    let Some(members) = swarm_member_threads(item, identifier) else {
        return Ok(None);
    };
    let title = item
        .swarms
        .iter()
        .find(|swarm| swarm.id == identifier)
        .map(|swarm| swarm.title())
        .or_else(|| {
            item.goal_swarms
                .iter()
                .find(|swarm| swarm.id == identifier)
                .map(|swarm| velocity_core::goal_swarm::goal_swarm_title(&swarm.prompt))
        })
        .unwrap_or_default();
    let members = item
        .threads
        .iter()
        .filter(|thread| members.contains(&thread.id))
        .cloned()
        .collect();
    Ok(Some(FoundSwarm { title, members }))
}

pub fn is_swarm(application: &AppHandle, project: u32, identifier: u32) -> Result<bool, String> {
    Ok(find_swarm(application, project, identifier)?.is_some())
}

/// The swarm row's menu, or `None` when the row is not a swarm. Measuring the members'
/// worktrees walks them, so it runs off the main thread.
pub async fn swarm_menu(
    application: &AppHandle,
    project: u32,
    identifier: u32,
) -> Result<Option<Vec<ThreadMenuEntry>>, String> {
    let Some(swarm) = find_swarm(application, project, identifier)? else {
        return Ok(None);
    };
    let mut facts = SwarmMenuFacts::new();
    {
        let runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        facts.agents_running = swarm
            .members
            .iter()
            .filter(|thread| runtimes.is_live((project, thread.id)))
            .count();
    }
    facts.open_pull_requests = swarm
        .members
        .iter()
        .filter(|thread| has_open_pull_request(thread.state.into(), thread.pull_request_number))
        .filter_map(|thread| thread.pull_request_number)
        .collect();
    let paths: Vec<PathBuf> = swarm
        .members
        .iter()
        .filter(|thread| owns_worktree(thread.state.into(), thread.worktree_path.as_deref()))
        .filter_map(|thread| thread.worktree_path.as_deref().map(PathBuf::from))
        .collect();
    let measured = tauri::async_runtime::spawn_blocking(move || {
        paths
            .iter()
            .filter(|path| path.is_dir())
            .map(|path| {
                velocity_git::inventory::measure_worktree(path, &AtomicBool::new(false))
                    .unwrap_or(0)
            })
            .collect::<Vec<u64>>()
    })
    .await
    .map_err(|error| error.to_string())?;
    facts.worktrees = measured.len();
    facts.worktree_bytes = measured.iter().sum();
    Ok(Some(velocity_core::thread_menu::swarm_menu(&facts)))
}

/// Hides the swarm and its members at once and offers Undo; the deletion lands after 5 s.
#[tauri::command]
#[specta::specta]
pub async fn delete_swarm(
    application: AppHandle,
    project_identifier: u32,
    swarm_identifier: u32,
) -> Result<LayoutSnapshot, String> {
    crate::layout::off_ui_thread(move || {
        delete_swarm_now(application, project_identifier, swarm_identifier)
    })
    .await
}

/// [`delete_swarm`] on the calling thread, for callers already off the UI thread.
pub(crate) fn delete_swarm_now(
    application: AppHandle,
    project_identifier: u32,
    swarm_identifier: u32,
) -> Result<LayoutSnapshot, String> {
    let selected = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .active_project()
    .is_some_and(|project| project.id == project_identifier);
    if !selected {
        return Err("The selected project changed".into());
    }
    let swarm = find_swarm(&application, project_identifier, swarm_identifier)?
        .ok_or("The swarm no longer exists")?;
    let worktrees = swarm
        .members
        .iter()
        .filter(|thread| {
            owns_worktree(thread.state.into(), thread.worktree_path.as_deref())
                && thread
                    .worktree_path
                    .as_deref()
                    .is_some_and(|path| std::path::Path::new(path).is_dir())
        })
        .count();
    let message = match worktrees {
        0 => format!("Deleted “{}”", swarm.title),
        1 => format!("Deleted “{}” and its worktree", swarm.title),
        count => format!("Deleted “{}” and {count} worktrees", swarm.title),
    };
    let deletion = Deletion::Swarm {
        project: project_identifier,
        swarm: swarm_identifier,
        members: swarm.members.iter().map(|thread| thread.id).collect(),
    };
    begin_deletion(&application, deletion, message)
}

/// The undo window is over. The swarm's record goes first, so nothing new joins it (a goal
/// swarm's keep going, + Items) and its view closes. Then every member, read again now, is
/// deleted as a thread, side by side so one slow agent does not hold up the others.
pub async fn land(application: &AppHandle, project: u32, swarm: u32, hidden: Vec<u32>) {
    let found = find_swarm(application, project, swarm);
    let mut problems = Vec::new();
    let mut rows: Vec<usize> = std::iter::once(swarm)
        .chain(hidden)
        .map(|row| row as usize)
        .collect();
    if let Ok(Some(found)) = found {
        let removed = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )
        .and_then(|mut store| {
            store
                .remove_swarm(project, swarm)
                .map_err(|error| error.to_string())
        });
        if let Err(error) = removed {
            problems.push(format!("Could not delete “{}”: {error}", found.title));
        }
        let landing: Vec<_> = found
            .members
            .iter()
            .map(|thread| {
                rows.push(thread.id as usize);
                let application = application.clone();
                let key = (project, thread.id);
                tauri::async_runtime::spawn(async move { land_thread(&application, key).await })
            })
            .collect();
        for member in landing {
            match member.await {
                Ok(member_problems) => problems.extend(member_problems),
                Err(error) => problems.push(error.to_string()),
            }
        }
    }
    show_problems(application, project, &rows, &problems);
}
