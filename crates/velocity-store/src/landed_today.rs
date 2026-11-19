//! The title bar's "landed today" count (F1 Pulse): a project's lands since local midnight.

use crate::ProjectItem;

/// Today as a day number in local time. The count starts again when it changes.
pub(crate) fn local_day() -> u32 {
    use chrono::Datelike;
    u32::try_from(chrono::Local::now().date_naive().num_days_from_ce()).unwrap_or_default()
}

impl ProjectItem {
    /// Counts one more land on `day`, dropping a count left from an earlier day.
    pub(crate) fn record_land(&mut self, day: u32) {
        if self.merged_day != day {
            self.merged_day = day;
            self.merged_today = 0;
        }
        self.merged_today = self.merged_today.saturating_add(1);
    }

    /// The lands counted on `day`. A count left from an earlier day reads as none, so the
    /// pill clears after midnight without a save.
    pub(crate) fn landed_on(&self, day: u32) -> u32 {
        if self.merged_day == day {
            self.merged_today
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{ProjectItem, StateStore};

    // Risk (#497): the day stamp is a new persisted field. Without it surviving save and
    // load, a restart would either drop today's lands or carry yesterday's into today.
    #[test]
    fn todays_lands_survive_a_restart_and_end_with_the_day()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory =
            std::env::temp_dir().join(format!("velocity-landed-today-{}", std::process::id()));
        let _ignored = std::fs::remove_dir_all(&directory);
        let mut store = StateStore::load_or_default(&directory)?;
        let mut project = ProjectItem::new(1, "One".into(), r"C:\project one".into());
        project.record_land(700_000);
        project.record_land(700_000);
        let mut next = store.state().clone();
        next.projects.push(project);
        store.store_snapshot(next)?;

        let reloaded = StateStore::load_or_default(&directory)?;
        let mut project = reloaded.state().projects[0].clone();
        assert_eq!(project.landed_on(700_000), 2);
        assert_eq!(project.landed_on(700_001), 0);
        project.record_land(700_001);
        assert_eq!(project.landed_on(700_001), 1);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
