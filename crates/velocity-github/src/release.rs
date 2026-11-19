//! Starting the release workflow for a landed commit (#431). Once the mirror is Collapse,
//! GitHub receives only the default branch its forwarder delivers, never tags, so the
//! workflow is dispatched with the tag and commit and creates the tag itself.

use serde::Deserialize;

use crate::{GitHubCommandError, IssueClient};

/// The workflow file `.github/workflows/release.yml` defines.
const RELEASE_WORKFLOW: &str = "release.yml";
/// The status Velocity publishes on a commit its integration build passed.
const INTEGRATION_CONTEXT: &str = "velocity/integration";

impl IssueClient {
    /// Runs the release workflow from the default branch for `tag` at `commit`. `green` is
    /// Rosen's last green commit covering `commit`, which the release gate checks.
    pub fn dispatch_release(
        &self,
        tag: &str,
        commit: &str,
        green: &str,
    ) -> Result<(), GitHubCommandError> {
        let repository = self.repository().to_owned();
        let tag_input = format!("tag={tag}");
        let commit_input = format!("commit={commit}");
        let green_input = format!("green={green}");
        self.transport.command.text(&[
            "workflow",
            "run",
            RELEASE_WORKFLOW,
            "--repo",
            &repository,
            "-f",
            &tag_input,
            "-f",
            &commit_input,
            "-f",
            &green_input,
        ])?;
        Ok(())
    }

    /// Whether `commit` carries the green `velocity/integration` status the release gate
    /// requires on Rosen's green commit. GitHub lists a context's newest status first.
    pub fn integration_passed(&self, commit: &str) -> Result<bool, GitHubCommandError> {
        #[derive(Deserialize)]
        struct Status {
            context: String,
            state: String,
        }
        let endpoint = format!(
            "repos/{}/commits/{commit}/statuses?per_page=100",
            self.repository()
        );
        let statuses: Vec<Status> = self.transport.command.read(&["api", &endpoint])?;
        Ok(statuses
            .iter()
            .find(|status| status.context == INTEGRATION_CONTEXT)
            .is_some_and(|status| status.state == "success"))
    }
}
