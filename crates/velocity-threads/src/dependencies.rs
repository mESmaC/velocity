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
        if waiting.holder.contains('/') {
            continue;
        }
        let holder = project.threads.iter().find(|thread| {
            thread.state != ThreadState::Spec && thread.matches_airc_member(&waiting.holder)
        });
        // Duty and spec members have their own lifecycle; absence from the build
        // thread list says nothing about whether their work has ended.
        if holder.is_none()
            && velocity_core::thread_titles::thread_id_from_member(&waiting.holder).is_none()
        {
            continue;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use velocity_store::ThreadItem;

    fn temp_dir(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "velocity-deps-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&path)?;
        Ok(path)
    }

    fn record_wait(
        channel: &Channel,
        holder_member: &str,
        holder_nick: &str,
        waiter_member: &str,
        waiter_nick: &str,
        file: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        channel.join(holder_member, holder_nick)?;
        channel.join(waiter_member, waiter_nick)?;
        channel.claim(
            holder_member,
            "Holder work",
            "vel/holder",
            vec![file.into()],
        )?;
        channel.claim(
            waiter_member,
            "Waiter work",
            "vel/waiter",
            vec![file.into()],
        )?;
        channel.begin_wait(waiter_member, holder_nick, file)?;
        Ok(())
    }

    fn saved_build_thread(id: u32, agent_name: &str, state: ThreadState) -> ThreadItem {
        let mut thread = ThreadItem::new(id, format!("Thread {id}"));
        thread.agent_name = Some(agent_name.to_owned());
        thread.state = state;
        thread
    }

    // Risk (#678): build threads with `spec-*` or pre-fix `duty-*` agent names must wake
    // dependency waiters when they fail or are deleted, while actual duty, spec-interview
    // and foreign holders must stay parked.
    #[test]
    fn refresh_wakes_reserved_slug_build_holders_and_skips_non_thread_holders()
    -> Result<(), Box<dyn std::error::Error>> {
        let data = temp_dir("reserved-slug")?;
        let channel = Channel::open(&data, "1")?;
        let cases = [
            ("spec-cleanup-50", "spec-cleanup-50", 60, "src/a.rs"),
            ("spec-deleted-51", "spec-deleted-51", 61, "src/b.rs"),
            ("duty-cleanup-52", "duty-cleanup-52", 62, "src/c.rs"),
            ("duty-scout-1", "scout-1", 63, "src/d.rs"),
            ("spec-12", "spec", 64, "src/e.rs"),
            (
                "0123456789abcdef/spec-cleanup-50",
                "remote-spec-50",
                65,
                "src/f.rs",
            ),
        ];
        let mut project = ProjectItem::new(1, "velocity".into(), data.to_string_lossy().into());
        project.threads.extend([
            saved_build_thread(50, "spec-cleanup-50", ThreadState::Error),
            saved_build_thread(52, "duty-cleanup-52", ThreadState::Error),
        ]);
        for (holder_member, holder_nick, waiter_id, file) in cases {
            let waiter = format!("waiter-{waiter_id}");
            record_wait(&channel, holder_member, holder_nick, &waiter, &waiter, file)?;
            project
                .threads
                .push(saved_build_thread(waiter_id, &waiter, ThreadState::Working));
        }

        refresh(&channel, &project)?;

        let remaining: Vec<String> = channel
            .dependency_waits()?
            .into_iter()
            .map(|wait| wait.holder)
            .collect();
        for ended in ["spec-cleanup-50", "spec-deleted-51", "duty-cleanup-52"] {
            assert!(!remaining.iter().any(|holder| holder == ended), "{ended}");
        }
        for parked in [
            "duty-scout-1",
            "spec-12",
            "0123456789abcdef/spec-cleanup-50",
        ] {
            assert!(remaining.iter().any(|holder| holder == parked), "{parked}");
        }

        std::fs::remove_dir_all(data)?;
        Ok(())
    }
}
