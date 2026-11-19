//! Deliver Rosen's durable, non-reverting test failures off the verification workers.

use std::path::Path;

use velocity_core::duty::DutyEventKind;
use velocity_github::post_land_failures::{self, FailureReport, PendingReports};
use velocity_rosen::post_land::{self, TestFailure};
use velocity_rosen::{ProjectServer, integration};
use velocity_store::duty::LandingProject;

use super::LANDER;
use super::{LandingHost, RosenLanding};

impl RosenLanding {
    pub(super) fn report_test_failures<Host: LandingHost>(
        &self,
        host: &Host,
        project_state: &LandingProject,
    ) {
        let project = project_state.project.id;
        if !self.lock().reporting_projects.insert(project) {
            return;
        }
        let (runner, host, project_state) = (self.clone(), host.clone(), project_state.clone());
        tokio::spawn(async move {
            let server = ProjectServer::new(&runner.app_data, project);
            let failures = post_land::test_failures(&server).await;
            let path = server.data_dir().join("test-failure-issues.json");
            let result = tokio::task::spawn_blocking(move || {
                if let Err(error) = publish_integrations(&server, &project_state) {
                    eprintln!("Integration status for project {project}: {error}");
                }
                report_pending(
                    &path,
                    &project_state,
                    failures.map_err(|error| error.to_string()),
                )
            })
            .await;
            let message = match result {
                Ok(Ok(Some(number))) => Some(format!(
                    "Post-land test failure reported on issue #{number} · landing continues"
                )),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => Some(format!("Post-land issue reporting waits · {error}")),
                Err(error) => Some(format!("Post-land issue reporting waits · {error}")),
            };
            runner.report_note(&host, project, message);
            runner.lock().reporting_projects.remove(&project);
        });
    }

    fn report_note<Host: LandingHost>(&self, host: &Host, project: u32, message: Option<String>) {
        let Some(message) = message else {
            self.lock().reporting_notes.remove(&project);
            return;
        };
        if self
            .lock()
            .reporting_notes
            .insert(project, message.clone())
            .as_ref()
            == Some(&message)
        {
            return;
        }
        host.activity(project, LANDER, DutyEventKind::Wait, message);
    }
}

fn report_pending(
    path: &Path,
    project_state: &LandingProject,
    failures: Result<Vec<TestFailure>, String>,
) -> Result<Option<u32>, String> {
    let mut pending = PendingReports::load(path).map_err(|error| error.to_string())?;
    let received =
        failures.and_then(|failures| enqueue_failures(&mut pending, project_state, failures));
    // Save before any request: a failed or interrupted GitHub call must be retried.
    if pending.first().is_some() {
        pending.save(path).map_err(|error| error.to_string())?;
    }
    let mut reported = None;
    while let Some(report) = pending.first() {
        reported = Some(post_land_failures::publish(report).map_err(|error| error.to_string())?);
        pending.acknowledge();
        pending.save(path).map_err(|error| error.to_string())?;
    }
    received?;
    Ok(reported)
}

fn enqueue_failures(
    pending: &mut PendingReports,
    project_state: &LandingProject,
    failures: Vec<TestFailure>,
) -> Result<(), String> {
    for failure in failures {
        if pending.contains(failure.session) {
            continue;
        }
        let repository = project_state
            .project
            .github_remote
            .clone()
            .ok_or("Project has no GitHub remote for post-land failure issues")?;
        let mut report = FailureReport::new(
            failure.session,
            repository,
            failure.change.clone(),
            failure.commit.clone(),
            failure.output.clone(),
        );
        if let Some(thread) = project_state
            .project
            .threads
            .iter()
            .find(|thread| thread.rosen_change.as_deref() == Some(&failure.change))
        {
            report = report.with_thread(thread.id, thread.title.clone());
        }
        pending.enqueue(report);
    }
    Ok(())
}

fn publish_integrations(
    server: &ProjectServer,
    project_state: &LandingProject,
) -> Result<(), String> {
    let Some(repository) = project_state.project.github_remote.as_deref() else {
        return Ok(());
    };
    let pending = integration::unpublished(server).map_err(|error| error.to_string())?;
    if pending.is_empty() {
        return Ok(());
    }
    let client = velocity_github::IssueClient::for_repository(
        Path::new(&project_state.project.repository_path),
        repository,
    )
    .map_err(|error| error.to_string())?;
    for commit in &pending {
        if client
            .publish_integration_success(commit)
            .map_err(|error| error.to_string())?
        {
            integration::published(server, commit).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}
