//! Which piece a build thread holds in the AIRC claim lock (F14, F16).
use velocity_core::work_pool::WorkKey;

use crate::ThreadItem;

/// A dispatched issue, a SPEC.md section or a Task List item locks on its work-pool
/// identity, so it collides with any other agent on the same item. A free-form composer
/// thread owns no pool item: a Rosen thread locks on its change ID (`change:<id>`) and a
/// git thread locks on its branch, both unique per project. Its title is display text only,
/// and two threads may share one (#100, #531).
pub fn thread_claim_piece(thread: &ThreadItem, branch: &str) -> String {
    if let Some(issue) = thread.issue.as_ref().filter(|issue| issue.number != 0) {
        return WorkKey::Issue {
            repository: issue.repository.to_lowercase(),
            number: issue.number,
        }
        .claim_piece();
    }
    if let Some(section) = thread.spec_section.as_ref() {
        return WorkKey::Spec {
            section: section.clone(),
        }
        .claim_piece();
    }
    if let Some(identifier) = thread.task_ids.first() {
        return WorkKey::Task {
            identifier: *identifier,
        }
        .claim_piece();
    }
    if let Some(change) = thread
        .rosen_change
        .as_deref()
        .map(str::trim)
        .filter(|change| !change.is_empty())
    {
        return format!("change:{change}");
    }
    format!("branch:{branch}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rosen::RosenPathWait;

    // Risk (#531): free-form Rosen threads must lock on `change:<id>` rather than `branch:vel/...`,
    // and persisted Rosen path waits must survive save/load round-trips.
    #[test]
    fn rosen_piece_uses_change_id_and_wait_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let mut rosen = ThreadItem::new(1, "Free-form Rosen".into());
        rosen.rosen_change = Some("kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk".into());
        assert_eq!(
            thread_claim_piece(&rosen, "vel/free-form"),
            "change:kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk"
        );

        let git = ThreadItem::new(2, "Free-form Git".into());
        assert_eq!(
            thread_claim_piece(&git, "vel/free-form"),
            "branch:vel/free-form"
        );

        rosen.rosen_path_wait = Some(RosenPathWait::new(
            vec!["src/lib.rs".into()],
            vec!["other-agent".into()],
        ));
        let json = serde_json::to_string(&rosen)?;
        let restored: ThreadItem = serde_json::from_str(&json)?;
        assert_eq!(restored.rosen_path_wait, rosen.rosen_path_wait);
        Ok(())
    }
}
