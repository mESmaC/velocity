//! Resume after a restart (SPEC.md §3.2, #279): build threads whose turn the last exit cut
//! off continue by themselves. `velocity_threads::restart` decides which; this module starts
//! their sessions through the same resume path parking and stall recovery use.

use tauri::{AppHandle, Manager};
use velocity_threads::ThreadKey;
use velocity_threads::restart::CONTINUE_PROMPT;

use crate::threads::{ManagedThreads, lock};

/// Resumes each interrupted thread independently, so a slow Rosen server doesn't hold
/// up the window or other threads' recovery.
pub fn resume_interrupted(application: &AppHandle, interrupted: Vec<ThreadKey>) {
    for key in interrupted {
        let application = application.clone();
        tauri::async_runtime::spawn(async move {
            crate::threads::report(resume(&application, key).await);
        });
    }
}

async fn resume(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    // PR follow-through delivers pending review work under the same lease, and resumes the
    // session itself when none runs; holding it means only one of the two starts a session.
    let lease = crate::threads::action_lock(application, key)?;
    let _guard = lease.lock().await;
    let result = resume_session(application, key).await;
    lock(
        application
            .state::<crate::state::ManagedStateStore>()
            .inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |thread| {
        thread.startup_recovery_pending = false
    })
    .map_err(|error| error.to_string())?;
    result
}

async fn resume_session(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    if session_running(application, key)? {
        return Ok(());
    }
    let result = velocity_rosen::retry_workspace(|| attempt_resume(application, key)).await;
    if let Err(error) = result.and_then(|attempt| attempt) {
        if session_running(application, key)? {
            return Ok(());
        }
        crate::threads::fail(
            application,
            key,
            "Could not continue after Velocity restarted",
            &error,
        );
    }
    Ok(())
}

async fn attempt_resume(
    application: &AppHandle,
    key: ThreadKey,
) -> Result<Result<(), String>, String> {
    let application = application.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let queued_runs = lock(
            application
                .state::<crate::state::ManagedStateStore>()
                .inner(),
            "persisted state",
        )?
        .project(key.0)
        .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
        .map(|thread| thread.queued_runs.clone())
        .unwrap_or_default();
        if queued_runs.is_empty() {
            crate::parking::resume(&application, key, CONTINUE_PROMPT)
        } else {
            crate::parking::resume_restored_runs(&application, key, queued_runs)
        }
    })
    .await
    .map_err(|error| error.to_string())?;
    // Only Rosen AIRC failures should reach the transport error classifier.
    match result {
        Err(error) if error.starts_with("Rosen AIRC: ") => Err(error),
        result => Ok(result),
    }
}

fn session_running(application: &AppHandle, key: ThreadKey) -> Result<bool, String> {
    let runtimes = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?;
    Ok(runtimes.is_live(key) && !runtimes.is_parked(key))
}
