//! Velocity's proof checks on a PR thread (SPEC.md F17 "What Velocity checks", "Proof card"),
//! run in the thread's own worktree once per PR head. Lake can take minutes, so each run is a
//! background job; the next follow-through observation picks its result up, saves it with the
//! rest of the PR history and publishes the card, the "Proved" step and the `proved` event.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::thread;

use tauri::{AppHandle, Manager};
use velocity_core::proof_card::ProofCard;
use velocity_github::follow_through::FollowThrough;

type ThreadKey = (u32, u32);

enum Job {
    Running,
    Finished {
        head: String,
        card: Option<ProofCard>,
    },
}

fn jobs() -> &'static Mutex<HashMap<ThreadKey, Job>> {
    static JOBS: OnceLock<Mutex<HashMap<ThreadKey, Job>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// What a PR thread's checks need: where its checkout is and what its PR merges into.
pub(crate) struct ProofTarget<'a> {
    pub key: ThreadKey,
    pub worktree: &'a str,
    pub base_branch: &'a str,
}

/// Folds a finished run into `watch`, ticks the statement lock once PR duty approved this
/// head, and starts a run when the current head has none.
pub(crate) fn observe(application: &AppHandle, target: &ProofTarget, watch: &mut FollowThrough) {
    if let Some((head, card)) = take_finished(target.key) {
        watch.record_proofs(&head, card);
    }
    if watch.proof.card.is_some()
        && let Some(locked_head) = approved_head(application, target.key.0, watch.number)
    {
        watch.statements_locked(&locked_head);
    }
    if watch.needs_proof_check() {
        start(target, &watch.head);
    }
}

fn take_finished(key: ThreadKey) -> Option<(String, Option<ProofCard>)> {
    let mut jobs = jobs().lock().ok()?;
    if !matches!(jobs.get(&key), Some(Job::Finished { .. })) {
        return None;
    }
    match jobs.remove(&key) {
        Some(Job::Finished { head, card }) => Some((head, card)),
        _ => None,
    }
}

/// The head PR duty's last approval locked, read from the statement lock it saved.
fn approved_head(application: &AppHandle, project: u32, number: u32) -> Option<String> {
    let app_data = application.path().app_data_dir().ok()?;
    velocity_proofs::load_statement_lock(&app_data, &project.to_string(), number)
        .ok()
        .flatten()
        .map(|lock| lock.head)
}

/// One run per thread at a time; a run for an older head finishes and is then dropped.
fn start(target: &ProofTarget, head: &str) {
    let Ok(mut running) = jobs().lock() else {
        return;
    };
    if matches!(running.get(&target.key), Some(Job::Running)) {
        return;
    }
    running.insert(target.key, Job::Running);
    drop(running);
    let key = target.key;
    let worktree = PathBuf::from(target.worktree);
    let base_branch = target.base_branch.to_owned();
    let head = head.to_owned();
    thread::spawn(move || {
        let base = velocity_proofs::base_reference(&worktree, &base_branch);
        let finished = run(&worktree, &base, &head);
        if let Ok(mut done) = jobs().lock() {
            match finished {
                Some(card) => done.insert(key, Job::Finished { head, card }),
                None => done.remove(&key),
            };
        }
    });
}

/// `None` leaves the head unchecked for the next observation to retry: the worktree is not at
/// the PR head yet (the agent has local commits or proof edits), or the checks could not run.
fn run(worktree: &Path, base: &str, head: &str) -> Option<Option<ProofCard>> {
    match velocity_proofs::checkout_at(worktree, head) {
        Ok(true) => {}
        Ok(false) => return None,
        Err(error) => {
            eprintln!("Proof checks in {}: {error}", worktree.display());
            return None;
        }
    }
    // A statement lock belongs to an approval of this exact head, which comes after these
    // checks; `observe` ticks the card's lock check when it appears.
    match velocity_proofs::proof_card(worktree, base, head, None) {
        Ok(card) => Some(card),
        Err(error) => {
            eprintln!("Proof checks in {}: {error}", worktree.display());
            None
        }
    }
}
