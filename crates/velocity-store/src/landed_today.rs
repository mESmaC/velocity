//! The title bar's "landed today" count (F1 Pulse): a project's lands since local midnight.

use chrono::{Datelike, Local, TimeZone};

use crate::ProjectItem;

/// Today as a day number in local time. The count starts again when it changes.
pub(crate) fn local_day() -> u32 {
    day_number(Local::now().date_naive())
}

/// The local calendar day for a UTC Unix-millisecond timestamp, or today when unavailable.
pub(crate) fn local_day_from_millis(timestamp_ms: Option<i64>) -> u32 {
    timestamp_ms
        .and_then(|millis| Local.timestamp_millis_opt(millis).single())
        .map_or_else(local_day, |time| day_number(time.date_naive()))
}

fn day_number(date: chrono::NaiveDate) -> u32 {
    u32::try_from(date.num_days_from_ce()).unwrap_or_default()
}

impl ProjectItem {
    /// Counts one more land on `day`, ignoring earlier days once a newer day is active and
    /// skipping any land whose deduplication key was already counted on `day` (#644).
    pub(crate) fn record_land(&mut self, day: u32, key: Option<&str>) -> bool {
        if day < self.merged_day {
            return false;
        }
        if self.merged_day != day {
            self.merged_day = day;
            self.merged_today = 0;
            self.landed_today_keys.clear();
        }
        if let Some(key) = key.filter(|key| !key.is_empty()) {
            if self.landed_today_keys.iter().any(|seen| seen == key) {
                return false;
            }
            self.landed_today_keys.push(key.to_owned());
        }
        self.merged_today = self.merged_today.saturating_add(1);
        true
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
    use super::local_day_from_millis;
    use crate::{ProjectItem, StateStore};

    // Risk (#497, #644): the day stamp and deduplication keys are persisted fields. Without
    // them surviving save and load, a restart could drop today's lands, double-count a replay,
    // carry yesterday's count into today, or let an earlier day's replay wipe today's count.
    #[test]
    fn todays_lands_survive_a_restart_and_end_with_the_day()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory =
            std::env::temp_dir().join(format!("velocity-landed-today-{}", std::process::id()));
        let _ignored = std::fs::remove_dir_all(&directory);
        let mut store = StateStore::load_or_default(&directory)?;
        let mut project = ProjectItem::new(1, "One".into(), r"C:\project one".into());
        assert!(project.record_land(700_000, Some("commit:aaa")));
        assert!(!project.record_land(700_000, Some("commit:aaa")));
        assert!(project.record_land(700_000, Some("commit:bbb")));
        let mut next = store.state().clone();
        next.projects.push(project);
        store.store_snapshot(next)?;

        let reloaded = StateStore::load_or_default(&directory)?;
        let mut project = reloaded.state().projects[0].clone();
        assert_eq!(project.landed_on(700_000), 2);
        assert!(!project.record_land(700_000, Some("commit:aaa")));
        assert_eq!(project.landed_on(700_000), 2);
        assert_eq!(project.landed_on(700_001), 0);
        assert!(project.record_land(700_001, Some("commit:ccc")));
        assert_eq!(project.landed_on(700_001), 1);
        assert!(!project.record_land(700_000, Some("commit:late-yesterday")));
        assert_eq!(project.landed_on(700_001), 1);

        let today_date = chrono::Local::now().date_naive();
        let yesterday_date = today_date.pred_opt().ok_or("missing previous day")?;
        let noon = chrono::NaiveTime::from_hms_opt(12, 0, 0).ok_or("invalid noon")?;
        let today_ms =
            chrono::TimeZone::from_local_datetime(&chrono::Local, &today_date.and_time(noon))
                .earliest()
                .ok_or("unrepresentable local today")?
                .timestamp_millis();
        let yesterday_ms =
            chrono::TimeZone::from_local_datetime(&chrono::Local, &yesterday_date.and_time(noon))
                .earliest()
                .ok_or("unrepresentable local yesterday")?
                .timestamp_millis();
        assert_eq!(
            local_day_from_millis(Some(yesterday_ms)) + 1,
            local_day_from_millis(Some(today_ms))
        );
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
