//! Durable context hand-off state, with experiment outcomes scoped to a Rosen change.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::rosen_issue::IssueObservation;
use crate::{ReviewStage, StateStore, StoreError, ThreadItem, ThreadState};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub change: String,
    pub text: String,
    /// A replacement may fail to start or be interrupted before its opening turn completes.
    pub pending: bool,
}

pub fn note_for(thread: &ThreadItem) -> Option<&Note> {
    thread
        .context_handoff
        .as_ref()
        .filter(|note| thread.rosen_change.as_deref() == Some(note.change.as_str()))
}

pub fn can_handoff(thread: &ThreadItem) -> bool {
    thread.interview.is_none()
        && (thread.state == ThreadState::Working
            || (thread.state == ThreadState::Review
                && thread.review_stage == Some(ReviewStage::Fixing)))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub enabled: bool,
    pub configuration: String,
    pub source: String,
    pub crossed_at: Option<u64>,
    pub handed_off: bool,
    pub landed: Option<String>,
    pub reverts: u32,
    pub requested_rounds: u32,
    /// One verdict per reviewed head or land. An unreviewed mark withdraws its verdict.
    pub verdicts: BTreeMap<String, String>,
    sequence: u64,
    #[serde(default)]
    last_reverted: Option<String>,
}

impl Item {
    fn new(thread: &ThreadItem, enabled: bool) -> Self {
        Self {
            enabled,
            configuration: configuration(thread),
            source: source(thread).into(),
            crossed_at: None,
            handed_off: false,
            landed: None,
            reverts: 0,
            requested_rounds: 0,
            verdicts: BTreeMap::new(),
            sequence: 0,
            last_reverted: None,
        }
    }

    pub fn review_rounds(&self) -> u32 {
        self.requested_rounds
            .saturating_add(u32::try_from(self.verdicts.len()).unwrap_or(u32::MAX))
    }

    pub(crate) fn observe_land(&mut self, sequence: u64, event: &IssueObservation<'_>) -> bool {
        if sequence <= self.sequence {
            return false;
        }
        match event {
            IssueObservation::Landed(commit) => {
                if self.last_reverted.as_deref() == Some(commit) {
                    return false;
                }
                self.landed = Some((*commit).into());
            }
            IssueObservation::Reverted { commit, .. } => {
                if commit.is_some() && *commit == self.last_reverted.as_deref() {
                    return false;
                }
                self.last_reverted = commit.map(str::to_owned).or_else(|| self.landed.clone());
                self.landed = None;
                self.reverts = self.reverts.saturating_add(1);
            }
            IssueObservation::BuildPassed(_) => return false,
        }
        self.sequence = sequence;
        true
    }

    pub(crate) fn rewind_events(&mut self, replacing: bool, latest: u64) {
        if replacing || self.sequence > latest {
            self.sequence = 0;
        }
    }
}

fn configuration(thread: &ThreadItem) -> String {
    format!(
        "{:?} / {} / {:?} / {:?}",
        thread.provider, thread.model_name, thread.effort, thread.effective_speed
    )
}

fn source(thread: &ThreadItem) -> &str {
    if thread.issue.is_some() {
        "Issue"
    } else if !thread.task_ids.is_empty() {
        "Task"
    } else if thread.spec_section.is_some() {
        "Spec section"
    } else {
        "Prompt"
    }
}

impl StateStore {
    /// Freeze the switch per change so editing Settings cannot reclassify past tokens.
    pub fn enroll_handoff(
        &mut self,
        project: u32,
        thread: u32,
    ) -> Result<Option<(String, bool)>, StoreError> {
        let Some(project_item) = self.project(project) else {
            return Ok(None);
        };
        let Some(thread) = project_item.threads.iter().find(|item| item.id == thread) else {
            return Ok(None);
        };
        let Some(change) = thread
            .rosen_change
            .clone()
            .filter(|_| thread.interview.is_none())
        else {
            return Ok(None);
        };
        if let Some(item) = project_item.handoff_items.get(&change) {
            return Ok(Some((change, item.enabled)));
        }
        // Already running work has earlier turns with no change on their ledger rows.
        if thread.agent_session.is_some() {
            return Ok(None);
        }
        let setting = self.state.settings.agents.context_handoff;
        let item = Item::new(thread, setting);
        let mut next = self.state.clone();
        if let Some(project) = next.projects.iter_mut().find(|item| item.id == project) {
            project.handoff_items.insert(change.clone(), item);
            self.store_snapshot(next)?;
        }
        Ok(Some((change, setting)))
    }

