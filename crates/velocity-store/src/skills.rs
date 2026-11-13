//! Save project choices atomically before acknowledging a settings edit (F18).
use crate::{StateStore, projects::ProjectError};
use velocity_core::skills::ProjectSkills;
impl StateStore {
    pub fn save_project_skills(
        &mut self,
        identifier: u32,
        skills: ProjectSkills,
    ) -> Result<(), ProjectError> {
        let mut next = self.state.clone();
        let project = next
            .projects
            .iter_mut()
            .find(|project| project.id == identifier)
            .ok_or(ProjectError::UnknownProject(identifier))?;
        project.skills = skills;
        self.store_snapshot(next)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProjectItem;
    use velocity_core::skills::SkillSettingsEdit;

    // Losing project skill choices on restart could give agents the wrong instructions.
    #[test]
    fn project_choices_survive_save_and_reload() -> Result<(), Box<dyn std::error::Error>> {
        let directory =
            std::env::temp_dir().join(format!("velocity-skill-choices-{}", std::process::id()));
        let mut store = StateStore::load_or_default(&directory)?;
        store.state.projects = vec![
            ProjectItem::new(1, "One".into(), "C:\\project one".into()),
            ProjectItem::new(2, "Two".into(), "C:\\project two".into()),
        ];
        let mut choices = ProjectSkills::default();
        choices.apply(SkillSettingsEdit::Pack {
            identifier: "cskills".into(),
            enabled: true,
        });
        store.save_project_skills(1, choices.clone())?;
        let loaded = StateStore::load_or_default(&directory)?;
        assert_eq!(loaded.state().projects[0].skills, choices);
        assert_eq!(loaded.state().projects[1].skills, ProjectSkills::default());
        let mut legacy = serde_json::to_value(&loaded.state().projects[0])?;
        if let Some(object) = legacy.as_object_mut() {
            object.remove("skills");
        }
        let legacy: ProjectItem = serde_json::from_value(legacy)?;
        assert_eq!(legacy.skills, ProjectSkills::default());
        std::fs::remove_file(directory.join(crate::STATE_FILE_NAME))?;
        std::fs::remove_dir(directory)?;
        Ok(())
    }
}
