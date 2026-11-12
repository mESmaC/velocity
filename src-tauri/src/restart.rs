//! Resume after a restart (SPEC.md §3.2, #279): build threads whose turn the last exit cut
//! off continue by themselves. `velocity_threads::restart` decides which; this module starts
//! their sessions through the same resume path parking and stall recovery use.

use tauri::{AppHandle, Manager};
use velocity_threads::ThreadKey;
use velocity_threads::restart::CONTINUE_PROMPT;

use crate::threads::{ManagedThreads, lock};

/// Resumes each interrupted thread off the setup thread: checking a worktree is git work,
/// and the window should open without waiting for it.
pub fn resume_interrupted(application: &AppHandle, interrupted: Vec<ThreadKey>) {
    if interrupted.is_empty() {
        return;
    }
    let application = application.clone();
    std::thread::spawn(move || {
        for key in interrupted {
            crate::threads::report(resume(&application, key));
        }
    });
}

fn resume(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    // PR follow-through delivers pending review work under the same lease, and resumes the
    // session itself when none runs; holding it means only one of the two starts a session.
    let lease = crate::threads::action_lock(application, key)?;
    let _guard = lease.blocking_lock();
    if session_running(application, key)? {
        return Ok(());
    }
    if let Err(error) = crate::parking::resume(application, key, CONTINUE_PROMPT) {
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

fn session_running(application: &AppHandle, key: ThreadKey) -> Result<bool, String> {
    let runtimes = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?;
    Ok(runtimes.is_live(key) && !runtimes.is_parked(key))
}
