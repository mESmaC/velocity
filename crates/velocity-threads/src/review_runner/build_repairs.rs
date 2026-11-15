//! Repair scheduling runs independently of the build worker and of staffed duty roles.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use velocity_rosen::{ProjectServer, build_repairs::BuildFailure};

use super::{PrDutyReviews, ReviewHost};

/// A fresh server observation. A passing test never closes a build culprit.
pub struct BuildRepairs {
    pub pending: Vec<BuildFailure>,
    pub settled: Vec<String>,
}

impl PrDutyReviews {
    pub(super) fn repair_builds<Host: ReviewHost>(&self, host: &Host, project: u32) {
        {
            let mut state = self.lock();
            if let Some(again) = state.repairing_projects.get_mut(&project) {
                *again = true;
                return;
            }
            state.repairing_projects.insert(project, false);
        }
        let (runner, host) = (self.clone(), host.clone());
        tokio::spawn(async move {
            let server = ProjectServer::new(&runner.app_data, project);
            let result = observe(&server).await;
            let result = match result {
                Ok((observation, deadline)) => {
                    if let Some(deadline) = deadline {
                        runner.repair_at(project, deadline);
                    }
                    tokio::task::spawn_blocking(move || host.repair_builds(project, observation))
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|result| result)
                }
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                eprintln!("Build repair for project {project}: {error}");
            }
            let again = runner.lock().repairing_projects.remove(&project) == Some(true);
            if again {
                runner.wake();
            }
        });
    }

    /// A durable recorded time sets the deadline, so restarts do not reset two minutes.
    fn repair_at(&self, project: u32, deadline: u64) {
        {
            let mut state = self.lock();
            if state
                .repair_deadlines
                .get(&project)
                .is_some_and(|existing| *existing <= deadline)
            {
                return;
            }
            state.repair_deadlines.insert(project, deadline);
        }
        let runner = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(deadline.saturating_sub(now()))).await;
            let current = {
                let mut state = runner.lock();
                if state.repair_deadlines.get(&project) == Some(&deadline) {
                    state.repair_deadlines.remove(&project);
                    true
                } else {
                    false
                }
            };
            if current {
                runner.wake();
            }
        });
    }
}

async fn observe(server: &ProjectServer) -> Result<(BuildRepairs, Option<u64>), String> {
    let failures = velocity_rosen::build_repairs::build_failures(server)
        .await
        .map_err(|error| error.to_string())?;
    let commits: Vec<_> = failures
        .iter()
        .map(|failure| failure.commit.clone())
        .collect();
    let mut settled = velocity_rosen::issue_build_coverage(server, &commits)
        .await
        .map_err(|error| error.to_string())?;
    settled.extend(
        failures
            .iter()
            .filter(|failure| failure.reverted)
            .map(|failure| failure.commit.clone()),
    );
    let now = now();
    let deadline = failures
        .iter()
        .filter(|failure| {
            !failure.reverted && !failure.needs_repair(now) && !settled.contains(&failure.commit)
        })
        .map(|failure| failure.recorded_at.saturating_add(120_000))
        .min();
    let pending = failures
        .into_iter()
        .filter(|failure| failure.needs_repair(now) && !settled.contains(&failure.commit))
        .collect();
    Ok((BuildRepairs { pending, settled }, deadline))
}

fn now() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}
