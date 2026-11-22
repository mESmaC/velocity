//! A goal member's AIRC dependency becomes a Rosen D19 stack once its session is parked.
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};
use velocity_airc::Channel;
use velocity_store::goal_rosen::GoalStack;
use velocity_store::{AgentActivity, ThreadItem, ThreadState};
use velocity_threads::ThreadKey;

use crate::state::ManagedStateStore;
use crate::threads::lock;

pub(crate) fn continue_wait(
    application: &AppHandle,
    key: ThreadKey,
    thread: &ThreadItem,
    channel: &Channel,
) -> Result<bool, String> {
    if thread.rosen_change.is_none()
        || thread.goal_member.is_none()
        || thread.activity != AgentActivity::Parked
    {
        return Ok(false);
    }
    let member = thread.airc_member();
    let air_wait = channel
        .dependency_waits()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|wait| wait.member == member);
    let parent = if let Some(wait) = air_wait.as_ref() {
        parent_change(application, key.0, &wait.holder)?
    } else if thread.rosen_path_wait.is_some()
        && thread
            .rosen_overlap_wait
            .as_ref()
            .is_none_or(|wait| wait.blockers.is_empty())
        && let Some(change) = thread.rosen_change.as_deref()
    {
        rosen_wait_parent(application, key.0, change, thread.goal_stack.as_ref())?
    } else {
        None
    };
    let Some(parent) = parent else {
        return Ok(false);
    };
    // Only one stack parent exists in D19. A second dependency waits for its land.
    if thread
        .goal_stack
        .as_ref()
        .is_some_and(|stack| stack.parent != parent)
    {
        return Ok(false);
    }
    let result = tauri::async_runtime::block_on(stack(application, key, thread, &parent));
    let change = match result {
        Ok(change) => change,
        Err(error) => {
            eprintln!("Goal stack kept thread {} waiting: {error}", key.1);
            return Ok(false);
        }
    };
    let message = format!(
        "Continued on stacked change {change}, on {parent}. Your task and transcript are kept. Read rosen status, claim your code paths with `rosen claim --exclusive <paths>` on this change and continue here; Rosen waits for the parent to land."
    );
    if air_wait.is_some() {
        channel
            .finish_stacked_wait(&member, &message)
            .map_err(|error| error.to_string())?;
    }
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |saved| {
        saved.rosen_path_wait = None;
        saved.dependency_wait = None;
    })
    .map_err(|error| error.to_string())?;
    match crate::parking::resume(application, key, &message) {
        Ok(()) => Ok(true),
        Err(error) if error.is_non_resumable() => {
            crate::parking::drop_non_resumable_delivery(application, key, &error);
            Ok(true)
        }
        Err(error) => Err(error.to_string()),
    }
}

fn rosen_wait_parent(
    application: &AppHandle,
    project: u32,
    change: &str,
    existing_stack: Option<&GoalStack>,
) -> Result<Option<String>, String> {
    let holders = tauri::async_runtime::block_on(async {
        let server = crate::rosen::server(application, project).await?;
        let holders = velocity_rosen::mcp_proxy::active_wait_holders(&server, change)
            .await
            .map_err(|error| error.to_string())?;
        if !holders.is_empty() {
            return Ok::<_, String>(holders);
        }
        // Retry an interrupted stack handoff whose original change still holds the wait.
        if let Some(stack) = existing_stack
            && Path::new(&stack.source_root).exists()
        {
            return velocity_rosen::mcp_proxy::active_wait_holders(&server, &stack.source_change)
                .await
                .map_err(|error| error.to_string());
        }
        Ok(Vec::new())
    })?;
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let Some(project) = store.project(project) else {
        return Ok(None);
    };
    Ok(select_rosen_wait_parent(&holders, &project.threads))
}

/// Chooses a single D19 stack parent from an active Rosen wait's actual `Claim` blockers (#674).
/// Returns `Some(change)` only when exactly one other live change blocks the active wait and
/// belongs to a working or reviewing thread in this project; multiple blockers or an unstackable
/// holder keep the thread waiting so it never resumes while another blocker still holds a path.
fn select_rosen_wait_parent(
    holders: &[(String, String)],
    threads: &[ThreadItem],
) -> Option<String> {
    let [(holder_change, _)] = holders else {
        return None;
    };
    threads
        .iter()
        .any(|other| {
            other.rosen_change.as_deref() == Some(holder_change.as_str())
                && matches!(other.state, ThreadState::Working | ThreadState::Review)
        })
        .then(|| holder_change.clone())
}

