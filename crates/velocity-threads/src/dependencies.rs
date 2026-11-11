//! F14 runner lifecycle to durable dependency wake events and sidebar descriptions.
use velocity_airc::{Channel, Error, waits::DependencyWait};
use velocity_store::{ProjectItem, ThreadState};

pub fn refresh(
    channel: &Channel,
    project: &ProjectItem,
) -> Result<Vec<(u32, Option<String>)>, Error> {
    let waits = channel.dependency_waits()?;
    for waiting in &waits {
        // Duty and spec members have their own lifecycle; absence from the build
        // thread list says nothing about whether their work has ended.
        if !waiting.holder.starts_with("thread-") {
            continue;
        }
        let holder = project
            .threads
            .iter()
            .find(|thread| waiting.holder == format!("thread-{}", thread.id));
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
            let description = waits
                .iter()
                .find(|wait| wait.member == format!("thread-{}", thread.id))
                .map(|wait| description(project, wait));
            (description != thread.dependency_wait).then_some((thread.id, description))
        })
        .collect())
}

fn description(project: &ProjectItem, wait: &DependencyWait) -> String {
    let number = project
        .threads
        .iter()
        .find(|other| wait.holder == format!("thread-{}", other.id))
        .and_then(|other| other.pull_request_number)
        .map_or(String::new(), |number| format!(" · #{number}"));
    format!(
        "waiting for {}{number} · shares {}",
        wait.holder_nick, wait.file
    )
}
