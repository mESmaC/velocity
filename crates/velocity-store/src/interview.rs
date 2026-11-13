//! What `state.json` keeps of a spec interview (SPEC.md F3): enough to show the thread
//! again after a restart and to resume its agent from where it stopped.

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::projects::ProjectError;
use crate::{PersistedState, ProjectItem, StateStore};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum InterviewStage {
    Scan,
    Ask,
    Ready,
    Locked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct InterviewOption {
    pub label: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub recommended: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct InterviewQuestion {
    pub key: String,
    pub text: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub options: Vec<InterviewOption>,
}

/// An answer, recorded as `key: value` (F3 Ask).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct InterviewDecision {
    pub question: String,
    pub key: String,
    pub value: String,
}

/// One feature the Ready card proposes for a Lean proof (F3, F17). You switch entries off
/// before locking; the ones left on are written into SPEC.md.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProofListEntry {
    pub feature: String,
    pub included: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct InterviewRecord {
    pub stage: InterviewStage,
    pub brief: String,
    #[serde(default)]
    pub summary: String,
    /// The question waiting for an answer, if any.
    #[serde(default)]
    pub question: Option<InterviewQuestion>,
    #[serde(default)]
    pub decisions: Vec<InterviewDecision>,
    #[serde(default)]
    pub proof_list: Vec<ProofListEntry>,
    #[serde(default)]
    pub notice: Option<String>,
}

impl StateStore {
    /// Applies `change` to project `project_identifier` and saves. Nothing changes in memory
    /// unless both `change` and the save succeed, so a failure leaves the store as it was.
    pub fn update_project<Value, Failure: From<ProjectError>>(
        &mut self,
        project_identifier: u32,
        change: impl FnOnce(&mut ProjectItem) -> Result<Value, Failure>,
    ) -> Result<Value, Failure> {
        let mut candidate = self.state.clone();
        let project = candidate
            .projects
            .iter_mut()
            .find(|project| project.id == project_identifier)
            .ok_or(ProjectError::UnknownProject(project_identifier))?;
        let value = change(project)?;
        self.save_candidate(candidate)?;
        Ok(value)
    }

    fn save_candidate(&mut self, candidate: PersistedState) -> Result<(), ProjectError> {
        if candidate == self.state {
            return Ok(());
        }
        self.store_snapshot(candidate)?;
        Ok(())
    }

    /// The active project's identifier, if any project is open.
    pub fn active_project_identifier(&self) -> Option<u32> {
        crate::project_layout::active_project(&self.state).map(|project| project.id)
    }

    /// Rebuilds the main window's read model from the saved state without touching what
    /// is being typed, searched or selected.
    pub fn refresh_layout(&self, layout: &mut velocity_core::layout_state::LayoutState) {
        layout.replace_data(crate::project_layout::read_model(&self.state));
        crate::build_gate::sync_layout(&self.state, layout);
    }
}
