//! Keeping Velocity's Rosen pin current (SPEC.md F5 "Rosen · Pin bumps", #513).
//!
//! Velocity pins Rosen by commit in its workspace `Cargo.toml`. When Rosen's `main` has a
//! green commit ahead of that pin, Velocity keeps exactly one open "Bump the Rosen pin to
//! `<commit>`" issue describing it. Everything here is the decision: what the pin is, which
//! commit is green, what the issue says and which issues to open, rewrite or close. The
//! GitHub reads and writes are in `velocity-github`.

use std::collections::BTreeSet;

/// Every pin-bump issue's title starts with this, which is how Velocity finds them.
pub const PIN_BUMP_TITLE: &str = "Bump the Rosen pin to ";

/// The last line of a pin-bump issue's body names the commit it asks for.
const TARGET_MARKER: &str = "<!-- velocity-rosen-pin-bump:";
/// Commits and tags are shown by this many characters, as GitHub shows them.
const SHORT_COMMIT: usize = 7;

/// The Rosen commit Velocity's workspace `Cargo.toml` pins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosenPin {
    /// Rosen's GitHub repository, `owner/name`.
    pub repository: String,
    /// The pinned commit, 40 hexadecimal characters.
    pub revision: String,
    /// The Rosen release built from that commit, empty when none is pinned.
    pub release: String,
}

impl RosenPin {
    /// Reads the pin as `.github/scripts/rosen-pin.ps1` does: the `rosen-client` dependency's
    /// `git` and `rev`, and `release` in `[workspace.metadata.rosen]`. `None` when the
    /// manifest pins no Rosen commit on GitHub.
    pub fn parse(manifest: &str) -> Option<Self> {
        let dependency = manifest
            .lines()
            .map(str::trim)
            .find(|line| key_of(line) == Some("rosen-client"))?;
        let url = string_value(dependency, "git")?;
        let revision = string_value(dependency, "rev")?;
        let repository = url
            .strip_prefix("https://github.com/")?
            .trim_end_matches('/')
            .trim_end_matches(".git");
        let named = repository
            .split('/')
            .filter(|part| !part.is_empty())
            .count()
            == 2;
        (named && crate::release::is_full_commit(revision)).then(|| Self {
            repository: repository.to_owned(),
            revision: revision.to_ascii_lowercase(),
            release: pinned_release(manifest).unwrap_or_default().to_owned(),
        })
    }

    /// The release Velocity publishes on Rosen for this pin: the one `Cargo.toml` names,
    /// when its tag carries the pinned commit. A tag that names another commit, or one the
    /// owner chose by hand, is never published from here.
    pub fn release_to_publish(&self) -> Option<&str> {
        let suffix = format!("-pin.g{}", short_commit(&self.revision));
        (self.release.ends_with(&suffix) && crate::release::release_tag(&self.release).is_some())
            .then_some(self.release.as_str())
    }
}

/// The key of a `key = value` manifest line.
fn key_of(line: &str) -> Option<&str> {
    line.split_once('=').map(|(key, _)| key.trim())
}

/// The string `key` is set to within `text`, for a table line or an inline table.
fn string_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut search = 0;
    while let Some(found) = text[search..].find(key) {
        let start = search + found;
        search = start + key.len();
        let standalone = text[..start]
            .chars()
            .next_back()
            .is_none_or(|before| !(before.is_ascii_alphanumeric() || "-_".contains(before)));
        let value = text[search..]
            .trim_start()
            .strip_prefix('=')
            .and_then(|rest| rest.trim_start().strip_prefix('"'))
            .and_then(|rest| rest.split_once('"'));
        if let (true, Some((value, _))) = (standalone, value) {
            return Some(value);
        }
    }
    None
}

/// `release` in the `[workspace.metadata.rosen]` table, read up to the next table.
fn pinned_release(manifest: &str) -> Option<&str> {
    manifest
        .lines()
        .map(str::trim)
        .skip_while(|line| *line != "[workspace.metadata.rosen]")
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .find(|line| key_of(line) == Some("release"))
        .and_then(|line| string_value(line, "release"))
}

