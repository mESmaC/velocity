//! F14 runner lifecycle to durable dependency wake events and sidebar descriptions.
use velocity_airc::{Channel, Error, waits::DependencyWait};
use velocity_store::{ProjectItem, ThreadState};

pub fn refresh(
    channel: &Channel,
    project: &ProjectItem,
) -> Result<Vec<(u32, Option<String>)>, Error> {
    let waits = channel.dependency_waits()?;
    for waiting in &waits {
        // Foreign Rosen members retain their machine prefix, so the check below
        // leaves their lifecycle to the holder's machine and the shared server.
        // Duty and spec members have their own lifecycle; absence from the build
        // thread list says nothing about whether their work has ended.
        if velocity_core::thread_titles::thread_id_from_member(&waiting.holder).is_none() {
            continue;
        }
        let holder = project
            .threads
            .iter()
            .find(|thread| thread.matches_airc_member(&waiting.holder));
        let outcome = match holder.map(|thread| thread.state) {
            Some(ThreadState::Merged) => Some("merged"),
            Some(ThreadState::Closed) => Some("closed"),
            Some(ThreadState::Error) => Some("failed"),
            None => Some("deleted"),
            _ => None,
        };
        if let Some(outcome) = outcome {
            channel.dependency_ended(
                &waiting.holder,
                outcome,
                holder.and_then(|thread| thread.pull_request_number),
            )?;
        }
    }
    Ok(project
        .threads
        .iter()
        .filter_map(|thread| {
            // Rosen path and overlap waits own `dependency_wait` on Rosen threads (#531, #565).
            if thread.has_rosen_wait() {
                return None;
            }
            let description = waits
                .iter()
                .find(|wait| thread.matches_airc_member(&wait.member))
                .map(|wait| description(project, wait));
            (description != thread.dependency_wait).then_some((thread.id, description))
        })
        .collect())
}

fn description(project: &ProjectItem, wait: &DependencyWait) -> String {
    let number = project
        .threads
        .iter()
        .find(|other| other.matches_airc_member(&wait.holder))
        .and_then(|other| other.pull_request_number)
        .map_or(String::new(), |number| format!(" · #{number}"));
    format!(
        "waiting for {}{number} · shares {}",
        wait.holder_nick, wait.file
    )
}