    pub fn observe_handoff(
        &mut self,
        (project, thread): (u32, u32),
        change: &str,
        note: Option<&str>,
    ) -> Result<bool, StoreError> {
        let mut next = self.state.clone();
        let Some(project) = next.projects.iter_mut().find(|item| item.id == project) else {
            return Ok(false);
        };
        let Some(thread) = project.threads.iter_mut().find(|item| item.id == thread) else {
            return Ok(false);
        };
        if thread.rosen_change.as_deref() != Some(change) || !can_handoff(thread) {
            return Ok(false);
        }
        let Some(item) = project.handoff_items.get_mut(change) else {
            return Ok(false);
        };
        item.crossed_at.get_or_insert_with(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        });
        item.source = source(thread).into();
        let ready = note.is_some() && item.enabled;
        if let Some(text) = note.filter(|_| ready) {
            thread.context_handoff = Some(Note {
                change: change.into(),
                text: text.into(),
                pending: true,
            });
        }
        if next != self.state {
            self.store_snapshot(next)?;
        }
        Ok(ready)
    }

    pub fn complete_handoff(&mut self, (project, thread): (u32, u32)) -> Result<(), StoreError> {
        let mut next = self.state.clone();
        let Some(project) = next.projects.iter_mut().find(|item| item.id == project) else {
            return Ok(());
        };
        let Some(thread) = project.threads.iter_mut().find(|item| item.id == thread) else {
            return Ok(());
        };
        let Some(change) = note_for(thread)
            .filter(|note| note.pending)
            .map(|note| note.change.clone())
        else {
            return Ok(());
        };
        if let Some(item) = project.handoff_items.get_mut(&change) {
            item.handed_off = true;
        }
        if let Some(note) = &mut thread.context_handoff {
            note.pending = false;
        }
        self.store_snapshot(next)
    }

    pub fn observe_handoff_review(
        &mut self,
        project: u32,
        change: &str,
        target: String,
        verdict: Option<&str>,
    ) -> Result<(), StoreError> {
        if self
            .project(project)
            .is_none_or(|project| !project.handoff_items.contains_key(change))
        {
            return Ok(());
        }
        let mut next = self.state.clone();
        if let Some(item) = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .and_then(|project| project.handoff_items.get_mut(change))
        {
            if let Some(verdict) = verdict {
                item.verdicts.insert(target, verdict.into());
            } else {
                item.verdicts.remove(&target);
            }
        }
        if next != self.state {
            self.store_snapshot(next)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[test]
fn handoff_note_and_cohort_survive_restart() -> Result<(), Box<dyn std::error::Error>> {
    // A crash between the old and fresh sessions must preserve the note and cohort together.
    use crate::{PersistedState, ProjectItem};

    let directory =
        std::env::temp_dir().join(format!("velocity-context-handoff-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let mut state = PersistedState::default();
    state.settings.agents.context_handoff = true;
    let mut project = ProjectItem::new(1, "Test".into(), ".".into());
    let mut thread = ThreadItem::new(2, "Implement work".into());
    thread.rosen_change = Some("change".into());
    project.threads.push(thread);
    state.projects.push(project);
    std::fs::write(directory.join("state.json"), serde_json::to_vec(&state)?)?;
    let mut store = StateStore::load_or_default(&directory)?;
    assert_eq!(store.enroll_handoff(1, 2)?, Some(("change".into(), true)));
    assert!(store.observe_handoff((1, 2), "change", Some("Check the fix"))?);
    let mut settings = store.settings();
    settings.agents.context_handoff = false;
    store.set_settings(settings)?;
    drop(store);

    let mut store = StateStore::load_or_default(&directory)?;
    assert_eq!(store.enroll_handoff(1, 2)?, Some(("change".into(), true)));
    let project = store.project(1).ok_or("missing project")?;
    let note = note_for(&project.threads[0]).ok_or("missing hand-off")?;
    assert!(note.pending);
    assert_eq!(note.text, "Check the fix");
    assert!(project.handoff_items["change"].crossed_at.is_some());
    store.complete_handoff((1, 2))?;
    let store = StateStore::load_or_default(&directory)?;
    let project = store.project(1).ok_or("missing project")?;
    assert!(project.handoff_items["change"].handed_off);
    assert!(note_for(&project.threads[0]).is_some_and(|note| !note.pending));
    std::fs::remove_dir_all(directory)?;
    Ok(())
}