/// The version a workspace manifest gives its packages, which names Rosen's releases.
pub fn package_version(manifest: &str) -> Option<&str> {
    ["[workspace.package]", "[package]"]
        .iter()
        .find_map(|table| {
            manifest
                .lines()
                .map(str::trim)
                .skip_while(|line| line != table)
                .skip(1)
                .take_while(|line| !line.starts_with('['))
                .find(|line| key_of(line) == Some("version"))
                .and_then(|line| string_value(line, "version"))
        })
}

pub fn short_commit(commit: &str) -> &str {
    commit.get(..SHORT_COMMIT).unwrap_or(commit)
}

/// The tag of the Rosen release a pin on `commit` names: Rosen's own version at that commit
/// and the commit, so each pinned commit has one tag and no tag is reused. `None` when
/// `version` or `commit` would not make a `v<semver>` tag.
pub fn release_tag(version: &str, commit: &str) -> Option<String> {
    let hexadecimal = commit.bytes().all(|byte| byte.is_ascii_hexdigit());
    let tag = format!("{}-pin.g{}", version.trim(), short_commit(commit));
    (hexadecimal && commit.len() >= SHORT_COMMIT)
        .then(|| crate::release::release_tag(&tag))
        .flatten()
}

/// What Rosen's CI says about one commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CiState {
    /// Every check and workflow ran and none failed.
    Green,
    /// A check or workflow run is queued or running.
    Running,
    Failed,
    /// No check or workflow ran on this commit, as on the commits a push carries below its tip.
    NotRun,
}

/// One GitHub check run: whether it completed, and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckRun<'a> {
    pub completed: bool,
    pub conclusion: Option<&'a str>,
}

/// One GitHub Actions workflow run for a commit: whether it completed, and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkflowRun<'a> {
    pub completed: bool,
    pub conclusion: Option<&'a str>,
}

/// The CI verdict on a commit from its check runs, workflow runs and commit statuses' states.
/// A failure decides before a running check or workflow does, because a rerun can't make the
/// commit green while another check or workflow has failed.
pub fn ci_state(
    check_runs: &[CheckRun<'_>],
    workflow_runs: &[WorkflowRun<'_>],
    statuses: &[&str],
) -> CiState {
    let passed =
        |conclusion: Option<&str>| matches!(conclusion, Some("success" | "neutral" | "skipped"));
    let failed = check_runs
        .iter()
        .any(|run| run.completed && !passed(run.conclusion))
        || workflow_runs
            .iter()
            .any(|run| run.completed && !passed(run.conclusion))
        || statuses
            .iter()
            .any(|state| matches!(*state, "failure" | "error"));
    let running = check_runs.iter().any(|run| !run.completed)
        || workflow_runs.iter().any(|run| !run.completed)
        || statuses.contains(&"pending");
    if failed {
        CiState::Failed
    } else if running {
        CiState::Running
    } else if check_runs.is_empty() && workflow_runs.is_empty() && statuses.is_empty() {
        CiState::NotRun
    } else {
        CiState::Green
    }
}

/// One commit on Rosen's `main`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosenCommit {
    pub commit: String,
    pub message: String,
}

/// The commits of `newest_first` that `main` has and the pin lacks, and whether the pin
/// was found below them. When it was not, the list is only the newest part of what is ahead.
pub fn commits_ahead<'a>(newest_first: &'a [RosenCommit], pin: &str) -> (&'a [RosenCommit], bool) {
    match newest_first
        .iter()
        .position(|commit| commit.commit.eq_ignore_ascii_case(pin))
    {
        Some(position) => (&newest_first[..position], true),
        None => (newest_first, false),
    }
}

/// The Rosen issues the commits' messages name as `#N`. A reference to another repository
/// (`owner/name#N`) is not one of Rosen's.
pub fn landed_issues(commits: &[RosenCommit]) -> BTreeSet<u32> {
    commits
        .iter()
        .flat_map(|commit| issue_references(&commit.message))
        .filter(|(before, _)| {
            before
                .chars()
                .next_back()
                .is_none_or(|last| !(last.is_alphanumeric() || "-_./&".contains(last)))
        })
        .map(|(_, number)| number)
        .collect()
}

