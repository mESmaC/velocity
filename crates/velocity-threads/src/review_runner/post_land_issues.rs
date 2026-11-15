//! Deliver Rosen's durable, non-reverting test failures off the verification workers.

use std::path::Path;

use velocity_core::duty::{DutyEventKind, DutyFeedEvent};
use velocity_github::post_land_failures::{self, FailureReport, PendingReports};
use velocity_rosen::ProjectServer;
use velocity_rosen::post_land::{self, TestFailure};
use velocity_store::duty::PrDutyRoster;

use super::landing::LANDER;
use super::{PrDutyReviews, ReviewHost};

impl PrDutyReviews {
    pub(super) fn report_test_failures<Host: ReviewHost>(
        &self,
        host: &Host,
        roster: &PrDutyRoster,
    ) {
        let project = roster.project.id;
        if !self.lock().reporting_projects.insert(project) {
            return;
        }
        let (runner, host, roster) = (self.clone(), host.clone(), roster.clone());
        tokio::spawn(async move {
            let server = ProjectServer::new(&runner.app_data, project);
            let failures = post_land::test_failures(&server).await;
            let path = server.data_dir().join("test-failure-issues.json");
            let result = tokio::task::spawn_blocking(move || {
                report_pending(&path, &roster, failures.map_err(|error| error.to_string()))
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

    fn report_note<Host: ReviewHost>(&self, host: &Host, project: u32, message: Option<String>) {
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
        host.update(
            project,
            Box::new(move |_, timestamp| {
                vec![DutyFeedEvent::new(
                    timestamp,
                    LANDER,
                    DutyEventKind::Wait,
                    message,
                )]
            }),
        );
    }
}

fn report_pending(
    path: &Path,
    roster: &PrDutyRoster,
    failures: Result<Vec<TestFailure>, String>,
) -> Result<Option<u32>, String> {
    let mut pending = PendingReports::load(path).map_err(|error| error.to_string())?;
    let received = failures.and_then(|failures| enqueue_failures(&mut pending, roster, failures));
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
    roster: &PrDutyRoster,
    failures: Vec<TestFailure>,
) -> Result<(), String> {
    for failure in failures {
        if pending.contains(failure.session) {
            continue;
        }
        let repository = roster
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
        if let Some(thread) = roster
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
