//! Keeping Velocity's Rosen pin current (SPEC.md F5 "Rosen · Pin bumps", #513): the GitHub
//! reads and writes behind `velocity_core::rosen_pin`. One look reads the pin from the
//! default branch's `Cargo.toml`, finds the newest green commit on Rosen's default branch
//! ahead of it, and leaves exactly one open, current pin-bump issue. It also publishes the
//! Rosen release a landed bump names, which starts Rosen's workflow that attaches the
//! prebuilt binaries. Everything is REST, which keeps working when a swarm has spent the
//! GraphQL budget.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};
use velocity_core::rosen_pin::{
    self, CheckRun, CiState, GreenSource, LandedIssue, OpenIssue, PinBump, PinBumpStep, PinVerdict,
    PostLandState, RosenCommit, RosenPin,
};

use crate::gh_command::{GitHubCommand, GitHubCommandError};

/// Where Collapse's issues live. Its open issues are read with Velocity's for the ones
/// that name a landed Rosen issue.
pub const COLLAPSE_REPOSITORY: &str = "mESmaC/collapse";

/// Asks GitHub for a file's text instead of its base64 form.
const RAW_CONTENT: &str = "Accept: application/vnd.github.raw+json";
/// How many of the newest commits ahead of the pin are asked for their CI. A push runs CI
/// on its tip only, so the newest green commit is usually the first or second asked.
const GREEN_SCAN: usize = 8;

/// What a look found the pin-bump issue to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinIssue {
    /// Rosen's default branch has nothing the pin lacks.
    Current,
    /// Commits are ahead of the pin, and none of the newest has green CI yet.
    NoGreenCommit,
    Opened(u32),
    Rewritten(u32),
    /// The open issue already says what is due.
    Kept(u32),
}

