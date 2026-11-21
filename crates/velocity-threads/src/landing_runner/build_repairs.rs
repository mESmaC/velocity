//! Repair scheduling runs independently of the build worker and of staffed duty roles.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use velocity_rosen::{ProjectServer, build_repairs::BuildFailure};

use super::{LandingHost, RosenLanding};

/// A fresh server observation. A green build or test pass at or after the fix closes
/// the build repair (#567).
pub struct BuildRepairs {
    pub pending: Vec<BuildFailure>,
    pub settled: Vec<String>,
}

/// An open build repair's culprit commit and, when known, its repair thread's fix commit/change.
struct OpenRepair {
    culprit: String,
    fix_commit: Option<String>,
    fix_change: Option<String>,
}

impl RosenLanding {
    pub(super) fn repair_builds<Host: LandingHost>(&self, host: &Host, project: u32) {
        {
            let mut state = self.lock();
            if let Some(again) = state.repairing_projects.get_mut(&project) {
                *again = true;
                return;
            }
            state.repairing_projects.insert(project, false);
        }
        let open_repairs = open_repairs_for_project(host, project);
        let (runner, host) = (self.clone(), host.clone());
        tokio::spawn(async move {
            let server = ProjectServer::new(&runner.app_data, project);
            let result = observe(&server, open_repairs).await;
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

fn open_repairs_for_project<Host: LandingHost>(host: &Host, project: u32) -> Vec<OpenRepair> {
    let Some(project) = host
        .projects()
        .into_iter()
        .find(|item| item.project.id == project)
        .map(|item| item.project)
    else {
        return Vec::new();
    };
    project
        .build_repairs
        .iter()
        .filter(|repair| !repair.closed)
        .map(|repair| {
            let thread = project
                .threads
                .iter()
                .find(|thread| thread.id == repair.thread);
            let fix_commit = thread.and_then(|thread| {
                thread
                    .rosen_issue
                    .landed_commit()
                    .map(str::to_owned)
                    .or_else(|| {
                        thread
                            .rosen_change_view
                            .as_ref()?
                            .lands
                            .last()
                            .filter(|land| land.revert.is_none())
                            .map(|land| land.commit.clone())
                    })
            });
            let fix_change = thread.and_then(|thread| thread.rosen_change.clone());
            OpenRepair {
                culprit: repair.commit.clone(),
                fix_commit,
                fix_change,
            }
        })
        .collect()
}

async fn resolve_repair_targets(
    server: &ProjectServer,
    open_repairs: Vec<OpenRepair>,
) -> Vec<(String, String)> {
    let mut targets = Vec::with_capacity(open_repairs.len());
    for repair in open_repairs {
        let fix = match (repair.fix_commit, repair.fix_change.as_deref()) {
            (Some(commit), _) => Some(commit),
            (None, Some(change)) => {
                velocity_rosen::build_repairs::change_landed_commit(server, change)
                    .await
                    .ok()
                    .flatten()
            }
            (None, None) => None,
        };
        let target = fix.unwrap_or_else(|| repair.culprit.clone());
        targets.push((repair.culprit, target));
    }
    targets
}

async fn observe(
    server: &ProjectServer,
    open_repairs: Vec<OpenRepair>,
) -> Result<(BuildRepairs, Option<u64>), String> {
    let failures = velocity_rosen::build_repairs::build_failures(server)
        .await
        .map_err(|error| error.to_string())?;
    let repair_targets = resolve_repair_targets(server, open_repairs).await;
    let mut commits: Vec<String> = failures
        .iter()
        .map(|failure| failure.commit.clone())
        .collect();
    commits.extend(repair_targets.iter().map(|(_, target)| target.clone()));
    commits.sort_unstable();
    commits.dedup();
    let covered = velocity_rosen::build_repairs::repair_coverage(server, &commits)
        .await
        .map_err(|error| error.to_string())?;
    let mut settled: Vec<String> = failures
        .iter()
        .filter(|failure| failure.reverted || covered.contains(&failure.commit))
        .map(|failure| failure.commit.clone())
        .collect();
    for (culprit, target) in &repair_targets {
        if covered.contains(target) {
            if !settled.contains(culprit) {
                settled.push(culprit.clone());
            }
        } else if target != culprit {
            // Keep an open repair whose fix landed until a green pass covers the fix commit.
            settled.retain(|commit| commit != culprit);
        }
    }
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
