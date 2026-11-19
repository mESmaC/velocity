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
    self, CheckRun, CiState, LandedIssue, OpenIssue, PinBump, PinBumpStep, RosenCommit, RosenPin,
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
    /// The commits from the newest green one down to the pin, newest first, and whether
    /// more lie between them and the pin than GitHub's first page listed.
    Green {
        commits: Vec<RosenCommit>,
        more: bool,
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
        let endpoint = format!("repos/{}", self.velocity);
        let push = self
            .command
            .text(&["api", &endpoint, "--jq", ".permissions.push"])?;
        Ok(push.trim() == "true")
    }

    /// Brings the pin-bump issue and the pinned Rosen release up to date. `known_release`
    /// is the release an earlier look found on Rosen. Fails only when the pin can't be read.
    pub fn look(&self, known_release: Option<&str>) -> Result<PinLook, GitHubCommandError> {
        let pin = self.pin()?;
        Ok(PinLook {
            issue: self.keep_issue_current(&pin),
            release: self.publish_release(&pin, known_release),
        })
    }

    fn keep_issue_current(&self, pin: &RosenPin) -> Result<IssueLook, GitHubCommandError> {
        let open = self
            .open_issues(&self.velocity)?
            .ok_or_else(|| GitHubCommandError::Missing(format!("{}'s issues", self.velocity)))?;
        let (issue, closed) = match self.ahead(pin)? {
            Ahead::Green { commits, more } => {
                let bump = self.bump(pin, &commits, more, &open)?;
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

    fn ahead(&self, pin: &RosenPin) -> Result<Ahead, GitHubCommandError> {
        let rosen = &pin.repository;
        let commits: Vec<Commit> = self.command.read(&[
            "api",
            &format!("repos/{rosen}/commits?per_page=100"),
            "--jq",
            "[.[] | {sha, message: .commit.message}]",
        ])?;
        let commits: Vec<RosenCommit> = commits.into_iter().map(Commit::into_commit).collect();
        let (ahead, found) = rosen_pin::commits_ahead(&commits, &pin.revision);
        let Some(newest) = ahead.first() else {
            return Ok(Ahead::Nothing);
        };
        // A pin missing from the newest page is either far behind or not on the branch at
        // all, such as a pin on a commit that was never merged. Only the first is a bump.
        if !found && !self.contains(rosen, &newest.commit, &pin.revision)? {
            return Ok(Ahead::Nothing);
        }
        for (index, commit) in ahead.iter().enumerate().take(GREEN_SCAN) {
            if self.ci_state(rosen, &commit.commit)? == CiState::Green {
                return Ok(Ahead::Green {
                    commits: ahead[index..].to_vec(),
                    more: !found,
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
        let endpoint = format!("repos/{repository}/compare/{ancestor}...{descendant}?per_page=1");
        let status = self.command.text(&["api", &endpoint, "--jq", ".status"])?;
        Ok(status.trim() == "ahead")
    }

    fn ci_state(&self, repository: &str, commit: &str) -> Result<CiState, GitHubCommandError> {
        let commit_endpoint = format!("repos/{repository}/commits/{commit}");
        let runs: Vec<Run> = self.command.read(&[
            "api",
            &format!("{commit_endpoint}/check-runs?per_page=100"),
            "--jq",
            "[.check_runs[] | {status, conclusion}]",
        ])?;
        let statuses: Vec<String> = self.command.read(&[
            "api",
            &format!("{commit_endpoint}/status?per_page=100"),
            "--jq",
            "[.statuses[].state]",
        ])?;
        let runs: Vec<CheckRun<'_>> = runs
            .iter()
            .map(|run| CheckRun {
                completed: run.status == "completed",
                conclusion: run.conclusion.as_deref(),
            })
            .collect();
        let statuses: Vec<&str> = statuses.iter().map(String::as_str).collect();
        Ok(rosen_pin::ci_state(&runs, &statuses))
    }

    /// What the issue says for the green commit at the head of `commits`.
    fn bump(
        &self,
        pin: &RosenPin,
        commits: &[RosenCommit],
        more: bool,
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
        let recent: Vec<Titled> = self.command.read(&[
            "api",
            &format!("{issues}?state=all&sort=updated&direction=desc&per_page=100"),
            "--jq",
            "[.[] | {number, title}]",
        ])?;
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
                match self.command.text(&["api", &endpoint, "--jq", ".title"]) {
                    Ok(title) => Ok((number, title.trim().to_owned())),
                    Err(error) if not_found(&error) => Ok((number, String::new())),
                    Err(error) => Err(error),
                }
            })
            .collect()
    }

    /// The repository's open issues without its pull requests, or `None` when the signed-in
    /// account cannot read the repository.
    fn open_issues(&self, repository: &str) -> Result<Option<Vec<OpenIssue>>, GitHubCommandError> {
        let listed = self.command.text(&[
            "api",
            "--paginate",
            &format!("repos/{repository}/issues?state=open&per_page=100"),
            "--jq",
            ".[] | select(.pull_request == null) | {number, title, body: (.body // \"\")} | @json",
        ]);
        let listed = match listed {
            Ok(listed) => listed,
            Err(error) if not_found(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        listed
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let issue: Listed = serde_json::from_str(line)?;
                Ok(OpenIssue {
                    repository: repository.to_owned(),
                    number: issue.number,
                    title: issue.title,
                    body: issue.body,
                })
            })
            .collect::<Result<Vec<_>, GitHubCommandError>>()
            .map(Some)
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
                (opened.number, PinIssue::Opened(opened.number))
            }
            PinBumpStep::Rewrite(number) => {
                let _: Value = self
                    .command
                    .api("PATCH", &format!("{issues}/{number}"), &text)?;
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
struct Commit {
    sha: String,
    message: String,
}

impl Commit {
    fn into_commit(self) -> RosenCommit {
        RosenCommit {
            commit: self.sha,
            message: self.message,
        }
    }
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
    body: String,
}

#[derive(Deserialize)]
struct Numbered {
    number: u32,
}