fn parent_change(
    application: &AppHandle,
    project: u32,
    holder: &str,
) -> Result<Option<String>, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    Ok(store
        .project(project)
        .and_then(|project| {
            project.threads.iter().find(|other| {
                other.matches_airc_member(holder)
                    && matches!(other.state, ThreadState::Working | ThreadState::Review)
            })
        })
        .and_then(|other| other.rosen_change.clone()))
}

async fn stack(
    application: &AppHandle,
    key: ThreadKey,
    thread: &ThreadItem,
    parent: &str,
) -> Result<String, String> {
    let server = crate::rosen::server(application, key.0).await?;
    let original = Path::new(thread.worktree_path.as_deref().ok_or("Missing workspace")?);
    let source = thread
        .rosen_change
        .as_deref()
        .ok_or("Missing Rosen change")?;
    if let Some(stack) = &thread.goal_stack {
        crate::rosen::record_work_sources(application, key, original, source).await?;
        cleanup_source(application, key.0, stack).await?;
        return Ok(source.to_owned());
    }
    let leaf = original.file_name().ok_or("Missing workspace name")?;
    let root = original
        .parent()
        .ok_or("Missing workspace root")?
        .join(".stacks")
        .join(parent)
        .join(leaf);
    crate::rosen::stop_watch(application, original).await;
    let created = velocity_rosen::stack_workspace(
        &server,
        &thread.rosen_agent_name(),
        original,
        parent,
        &root,
    )
    .await
    .map_err(|error| error.to_string());
    let created = created?;
    let transition = GoalStack::new(parent.into(), source.into(), original.display().to_string());
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_rosen_work_thread(key.0, key.1, |saved| {
        saved.rosen_change = Some(created.change.clone());
        saved.worktree_path = Some(root.display().to_string());
        saved.goal_stack = Some(transition.clone());
        // A provider's loaded session may retain its original working directory.
        // The new session instead receives the saved transcript in the new workspace.
        saved.agent_session = None;
    })
    .map_err(|error| error.to_string())?;
    crate::rosen::record_work_sources(application, key, &root, &created.change).await?;
    cleanup_source(application, key.0, &transition).await?;
    Ok(created.change)
}

async fn cleanup_source(
    application: &AppHandle,
    project: u32,
    stack: &GoalStack,
) -> Result<(), String> {
    // Deferred updates can still exist only in memory, including on a retry.
    // Keep the source recoverable until the child identity and workspace reach disk.
    crate::state::save_now(application)?;
    let root = PathBuf::from(&stack.source_root);
    if root.exists() {
        crate::rosen::abandon_unstarted(application, project, &root, &stack.source_change).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::select_rosen_wait_parent;
    use velocity_store::{ThreadItem, ThreadState};

    fn thread_with_change(id: u32, change: &str, state: ThreadState) -> ThreadItem {
        let mut thread = ThreadItem::new(id, format!("Thread {id}"));
        thread.rosen_change = Some(change.to_owned());
        thread.state = state;
        thread
    }

    // Risk (#674): F16 goal stacking must only select a stack parent when a single live Claim
    // blocker belongs to a working or reviewing thread, and must keep waiting when a real holder
    // is unstackable or when more than one change blocks the active wait.
    #[test]
    fn select_rosen_wait_parent_requires_single_stackable_blocker() {
        let working = "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm";
        let reviewing = "nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn";
        let failed = "pppppppppppppppppppppppppppppppp";
        let external = "qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
        let threads = vec![
            thread_with_change(1, working, ThreadState::Working),
            thread_with_change(2, reviewing, ThreadState::Review),
            thread_with_change(3, failed, ThreadState::Error),
        ];

        assert_eq!(
            select_rosen_wait_parent(&[(working.into(), "w1".into())], &threads),
            Some(working.to_owned())
        );
        assert_eq!(
            select_rosen_wait_parent(&[(reviewing.into(), "r1".into())], &threads),
            Some(reviewing.to_owned())
        );
        assert_eq!(
            select_rosen_wait_parent(&[(failed.into(), "f1".into())], &threads),
            None
        );
        assert_eq!(
            select_rosen_wait_parent(&[(external.into(), "ext".into())], &threads),
            None
        );
        assert_eq!(select_rosen_wait_parent(&[], &threads), None);
        // Two blockers (whether both stackable or one unstackable) must keep waiting so the
        // waiter never resumes on one parent while another holder still blocks a waited path.
        assert_eq!(
            select_rosen_wait_parent(
                &[
                    (working.into(), "w1".into()),
                    (reviewing.into(), "r1".into()),
                ],
                &threads,
            ),
            None
        );
        assert_eq!(
            select_rosen_wait_parent(
                &[
                    (working.into(), "w1".into()),
                    (external.into(), "ext".into())
                ],
                &threads,
            ),
            None
        );
    }
}
