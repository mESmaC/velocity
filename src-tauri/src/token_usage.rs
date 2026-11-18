use tauri::{AppHandle, Manager};
use velocity_graph::token_usage::{self, Period, Project, Report};

use crate::state::ManagedStateStore;

#[tauri::command]
#[specta::specta]
pub async fn get_token_usage(
    application: AppHandle,
    period: Period,
    project_identifier: Option<u32>,
    thread_identifier: Option<u32>,
) -> Result<Report, String> {
    let thread = project_identifier.zip(thread_identifier);
    let store = application.state::<ManagedStateStore>();
    let (mut projects, samples, trial_enabled) = {
        let store = store.lock().map_err(|error| error.to_string())?;
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
            .collect::<std::collections::BTreeMap<_, _>>();
        (
            projects,
            samples,
            store.state().settings.agents.context_handoff,
        )
    };
    if thread.is_none() {
        projects.entry(0).or_insert_with(|| Project {
            id: 0,
            name: "Machine duty".to_owned(),
            threads: Default::default(),
        });
    }
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let show_trial = thread.is_none() && (trial_enabled || !samples.is_empty());
    let items = trial_items(&data, samples).await;
    crate::layout::off_ui_thread(move || {
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
