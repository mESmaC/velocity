//! Starting the release workflow for a landed commit (#431), comparing published releases
//! against the updater manifest, and inspecting refused release-gate runs (#521).
//!
//! Once the mirror is Collapse, GitHub receives only the default branch its forwarder
//! delivers, never tags, so the workflow is dispatched with the tag and commit and creates
//! the tag itself.

use serde::Deserialize;
use velocity_core::release::{
    ManifestLag, PublishedRelease, detect_manifest_lag, version_from_release_tag,
};

use crate::{GitHubCommandError, IssueClient};

/// The workflow file `.github/workflows/release.yml` defines.
const RELEASE_WORKFLOW: &str = "release.yml";
/// The rolling release holding `latest.json` for installed copies.
const MANIFEST_TAG: &str = "updater";
/// The manifest filename uploaded to `updater` and signed releases.
const MANIFEST_ASSET: &str = "latest.json";
/// The status Velocity publishes on a commit its integration build passed.
const INTEGRATION_CONTEXT: &str = "velocity/integration";
/// The job name in `release.yml` that runs `scripts/release-gate.ps1 -Stage Checks`.
const GATE_CHECKS_JOB: &str = "Release gate checks";

/// A recent `release.yml` run whose gate checks failed without an open release-blocker,
/// meaning the release commit lacked a green `velocity/integration` status (#521).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefusedReleaseGate {
    pub run_id: u64,
    pub tag: Option<String>,
    pub commit: String,
    pub url: String,
}