/// Every `#N` in `text`, with the text of its line before the `#`.
fn issue_references(text: &str) -> Vec<(&str, u32)> {
    let mut references = Vec::new();
    for line in text.lines() {
        for (position, _) in line.match_indices('#') {
            let digits: String = line[position + 1..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(number) = digits.parse() {
                references.push((&line[..position], number));
            }
        }
    }
    references
}

/// An open issue Velocity read from GitHub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenIssue {
    /// `owner/name` of the issue's repository.
    pub repository: String,
    pub number: u32,
    pub title: String,
    pub body: String,
}

impl OpenIssue {
    pub fn is_pin_bump(&self) -> bool {
        self.title.starts_with(PIN_BUMP_TITLE)
    }

    /// The commit a pin-bump issue asks for, from its body's last line.
    pub fn pin_bump_target(&self) -> Option<&str> {
        self.body
            .lines()
            .next_back()?
            .trim()
            .strip_prefix(TARGET_MARKER)?
            .strip_suffix("-->")
            .map(str::trim)
    }
}

/// An open issue that names Rosen issues the bump brings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependent {
    pub repository: String,
    pub number: u32,
    pub title: String,
    pub needs: BTreeSet<u32>,
}

/// The open issues that name one of the `landed` Rosen issues, in the order given. An issue
/// names a Rosen issue as `<owner>/rosen#N`, `rosen#N`, "Rosen #N", "Rosen issue #N" or a
/// link to it. A bare `#N` is the issue's own repository's and does not count, and the
/// pin-bump issue, which lists them all, is left out.
pub fn dependents(issues: &[OpenIssue], rosen: &str, landed: &BTreeSet<u32>) -> Vec<Dependent> {
    issues
        .iter()
        .filter(|issue| !issue.is_pin_bump())
        .filter_map(|issue| {
            let mut needs = rosen_references(&issue.title, rosen);
            needs.extend(rosen_references(&issue.body, rosen));
            needs.retain(|number| landed.contains(number));
            (!needs.is_empty()).then(|| Dependent {
                repository: issue.repository.clone(),
                number: issue.number,
                title: issue.title.clone(),
                needs,
            })
        })
        .collect()
}

/// The Rosen issues `text` names; see `dependents` for the forms.
fn rosen_references(text: &str, rosen: &str) -> BTreeSet<u32> {
    let name = rosen.rsplit('/').next().unwrap_or(rosen).to_lowercase();
    let mut found: BTreeSet<u32> = issue_references(text)
        .into_iter()
        .filter(|(before, _)| names_repository(before, &name))
        .map(|(_, number)| number)
        .collect();
    let lowered = text.to_lowercase();
    for path in ["issues", "pull"] {
        let link = format!("github.com/{}/{path}/", rosen.to_lowercase());
        for (position, _) in lowered.match_indices(&link) {
            let digits: String = lowered[position + link.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            found.extend(digits.parse::<u32>().ok());
        }
    }
    found
}

/// Whether the text before a `#N` ends by naming the repository called `name`.
fn names_repository(before: &str, name: &str) -> bool {
    let lowered = before.trim_end().to_lowercase();
    ["", " issue", " issues"].iter().any(|noun| {
        lowered
            .strip_suffix(*noun)
            .and_then(|rest| rest.strip_suffix(name))
            .is_some_and(|rest| {
                rest.chars()
                    .next_back()
                    .is_none_or(|last| !(last.is_alphanumeric() || "-_.".contains(last)))
            })
    })
}

/// A Rosen issue landed since the pin; `title` is empty when GitHub's list did not have it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandedIssue {
    pub number: u32,
    pub title: String,
}

/// What the pin-bump issue asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinBump {
    pub pin: RosenPin,
    /// The newest green commit on Rosen's `main` ahead of the pin.
    pub target: String,
    /// How many commits from the pin to the target, and whether there are more than counted.
    pub commits: usize,
    pub more_commits: bool,
    /// The Rosen release the bump names for the target (`release_tag`).
    pub release: String,
    pub landed: Vec<LandedIssue>,
    pub dependents: Vec<Dependent>,
    /// Repositories whose open issues Velocity could not read.
    pub unread: Vec<String>,
}