/// What one look did. The issue and the release fail separately, so an account that may
/// not publish on Rosen still keeps the issue current.
#[derive(Debug)]
pub struct PinLook {
    pub issue: Result<IssueLook, GitHubCommandError>,
    pub release: Result<ReleaseLook, GitHubCommandError>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueLook {
    pub issue: PinIssue,
    /// Pin-bump issues closed because their bump landed or another issue is kept.
    pub closed: Vec<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReleaseLook {
    /// The pinned Rosen release known to exist, which the next look need not ask about.
    pub release: Option<String>,
    /// Whether this look published that release.
    pub published: bool,
}

/// What Rosen's default branch has beyond the pin.
enum Ahead {
    Nothing,
    NoGreenCommit,
    /// The commits from the newest green one down to the pin, newest first, whether more lie
    /// between them and the pin than GitHub's first page listed, and which source certified it.
    Green {
        commits: Vec<RosenCommit>,
        more: bool,
        green_source: GreenSource,
    },
}

/// The `gh` transport for the repository that holds the pin and the pin-bump issue.
pub struct RosenPinWatch {
    command: GitHubCommand,
    velocity: String,
}

impl RosenPinWatch {
    /// `velocity` is the `owner/name` of the repository whose `Cargo.toml` pins Rosen.
    #[track_caller]
    pub fn new(velocity: &str) -> Result<Self, GitHubCommandError> {
        let name = crate::RepositoryName::parse(velocity)
            .map_err(|error| GitHubCommandError::Invalid(error.to_string()))?;
        Ok(Self {
            command: GitHubCommand::for_repository(&name),
            velocity: name.full_name(),
        })
    }

    /// Whether the signed-in account may push to the repository. Only its maintainers keep
    /// the issue, so a copy of Velocity that merely cloned the repository files nothing.
    pub fn can_maintain(&self) -> Result<bool, GitHubCommandError> {
        #[derive(Deserialize)]
        struct Permissions {
            #[serde(default)]
            push: bool,
        }
        #[derive(Deserialize)]
        struct RepoPermissions {
            #[serde(default)]
            permissions: Option<Permissions>,
        }
        let endpoint = format!("repos/{}", self.velocity);
        let repo: RepoPermissions = self.command.read(&["api", &endpoint])?;
        Ok(repo.permissions.is_some_and(|perm| perm.push))
    }

    /// Brings the pin-bump issue and the pinned Rosen release up to date. `known_release`
    /// is the release an earlier look found on Rosen. Fails only when the pin can't be read.
    pub fn look(&self, known_release: Option<&str>) -> Result<PinLook, GitHubCommandError> {
        self.look_with_post_land(known_release, |_, _| BTreeMap::new())
    }

    /// Brings the pin-bump issue up to date and returns the parsed [`RosenPin`] so the caller
    /// can build and publish the pinned Rosen release to Collapse directly without touching
    /// GitHub's release workflow (#593).
    pub fn look_issue_with_post_land(
        &self,
        read_post_land: impl FnOnce(&RosenPin, &[RosenCommit]) -> BTreeMap<String, PostLandState>,
    ) -> Result<(RosenPin, Result<IssueLook, GitHubCommandError>), GitHubCommandError> {
        let pin = self.pin()?;
        let issue = self.keep_issue_current(&pin, read_post_land);
        Ok((pin, issue))
    }

    /// Brings the pin-bump issue and the pinned Rosen release up to date, combining GitHub CI
    /// with Rosen's own post-land verdicts supplied by `read_post_land` (#580).
    pub fn look_with_post_land(
        &self,
        known_release: Option<&str>,
        read_post_land: impl FnOnce(&RosenPin, &[RosenCommit]) -> BTreeMap<String, PostLandState>,
    ) -> Result<PinLook, GitHubCommandError> {
        let pin = self.pin()?;
        Ok(PinLook {
            issue: self.keep_issue_current(&pin, read_post_land),
            release: self.publish_release(&pin, known_release),
        })
    }

    fn keep_issue_current(
        &self,
        pin: &RosenPin,
        read_post_land: impl FnOnce(&RosenPin, &[RosenCommit]) -> BTreeMap<String, PostLandState>,
    ) -> Result<IssueLook, GitHubCommandError> {
        let open = self
            .open_issues(&self.velocity)?
            .ok_or_else(|| GitHubCommandError::Missing(format!("{}'s issues", self.velocity)))?;
        let (issue, closed) = match self.ahead(pin, read_post_land)? {
            Ahead::Green {
                commits,
                more,
                green_source,
            } => {
                let bump = self.bump(pin, &commits, more, green_source, &open)?;
                self.keep_one(&open, &bump)?
            }
            Ahead::Nothing => (PinIssue::Current, self.close_surplus(&open)?),
            Ahead::NoGreenCommit => (PinIssue::NoGreenCommit, self.close_surplus(&open)?),
        };
        Ok(IssueLook { issue, closed })
    }

    /// The pin on the default branch, which is what the next release builds.
    fn pin(&self) -> Result<RosenPin, GitHubCommandError> {
        let manifest = self.file(&self.velocity, "Cargo.toml", None)?;
        RosenPin::parse(&manifest).ok_or_else(|| {
            GitHubCommandError::Missing(format!("a Rosen pin in {}'s Cargo.toml", self.velocity))
        })
    }

    fn file(
        &self,
        repository: &str,
        path: &str,
        commit: Option<&str>,
    ) -> Result<String, GitHubCommandError> {
        let reference = commit.map_or_else(String::new, |commit| format!("?ref={commit}"));
        let endpoint = format!("repos/{repository}/contents/{path}{reference}");
        self.command.text(&["api", "-H", RAW_CONTENT, &endpoint])
    }

    fn ahead(
        &self,
        pin: &RosenPin,
        read_post_land: impl FnOnce(&RosenPin, &[RosenCommit]) -> BTreeMap<String, PostLandState>,
    ) -> Result<Ahead, GitHubCommandError> {
        let rosen = &pin.repository;
        let endpoint = format!("repos/{rosen}/commits?per_page=100");
        let commits: Vec<ApiCommit> = self.command.read(&["api", &endpoint])?;
        let commits: Vec<RosenCommit> = commits.into_iter().map(ApiCommit::into_commit).collect();
        let (ahead, found) = rosen_pin::commits_ahead(&commits, &pin.revision);
        let Some(newest) = ahead.first() else {
            return Ok(Ahead::Nothing);
        };
        // A pin missing from the newest page is either far behind or not on the branch at
        // all, such as a pin on a commit that was never merged. Only the first is a bump.
        if !found && !self.contains(rosen, &newest.commit, &pin.revision)? {
            return Ok(Ahead::Nothing);
        }
        let post_land = read_post_land(pin, ahead);
        for (index, commit) in ahead.iter().enumerate().take(GREEN_SCAN) {
            let rosen_state = post_land
                .get(&commit.commit.to_ascii_lowercase())
                .unwrap_or(&PostLandState::Unknown);
            // A red post-land result blocks regardless of GitHub CI, so skip its CI reads.
            if matches!(rosen_state, PostLandState::Failed) {
                continue;
            }
            let github_state = self.ci_state(rosen, &commit.commit)?;
            if let PinVerdict::Green(green_source) =
                rosen_pin::pin_verdict(rosen_state, github_state)
            {
                return Ok(Ahead::Green {
                    commits: ahead[index..].to_vec(),
                    more: !found,
                    green_source,
                });
            }
        }
        Ok(Ahead::NoGreenCommit)
    }

    /// Whether `descendant` has `ancestor` in its history.
    fn contains(
        &self,
        repository: &str,
        descendant: &str,
        ancestor: &str,
    ) -> Result<bool, GitHubCommandError> {
        #[derive(Deserialize)]
        struct Comparison {
            status: String,
        }
        let endpoint = format!("repos/{repository}/compare/{ancestor}...{descendant}?per_page=1");
        let comparison: Comparison = self.command.read(&["api", &endpoint])?;
        Ok(comparison.status == "ahead")
    }

    fn ci_state(&self, repository: &str, commit: &str) -> Result<CiState, GitHubCommandError> {
        let commit_endpoint = format!("repos/{repository}/commits/{commit}");
        let runs_endpoint = format!("{commit_endpoint}/check-runs?per_page=100");
        let runs: CheckRunsResponse = self.command.read(&["api", &runs_endpoint])?;
        let workflow_runs_endpoint =
            format!("repos/{repository}/actions/runs?head_sha={commit}&per_page=100");
        let workflow_runs: WorkflowRunsResponse =
            self.command.read(&["api", &workflow_runs_endpoint])?;
        let status_endpoint = format!("{commit_endpoint}/status?per_page=100");
        let combined: CombinedStatus = self.command.read(&["api", &status_endpoint])?;
        let runs: Vec<CheckRun<'_>> = runs
            .check_runs
            .iter()
            .map(|run| CheckRun {
                completed: run.status == "completed",
                conclusion: run.conclusion.as_deref(),
            })
            .collect();
        let workflow_runs: Vec<rosen_pin::WorkflowRun<'_>> = workflow_runs
            .workflow_runs
            .iter()
            .map(|run| rosen_pin::WorkflowRun {
                completed: run.completed(),
                conclusion: run.conclusion.as_deref(),
            })
            .collect();
        let statuses: Vec<&str> = combined
            .statuses
            .iter()
            .map(|status| status.state.as_str())
            .collect();
        Ok(rosen_pin::ci_state(&runs, &workflow_runs, &statuses))
    }

    /// What the issue says for the green commit at the head of `commits`.
    fn bump(
        &self,
        pin: &RosenPin,
        commits: &[RosenCommit],
        more: bool,
        green_source: GreenSource,
        velocity_open: &[OpenIssue],
    ) -> Result<PinBump, GitHubCommandError> {
        let rosen = pin.repository.clone();
        let target = commits
            .first()
            .map(|commit| commit.commit.clone())
            .ok_or_else(|| GitHubCommandError::Missing("a green Rosen commit".into()))?;
        let manifest = self.file(&rosen, "Cargo.toml", Some(&target))?;
        let release = rosen_pin::package_version(&manifest)
            .and_then(|version| rosen_pin::release_tag(version, &target))
            .ok_or_else(|| {
                GitHubCommandError::Missing(format!("a release version in {rosen}'s Cargo.toml"))
            })?;
        let numbers = rosen_pin::landed_issues(commits);
        let mut issues = velocity_open.to_vec();
        let mut unread = Vec::new();
        // Without a landed Rosen issue no open issue can name one, so nothing more is read.
        if !numbers.is_empty() {
            match self.open_issues(COLLAPSE_REPOSITORY)? {
                Some(collapse) => issues.extend(collapse),
                None => unread.push(COLLAPSE_REPOSITORY.to_owned()),
            }
        }
        let titles = self.issue_titles(&rosen, numbers.iter().copied())?;
        Ok(PinBump {
            dependents: rosen_pin::dependents(&issues, &rosen, &numbers),
            landed: titles
                .into_iter()
                .map(|(number, title)| LandedIssue { number, title })
                .collect(),
            commits: commits.len(),
            more_commits: more,
            pin: pin.clone(),
            target,
            green_source,
            release,
            unread,
        })
    }

    /// Each numbered issue's title, empty for a number that is no issue of `repository`.
    /// The recently updated issues come in one request, which usually has every issue a
    /// recent commit names; the rest are read one by one.
    fn issue_titles(
        &self,
        repository: &str,
        numbers: impl Iterator<Item = u32>,
    ) -> Result<BTreeMap<u32, String>, GitHubCommandError> {
        let mut numbers = numbers.peekable();
        if numbers.peek().is_none() {
            return Ok(BTreeMap::new());
        }
        let issues = format!("repos/{repository}/issues");
        let recent_endpoint =
            format!("{issues}?state=all&sort=updated&direction=desc&per_page=100");
        let recent: Vec<Titled> = self.command.read(&["api", &recent_endpoint])?;
        let recent: BTreeMap<u32, String> = recent
            .into_iter()
            .map(|issue| (issue.number, issue.title))
            .collect();
        numbers
            .map(|number| {
                if let Some(title) = recent.get(&number) {
                    return Ok((number, title.clone()));
                }
                let endpoint = format!("{issues}/{number}");
                match self.command.read::<Titled>(&["api", &endpoint]) {
                    Ok(issue) => Ok((number, issue.title)),
                    Err(error) if not_found(&error) => Ok((number, String::new())),
                    Err(error) => Err(error),
                }
            })
            .collect()
    }

    /// The repository's open issues without its pull requests, or `None` when the signed-in
    /// account cannot read the repository. Reuses the shared open-issues cache when fresh (#556).
    fn open_issues(&self, repository: &str) -> Result<Option<Vec<OpenIssue>>, GitHubCommandError> {
        if let Some(cached) = crate::issue_cache::get_fresh(repository, crate::OPEN_ISSUES_REFRESH)
        {
            return Ok(Some(
                cached
                    .into_iter()
                    .map(|issue| OpenIssue {
                        repository: repository.to_owned(),
                        number: issue.number,
                        title: issue.title,
                        body: issue.body,
                    })
                    .collect(),
            ));
        }
        let endpoint = format!("repos/{repository}/issues?state=open&per_page=100");
        let listed = match self.command.get_paginated::<Listed>(&endpoint) {
            Ok(listed) => listed,
            Err(error) if not_found(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(Some(
            listed
                .into_iter()
                .filter(|issue| issue.pull_request.is_none())
                .map(|issue| OpenIssue {
                    repository: repository.to_owned(),
                    number: issue.number,
                    title: issue.title,
                    body: issue.body.unwrap_or_default(),
                })
                .collect(),
        ))
    }

    /// Opens or rewrites the one pin-bump issue and closes the others.
    fn keep_one(
        &self,
        open: &[OpenIssue],
        bump: &PinBump,
    ) -> Result<(PinIssue, Vec<u32>), GitHubCommandError> {
        let plan = rosen_pin::plan(open, bump);
        let text = json!({"title": bump.title(), "body": bump.body()});
        let issues = format!("repos/{}/issues", self.velocity);
        let (kept, issue) = match plan.step {
            PinBumpStep::Open => {
                let opened: Numbered = self.command.api("POST", &issues, &text)?;
                crate::issue_cache::invalidate(&self.velocity);
                (opened.number, PinIssue::Opened(opened.number))
            }
            PinBumpStep::Rewrite(number) => {
                let _: Value = self
                    .command
                    .api("PATCH", &format!("{issues}/{number}"), &text)?;
                crate::issue_cache::invalidate(&self.velocity);
                (number, PinIssue::Rewritten(number))
            }
            PinBumpStep::Keep(number) => (number, PinIssue::Kept(number)),
        };
        let landed = format!(
            "The default branch pins `{}`, the commit this issue asked for. Rosen has a newer \
             green commit, which #{kept} asks for.",
            bump.pin.revision
        );
        for number in &plan.finished {
            self.close(*number, "completed", &landed)?;
        }
        for number in &plan.duplicates {
            self.close(*number, "not_planned", &one_issue_notice(kept))?;
        }
        Ok((issue, [plan.finished, plan.duplicates].concat()))
    }

    /// While no bump is due, only the oldest open pin-bump issue stays for its follow-through.
    fn close_surplus(&self, open: &[OpenIssue]) -> Result<Vec<u32>, GitHubCommandError> {
        let Some((kept, surplus)) = rosen_pin::oldest_and_surplus(open) else {
            return Ok(Vec::new());
        };
        for number in &surplus {
            self.close(*number, "not_planned", &one_issue_notice(kept))?;
        }
        Ok(surplus)
    }

    /// Comment first, state second: a failure never leaves a closed issue without its reason.
    fn close(&self, number: u32, reason: &str, comment: &str) -> Result<(), GitHubCommandError> {
        let issue = format!("repos/{}/issues/{number}", self.velocity);
        let _: Value = self.command.api(
            "POST",
            &format!("{issue}/comments"),
            &json!({"body": comment}),
        )?;
        let _: Value = self.command.api(
            "PATCH",
            &issue,
            &json!({"state": "closed", "state_reason": reason}),
        )?;
        crate::issue_cache::invalidate(&self.velocity);
        Ok(())
    }

    /// Publishes the Rosen release the pin names when Rosen has none by that tag. The
    /// release is created on the pinned commit, and Rosen's release workflow attaches the
    /// binaries.
    fn publish_release(
        &self,
        pin: &RosenPin,
        known: Option<&str>,
    ) -> Result<ReleaseLook, GitHubCommandError> {
        let Some(tag) = pin.release_to_publish() else {
            return Ok(ReleaseLook::default());
        };
        let found = |published| ReleaseLook {
            release: Some(tag.to_owned()),
            published,
        };
        if known == Some(tag) {
            return Ok(found(false));
        }
        let releases = format!("repos/{}/releases", pin.repository);
        match self
            .command
            .read::<Value>(&["api", &format!("{releases}/tags/{tag}")])
        {
            Ok(_) => return Ok(found(false)),
            Err(error) if !not_found(&error) => return Err(error),
            Err(_) => {}
        }
        let short = rosen_pin::short_commit(&pin.revision);
        let notes = format!(
            "The Rosen build {} pins at `{}`. This release's workflow attaches the prebuilt \
             binaries that Velocity's release workflow downloads for this commit.",
            self.velocity, pin.revision
        );
        let _: Value = self.command.api(
            "POST",
            &releases,
            &json!({
                "tag_name": tag,
                "target_commitish": pin.revision,
                "name": format!("Rosen at {short}, pinned by Velocity"),
                "body": notes,
                "prerelease": true,
            }),
        )?;
        Ok(found(true))
    }
}

fn one_issue_notice(kept: u32) -> String {
    format!("Velocity keeps one open pin-bump issue: #{kept}.")
}

/// Whether `gh` reported that the repository, issue or release does not exist, which is
/// also GitHub's answer for a private repository the account cannot read.
fn not_found(error: &GitHubCommandError) -> bool {
    matches!(error, GitHubCommandError::Command { message, .. } if message.contains("HTTP 404"))
}

#[derive(Deserialize)]
struct ApiCommit {
    sha: String,
    commit: CommitDetail,
}

#[derive(Deserialize)]
struct CommitDetail {
    message: String,
}

impl ApiCommit {
    fn into_commit(self) -> RosenCommit {
        RosenCommit {
            commit: self.sha,
            message: self.commit.message,
        }
    }
}

#[derive(Deserialize)]
struct CheckRunsResponse {
    #[serde(default)]
    check_runs: Vec<Run>,
}

#[derive(Deserialize)]
struct WorkflowRunsResponse {
    #[serde(default)]
    workflow_runs: Vec<crate::workflow_runs::WorkflowRun>,
}

#[derive(Deserialize)]
struct CombinedStatus {
    #[serde(default)]
    statuses: Vec<CommitStatus>,
}

#[derive(Deserialize)]
struct CommitStatus {
    state: String,
}

#[derive(Deserialize)]
struct Run {
    status: String,
    conclusion: Option<String>,
}

#[derive(Deserialize)]
struct Titled {
    number: u32,
    title: String,
}

#[derive(Deserialize)]
struct Listed {
    number: u32,
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    pull_request: Option<Value>,
}

#[derive(Deserialize)]
struct Numbered {
    number: u32,
}