/// Checks `repository`'s `updater/latest.json` against its newest published `v*` release
/// via `gh`, without requiring a local checkout (#521).
pub fn check_repository_manifest_lag(
    repository: &str,
) -> Result<Option<ManifestLag>, GitHubCommandError> {
    let client = IssueClient::for_repository(&std::env::temp_dir(), repository)?;
    client.manifest_lag()
}

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

    /// The newest commit on GitHub's default branch carrying a green `velocity/integration`
    /// status, scanning up to `limit` recent commits (#521).
    pub fn newest_integration_green(
        &self,
        limit: usize,
    ) -> Result<Option<String>, GitHubCommandError> {
        #[derive(Deserialize)]
        struct CommitEntry {
            sha: String,
        }
        let per_page = limit.clamp(1, 50);
        let endpoint = format!("repos/{}/commits?per_page={per_page}", self.repository());
        let commits: Vec<CommitEntry> = self.transport.command.read(&["api", &endpoint])?;
        for entry in commits {
            if self.integration_passed(&entry.sha)? {
                return Ok(Some(entry.sha));
            }
        }
        Ok(None)
    }

    /// Compares the `updater` release's `latest.json` version against the newest non-draft
    /// published `v*` release. Returns `None` when this repository has no `updater`
    /// manifest release or the manifest is already up to date (#521).
    pub fn manifest_lag(&self) -> Result<Option<ManifestLag>, GitHubCommandError> {
        let releases = self.list_releases(30)?;
        let Some(asset_id) = manifest_asset_id(&releases) else {
            return Ok(None);
        };
        let manifest_version = self.read_manifest_version(asset_id)?;
        let published = to_published_releases(&releases);
        Ok(detect_manifest_lag(&manifest_version, &published))
    }

    /// Detects when the newest `release.yml` workflow run failed its `Release gate checks`
    /// job without an open `release-blocker` issue (and no newer release run is in flight),
    /// indicating the release commit lacked a green `velocity/integration` status (#521).
    pub fn latest_refused_release_gate(
        &self,
    ) -> Result<Option<RefusedReleaseGate>, GitHubCommandError> {
        let Some(runs) = self.recent_release_runs()? else {
            return Ok(None);
        };
        if runs.iter().any(|run| is_active_run(&run.status)) {
            return Ok(None);
        }
        let Some(newest) = runs.into_iter().next() else {
            return Ok(None);
        };
        if !matches!(newest.conclusion.as_deref(), Some("failure" | "cancelled")) {
            return Ok(None);
        }
        if !self.gate_checks_failed(newest.id)? || self.has_open_release_blocker()? {
            return Ok(None);
        }
        let tag = extract_run_tag(&newest)
            .or_else(|| self.tag_at_commit(&newest.head_sha).ok().flatten());
        Ok(Some(RefusedReleaseGate {
            run_id: newest.id,
            tag,
            commit: newest.head_sha,
            url: newest.html_url,
        }))
    }

    fn list_releases(&self, limit: usize) -> Result<Vec<ApiRelease>, GitHubCommandError> {
        let endpoint = format!("repos/{}/releases?per_page={limit}", self.repository());
        self.transport.command.read(&["api", &endpoint])
    }

    fn read_manifest_version(&self, asset_id: u64) -> Result<String, GitHubCommandError> {
        #[derive(Deserialize)]
        struct Manifest {
            version: String,
        }
        let endpoint = format!("repos/{}/releases/assets/{asset_id}", self.repository());
        let manifest: Manifest = self.transport.command.read(&[
            "api",
            "-H",
            "Accept: application/octet-stream",
            &endpoint,
        ])?;
        Ok(manifest.version)
    }

    fn recent_release_runs(&self) -> Result<Option<Vec<WorkflowRun>>, GitHubCommandError> {
        #[derive(Deserialize)]
        struct RunsResponse {
            #[serde(default)]
            workflow_runs: Vec<WorkflowRun>,
        }
        let endpoint = format!(
            "repos/{}/actions/workflows/{RELEASE_WORKFLOW}/runs?per_page=5",
            self.repository()
        );
        match self
            .transport
            .command
            .read::<RunsResponse>(&["api", &endpoint])
        {
            Ok(response) => Ok(Some(response.workflow_runs)),
            Err(GitHubCommandError::Command { message, .. })
                if message.contains("HTTP 404") || message.contains("Not Found") =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn gate_checks_failed(&self, run_id: u64) -> Result<bool, GitHubCommandError> {
        #[derive(Deserialize)]
        struct Job {
            name: String,
            #[serde(default)]
            conclusion: Option<String>,
        }
        #[derive(Deserialize)]
        struct JobsResponse {
            #[serde(default)]
            jobs: Vec<Job>,
        }
        let endpoint = format!(
            "repos/{}/actions/runs/{run_id}/jobs?per_page=20",
            self.repository()
        );
        let response: JobsResponse = self.transport.command.read(&["api", &endpoint])?;
        Ok(response
            .jobs
            .iter()
            .any(|job| job.name == GATE_CHECKS_JOB && job.conclusion.as_deref() == Some("failure")))
    }

    fn has_open_release_blocker(&self) -> Result<bool, GitHubCommandError> {
        #[derive(Deserialize)]
        struct IssueStub {
            #[serde(default)]
            pull_request: Option<serde_json::Value>,
        }
        let endpoint = format!(
            "repos/{}/issues?state=open&labels=release-blocker&per_page=5",
            self.repository()
        );
        let issues: Vec<IssueStub> = self.transport.command.read(&["api", &endpoint])?;
        Ok(issues.iter().any(|issue| issue.pull_request.is_none()))
    }

    fn tag_at_commit(&self, commit: &str) -> Result<Option<String>, GitHubCommandError> {
        #[derive(Deserialize)]
        struct TagCommit {
            sha: String,
        }
        #[derive(Deserialize)]
        struct TagEntry {
            name: String,
            commit: TagCommit,
        }
        let endpoint = format!("repos/{}/tags?per_page=20", self.repository());
        let tags: Vec<TagEntry> = self.transport.command.read(&["api", &endpoint])?;
        Ok(tags.into_iter().find_map(|entry| {
            (entry.commit.sha == commit && version_from_release_tag(&entry.name).is_some())
                .then_some(entry.name)
        }))
    }
}

#[derive(Deserialize)]
struct ApiAsset {
    id: u64,
    name: String,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct WorkflowRun {
    id: u64,
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
    head_sha: String,
    #[serde(default)]
    head_branch: Option<String>,
    #[serde(default)]
    display_title: String,
    #[serde(default)]
    html_url: String,
}

fn manifest_asset_id(releases: &[ApiRelease]) -> Option<u64> {
    releases
        .iter()
        .find(|release| !release.draft && release.tag_name == MANIFEST_TAG)
        .and_then(|release| {
            release
                .assets
                .iter()
                .find(|asset| asset.name == MANIFEST_ASSET)
                .map(|asset| asset.id)
        })
}

fn to_published_releases(releases: &[ApiRelease]) -> Vec<PublishedRelease> {
    releases
        .iter()
        .filter_map(|release| {
            let version = version_from_release_tag(&release.tag_name)?;
            let has_manifest = release
                .assets
                .iter()
                .any(|asset| asset.name == MANIFEST_ASSET);
            let has_signature = release
                .assets
                .iter()
                .any(|asset| asset.name.ends_with(".sig"));
            Some(PublishedRelease {
                tag: release.tag_name.clone(),
                version,
                draft: release.draft,
                signed: has_manifest && has_signature,
            })
        })
        .collect()
}

fn is_active_run(status: &str) -> bool {
    matches!(
        status,
        "queued" | "in_progress" | "waiting" | "requested" | "pending"
    )
}

fn extract_run_tag(run: &WorkflowRun) -> Option<String> {
    if let Some(branch) = run.head_branch.as_deref()
        && version_from_release_tag(branch).is_some()
    {
        return Some(branch.to_owned());
    }
    run.display_title.split_whitespace().find_map(|token| {
        let clean =
            token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '.' && ch != '-');
        version_from_release_tag(clean).map(|_| clean.to_owned())
    })
}