impl PinBump {
    pub fn title(&self) -> String {
        format!("{PIN_BUMP_TITLE}{}", short_commit(&self.target))
    }

    /// The issue's whole text. It is the same for the same facts, so an unchanged issue is
    /// recognised by comparing it.
    pub fn body(&self) -> String {
        let (rosen, target, pin) = (&self.pin.repository, &self.target, &self.pin.revision);
        let count = match (self.commits, self.more_commits) {
            (count, true) => format!("more than {count} commits"),
            (1, false) => "1 commit".to_owned(),
            (count, false) => format!("{count} commits"),
        };
        let release = &self.release;
        [
            format!(
                "Rosen `main` is green at {rosen}@{target}, {count} ahead of the pin `{}` \
                 ([compare](https://github.com/{rosen}/compare/{pin}...{target})).\n\
                 Velocity keeps this issue current: a newer green commit rewrites it.",
                short_commit(pin)
            ),
            format!(
                "**Change.**\n\
                 - In the workspace `Cargo.toml`, set `rev` of every Rosen dependency to `{target}`.\n\
                 - Set `[workspace.metadata.rosen]` `release = \"{release}\"`. Once that lands, \
                 Velocity publishes the Rosen release `{release}` at the pinned commit, and Rosen's \
                 release workflow attaches the prebuilt binaries Velocity's release downloads.\n\
                 - Regenerate `Cargo.lock` and adapt Velocity to Rosen's API changes."
            ),
            format!("**Rosen issues landed since the pin.**\n{}", self.landed_list()),
            format!("**Open issues that name them.**\n{}", self.dependent_list()),
            format!(
                "**Done when** `Cargo.toml` pins `{target}` with release `{release}` and the \
                 integration build is green."
            ),
            format!("{TARGET_MARKER}{target} -->"),
        ]
        .join("\n\n")
    }

    fn landed_list(&self) -> String {
        if self.landed.is_empty() {
            return "No commit since the pin names a Rosen issue.".to_owned();
        }
        let rosen = &self.pin.repository;
        let lines: Vec<String> = self
            .landed
            .iter()
            .map(|issue| format!("- {rosen}#{}{}", issue.number, code_span(&issue.title)))
            .collect();
        lines.join("\n")
    }

    fn dependent_list(&self) -> String {
        let mut lines: Vec<String> = self
            .dependents
            .iter()
            .map(|issue| {
                let rosen = &self.pin.repository;
                let needs: Vec<String> = issue
                    .needs
                    .iter()
                    .map(|need| format!("{rosen}#{need}"))
                    .collect();
                format!(
                    "- {}#{}{} names {}",
                    issue.repository,
                    issue.number,
                    code_span(&issue.title),
                    needs.join(", ")
                )
            })
            .collect();
        if lines.is_empty() {
            lines.push("No open issue names these Rosen issues.".to_owned());
        }
        for repository in &self.unread {
            lines.push(format!(
                "`{repository}` could not be read, so its issues are not listed."
            ));
        }
        lines.join("\n")
    }
}

/// An issue's title as a code span after its reference, so a `@name` in it notifies nobody
/// and a `#N` in it links to no issue of the repository the list is posted on.
fn code_span(title: &str) -> String {
    let title = title.trim().replace('`', "'");
    if title.is_empty() {
        String::new()
    } else {
        format!(" `{title}`")
    }
}

/// What to do with the kept pin-bump issue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinBumpStep {
    /// No open issue asks for a bump: open one.
    Open,
    /// The kept issue says something else: rewrite its title and body.
    Rewrite(u32),
    /// The kept issue is current.
    Keep(u32),
}

/// How the open pin-bump issues become exactly one current issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinBumpPlan {
    pub step: PinBumpStep,
    /// Issues whose commit the default branch already pins: their bump landed, so they are
    /// closed instead of being rewritten into a second bump.
    pub finished: Vec<u32>,
    /// Further open pin-bump issues, closed in favour of the kept one.
    pub duplicates: Vec<u32>,
}

