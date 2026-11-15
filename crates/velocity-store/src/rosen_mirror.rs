//! Durable project mirror state, independent of the thread event cursor (F5, #402).

use velocity_core::rosen_mirror::MirrorState;

use crate::{StateStore, StoreError};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MirrorRejection {
    #[serde(default)]
    pub sequence: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub state: Option<MirrorState>,
}

impl StateStore {
    pub fn save_rosen_mirror(
        &mut self,
        project: u32,
        sequence: u64,
        state: Option<MirrorState>,
    ) -> Result<(), StoreError> {
        let Some(current) = self.project(project) else {
            return Ok(());
        };
        if sequence < current.rosen_mirror_sequence
            || (sequence == current.rosen_mirror_sequence && state == current.rosen_mirror)
        {
            return Ok(());
        }
        let mut next = self.state.clone();
        if let Some(item) = next.projects.iter_mut().find(|item| item.id == project) {
            if state.as_ref().is_some_and(|state| state.rejected)
                && item.rosen_mirror_rejection_sequence.is_none()
            {
                item.rosen_mirror_rejection_sequence = Some(sequence);
                item.rosen_mirror_notices.push(MirrorRejection {
                    sequence,
                    name: item.name.clone(),
                    state: state.clone(),
                });
            }
            if state.is_none() {
                item.rosen_mirror_rejection_sequence = None;
            }
            item.rosen_mirror = state;
            item.rosen_mirror_sequence = sequence;
        }
        self.store_snapshot(next)
    }

    pub fn finish_mirror_notice(&mut self, project: u32, sequence: u64) -> Result<(), StoreError> {
        let mut next = self.state.clone();
        if let Some(item) = next.projects.iter_mut().find(|item| item.id == project) {
            item.rosen_mirror_notices
                .retain(|notice| notice.sequence != sequence);
        }
        self.store_snapshot(next)
    }
}
