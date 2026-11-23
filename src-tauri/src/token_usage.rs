use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use velocity_graph::token_usage::{self, Period, Project, Report};

const TOKEN_USAGE_COMMAND: &str = "get_token_usage";

type HandoffSamples = Vec<(
    u32,
    std::collections::BTreeMap<String, velocity_store::handoff::Item>,
)>;

struct StoreSnapshot {
    projects: std::collections::BTreeMap<u32, Project>,
    samples: HandoffSamples,
    trial_enabled: bool,
}

#[tauri::command]
#[specta::specta]
pub async fn get_token_usage(
    application: AppHandle,
    period: Period,
    project_identifier: Option<u32>,
    thread_identifier: Option<u32>,
) -> Result<Report, String> {
    measure_command_with(
        load_token_usage(application, period, project_identifier, thread_identifier),
        velocity_core::performance::record_ui_command,
    )
    .await
}

/// Times the full `get_token_usage` invocation from entry to result — including store lock
/// wait, Rosen build-coverage queries and report generation — and emits one latency sample
/// whether the command succeeds or exits early with an error (#693).
async fn measure_command_with<Value>(
    work: impl Future<Output = Value>,
    record: impl FnOnce(&'static str, Duration),
) -> Value {
    let started = Instant::now();
    let outcome = work.await;
    record(TOKEN_USAGE_COMMAND, started.elapsed());
    outcome
}

async fn load_token_usage(
    application: AppHandle,
    period: Period,
    project_identifier: Option<u32>,
    thread_identifier: Option<u32>,
) -> Result<Report, String> {
    let thread = project_identifier.zip(thread_identifier);
    let mut snapshot = read_store_snapshot(&application, project_identifier, thread).await?;
    if thread.is_none() {
        snapshot.projects.entry(0).or_insert_with(|| Project {
            id: 0,
            name: "Machine duty".to_owned(),
            threads: Default::default(),
        });
    }
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let show_trial = thread.is_none() && (snapshot.trial_enabled || !snapshot.samples.is_empty());
    let items = trial_items(&data, snapshot.samples).await;
    let projects = snapshot.projects;
    // The outer `measure_command_with` boundary already times the full command, so run the
    // blocking report directly on Tokio's pool instead of `layout::run_concurrently` (#693).
    tauri::async_runtime::spawn_blocking(move || {
        let mut report = token_usage::report(&data, projects, period, thread)
            .map_err(|error| error.to_string())?;
        if show_trial {
            report.handoff = Some(
                token_usage::handoff::compare(&data, items).map_err(|error| error.to_string())?,
            );
        }
        Ok(report)
    })
    .await
    .map_err(|error| format!("Token usage report task failed: {error}"))?
}

async fn read_store_snapshot(
    application: &AppHandle,
    project_identifier: Option<u32>,
    thread: Option<(u32, u32)>,
) -> Result<StoreSnapshot, String> {
    crate::state::with_store(application, move |store| {
        let samples = if thread.is_none() {
            store
                .state()
                .projects
                .iter()
                .filter(|project| project_identifier.is_none_or(|id| id == project.id))
                .map(|project| (project.id, project.handoff_items.clone()))
                .filter(|(_, items)| items.values().any(|item| item.crossed_at.is_some()))
                .collect()
        } else {
            Vec::new()
        };
        let projects = store
            .state()
            .projects
            .iter()
            .filter(|project| thread.is_none_or(|(id, _)| id == project.id))
            .map(|project| {
                (
                    project.id,
                    Project {
                        id: project.id,
                        name: project.name.clone(),
                        threads: project
                            .threads
                            .iter()
                            .map(|thread| (thread.id, thread.title.clone()))
                            .collect(),
                    },
                )
            })
            .collect();
        StoreSnapshot {
            projects,
            samples,
            trial_enabled: store.state().settings.agents.context_handoff,
        }
    })
    .await
}

async fn trial_items(
    data: &std::path::Path,
    samples: Vec<(
        u32,
        std::collections::BTreeMap<String, velocity_store::handoff::Item>,
    )>,
) -> Vec<token_usage::handoff::Item> {
    let mut result = Vec::new();
    for (project, items) in samples {
        let candidates: Vec<_> = items
            .into_iter()
            .filter_map(|(change, item)| {
                let crossed = item.crossed_at?;
                let commit = item.landed.clone()?;
                let reviewed = item.verdicts.contains_key(&format!("land:{commit}"))
                    || item.verdicts.values().any(|state| state == "approved");
                reviewed.then_some((change, item, crossed, commit))
            })
            .collect();
        if candidates.is_empty() {
            continue;
        }
        let commits: Vec<_> = candidates
            .iter()
            .map(|(_, _, _, commit)| commit.clone())
            .collect();
        let server = velocity_rosen::ProjectServer::new(data, project);
        let covered = match velocity_rosen::issue_build_coverage(&server, &commits).await {
            Ok(covered) => covered,
            Err(error) => {
                eprintln!("Could not read hand-off build evidence for project {project}: {error}");
                continue;
            }
        };
        result.extend(
            candidates
                .into_iter()
                .filter_map(|(change, item, crossed_at, commit)| {
                    if !covered.contains(&commit) {
                        return None;
                    }
                    let review_rounds = item.review_rounds();
                    Some(token_usage::handoff::Item {
                        project,
                        change,
                        configuration: item.configuration,
                        source: item.source,
                        crossed_at,
                        enabled: item.enabled,
                        handed_off: item.handed_off,
                        review_rounds,
                        reverts: item.reverts,
                    })
                }),
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use velocity_core::performance::PerformanceTracker;

    // Risk (#693, SPEC.md §3.1/F15): `get_token_usage` must measure from command entry to
    // result so pre-report store waits and Rosen build-coverage awaits are included in its
    // latency row, emitting one sample per invocation on both success and early error.
    #[test]
    fn measures_awaited_work_once_on_success_and_early_error() -> Result<(), String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|error| error.to_string())?;
        let mut tracker = PerformanceTracker::new(Instant::now());
        let mut samples = Vec::new();

        let success = runtime.block_on(measure_command_with(
            async {
                tokio::time::sleep(Duration::from_millis(40)).await;
                tokio::task::spawn_blocking(|| Ok::<&str, String>("report"))
                    .await
                    .map_err(|error| error.to_string())?
            },
            |command, duration| {
                samples.push((command, duration));
                tracker.record_ui_command(command, duration, Instant::now(), 1);
            },
        ));
        assert_eq!(success, Ok("report"));

        let failure = runtime.block_on(measure_command_with(
            async {
                tokio::time::sleep(Duration::from_millis(40)).await;
                Err::<&str, String>(String::from("store unavailable"))
            },
            |command, duration| {
                samples.push((command, duration));
                tracker.record_ui_command(command, duration, Instant::now(), 2);
            },
        ));
        assert_eq!(failure, Err(String::from("store unavailable")));

        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].0, TOKEN_USAGE_COMMAND);
        assert_eq!(samples[1].0, TOKEN_USAGE_COMMAND);
        assert!(samples[0].1 >= Duration::from_millis(20));
        assert!(samples[1].1 >= Duration::from_millis(20));

        let diagnostics = tracker.snapshot(Instant::now());
        assert_eq!(diagnostics.ui_commands.len(), 1);
        assert_eq!(diagnostics.ui_commands[0].command, TOKEN_USAGE_COMMAND);
        assert_eq!(diagnostics.ui_commands[0].calls, 2);
        assert!(diagnostics.ui_commands[0].p95_ms >= 20);
        Ok(())
    }

    // Risk (#693): `get_token_usage` must record into the shared performance collector once
    // per invocation for both success and early error paths.
    #[test]
    fn records_into_shared_collector_on_success_and_early_error() -> Result<(), String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|error| error.to_string())?;
        let calls_for_command = || {
            velocity_core::performance::snapshot()
                .ui_commands
                .into_iter()
                .find(|row| row.command == TOKEN_USAGE_COMMAND)
                .map_or(0, |row| row.calls)
        };
        let before = calls_for_command();

        let ok = runtime.block_on(measure_command_with(
            async { Ok::<(), String>(()) },
            velocity_core::performance::record_ui_command,
        ));
        assert_eq!(ok, Ok(()));
        assert_eq!(calls_for_command(), before + 1);

        let err = runtime.block_on(measure_command_with(
            async { Err::<(), String>(String::from("early failure")) },
            velocity_core::performance::record_ui_command,
        ));
        assert_eq!(err, Err(String::from("early failure")));
        assert_eq!(calls_for_command(), before + 2);
        Ok(())
    }
}