/// Keeps the oldest unfinished pin-bump issue and makes it say `bump`.
pub fn plan(open: &[OpenIssue], bump: &PinBump) -> PinBumpPlan {
    let mut issues: Vec<&OpenIssue> = open.iter().filter(|issue| issue.is_pin_bump()).collect();
    issues.sort_by_key(|issue| issue.number);
    let (finished, unfinished): (Vec<&OpenIssue>, Vec<&OpenIssue>) = issues
        .into_iter()
        .partition(|issue| issue.pin_bump_target() == Some(bump.pin.revision.as_str()));
    let step = match unfinished.first() {
        None => PinBumpStep::Open,
        Some(kept)
            if same_text(&kept.title, &bump.title()) && same_text(&kept.body, &bump.body()) =>
        {
            PinBumpStep::Keep(kept.number)
        }
        Some(kept) => PinBumpStep::Rewrite(kept.number),
    };
    PinBumpPlan {
        step,
        finished: finished.iter().map(|issue| issue.number).collect(),
        duplicates: unfinished
            .iter()
            .skip(1)
            .map(|issue| issue.number)
            .collect(),
    }
}

/// The oldest open pin-bump issue and the others, which are closed in its favour while no
/// bump is due.
pub fn oldest_and_surplus(open: &[OpenIssue]) -> Option<(u32, Vec<u32>)> {
    let mut numbers: Vec<u32> = open
        .iter()
        .filter(|issue| issue.is_pin_bump())
        .map(|issue| issue.number)
        .collect();
    numbers.sort_unstable();
    let (oldest, surplus) = numbers.split_first()?;
    Some((*oldest, surplus.to_vec()))
}

/// GitHub may return a body with other line endings than it was sent with.
fn same_text(left: &str, right: &str) -> bool {
    left.replace("\r\n", "\n").trim() == right.replace("\r\n", "\n").trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Risk (#569): a fast workflow (such as Proofs) can complete its check runs while a
    // slower workflow (such as CI) is still queued without having created any check runs yet.
    // Treating the commit as green before every workflow run finishes moves the Rosen pin
    // onto an untested commit.
    #[test]
    fn queued_workflow_run_keeps_commit_running_after_check_run_passes() {
        let check_runs = [CheckRun {
            completed: true,
            conclusion: Some("success"),
        }];
        let workflow_runs = [
            WorkflowRun {
                completed: true,
                conclusion: Some("success"),
            },
            WorkflowRun {
                completed: false,
                conclusion: None,
            },
        ];
        assert_eq!(ci_state(&check_runs, &workflow_runs, &[]), CiState::Running);
    }

    // Risk (#569): workflow runs that fail, cancel or error without creating check runs
    // (such as startup failures) must keep a commit from reading as green, and a failure
    // must still outrank another queued workflow run.
    #[test]
    fn workflow_run_outcomes_decide_ci_state() {
        let passing_check = [CheckRun {
            completed: true,
            conclusion: Some("success"),
        }];
        assert_eq!(ci_state(&[], &[], &[]), CiState::NotRun);
        assert_eq!(
            ci_state(
                &[],
                &[WorkflowRun {
                    completed: false,
                    conclusion: None,
                }],
                &[],
            ),
            CiState::Running
        );
        assert_eq!(
            ci_state(
                &passing_check,
                &[
                    WorkflowRun {
                        completed: true,
                        conclusion: Some("success"),
                    },
                    WorkflowRun {
                        completed: true,
                        conclusion: Some("skipped"),
                    },
                    WorkflowRun {
                        completed: true,
                        conclusion: Some("neutral"),
                    },
                ],
                &[],
            ),
            CiState::Green
        );
        for conclusion in [
            Some("failure"),
            Some("cancelled"),
            Some("timed_out"),
            Some("startup_failure"),
            None,
        ] {
            assert_eq!(
                ci_state(
                    &passing_check,
                    &[
                        WorkflowRun {
                            completed: true,
                            conclusion,
                        },
                        WorkflowRun {
                            completed: false,
                            conclusion: None,
                        },
                    ],
                    &[],
                ),
                CiState::Failed
            );
        }
    }
}
