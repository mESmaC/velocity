//! Which piece a build thread holds in the AIRC claim lock (F14, F16).
use velocity_core::work_pool::WorkKey;

use crate::ThreadItem;

/// A dispatched issue, a SPEC.md section or a Task List item locks on its work-pool
/// identity, so it collides with any other agent on the same item. A free-form composer
/// thread owns no pool item: it locks on its branch, which is unique per project. Its
/// title is display text only, and two threads may share one (#100).
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
    format!("branch:{branch}")
}
