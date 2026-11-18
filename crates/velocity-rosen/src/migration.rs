use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use rosen_client::{Client, ObjectId};
use rosen_store::ObjectStore;

use crate::{ImportSource, ProjectServer, RosenError};

mod history;

#[derive(Debug, PartialEq, Eq)]
pub struct Migration {
    pub source: ImportSource,
    base: String,
    branches: BTreeMap<String, String>,
    worktrees: BTreeMap<PathBuf, Checkout>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Checkout {
    name: String,
    tip: String,
}

impl Migration {
    pub fn inspect(repository: &Path, branch: &str) -> Result<Self, RosenError> {
        let reference = format!("refs/heads/{branch}");
        let base = git(repository, &["rev-parse", "--verify", &reference])?
            .trim_end()
            .to_owned();
        let worktrees = worktrees(repository)?;
        let branches = unmerged_branches(repository, &reference, &worktrees, &base)?;
        let mirror = if git(repository, &["remote"])?
            .lines()
            .any(|name| name == "origin")
        {
            let origin = git(repository, &["remote", "get-url", "--push", "origin"])?;
            Some(crate::server::absolute_remote(
                origin.trim_end(),
                repository,
            ))
        } else {
            None
        };
        Ok(Self {
            source: ImportSource {
                source: repository.to_string_lossy().into_owned(),
                branch: branch.to_owned(),
                mirror,
            },
            base,
            branches,
            worktrees,
        })
    }

    pub fn verify(&self, server: &ProjectServer) -> Result<(), RosenError> {
        let current = Self::inspect(Path::new(&self.source.source), &self.source.branch)?;
        if self != &current {
            return Err(failure(
                "Git changed during migration. Velocity will retry.",
            ));
        }
        if crate::git_base::read_base(server)?.as_deref() != Some(self.base.as_str()) {
            return Err(failure(
                "Rosen's existing base differs from the local default branch. Reconcile the histories before migration.",
            ));
        }
        Ok(())
    }

    pub async fn import_changes(
        &self,
        server: &ProjectServer,
        workspace_root: &Path,
        threads: &[(u32, PathBuf)],
    ) -> Result<BTreeMap<u32, (String, PathBuf)>, RosenError> {
        let client = server.connect().await?;
        let store = client.local_store().await?;
        let base = store
            .read_commit(&object_id(&self.base)?)
            .map_err(failure)?
            .tree;
        let mut changes = BTreeMap::new();
        for (branch, tip) in &self.branches {
            let change = self.import_branch(&client, branch, tip, base).await?;
            changes.insert(branch.as_str(), change);
        }
        let mut migrated = BTreeMap::new();
        for (thread, path) in threads {
            let Some(checkout) = path
                .canonicalize()
                .ok()
                .and_then(|path| self.worktrees.get(&path))
            else {
                if path.try_exists().map_err(failure)? {
                    return Err(failure(format!(
                        "Cannot match the saved thread folder {} to a Git worktree.",
                        path.display()
                    )));
                }
                continue;
            };
            let change = match changes.remove(checkout.name.as_str()) {
                Some(change) => Ok(change),
                None => {
                    let name = format!("{} (thread {thread})", checkout.name);
                    self.import_branch(&client, &name, &checkout.tip, base)
                        .await
                }
            }?;
            let root = history::workspace(&client, workspace_root, *thread, &change).await?;
            migrated.insert(*thread, (change.change.id.to_string(), root));
        }
        Ok(migrated)
    }

    async fn import_branch(
        &self,
        client: &Client,
        branch: &str,
        tip: &str,
        base: ObjectId,
    ) -> Result<rosen_client::protocol::ChangeRecord, RosenError> {
        let store = client.local_store().await?;
        let (repository, base_commit, tip) = (
            PathBuf::from(&self.source.source),
            self.base.clone(),
            tip.to_owned(),
        );
        let identity = format!("{branch}\0{base_commit}\0{tip}");
        let imported = tokio::task::spawn_blocking(move || {
            history::read_history(&repository, &base_commit, &tip, &store)
        })
        .await
        .map_err(failure)??;
        history::record(client, &identity, branch, base, imported).await
    }
}

fn unmerged_branches(
    repository: &Path,
    reference: &str,
    worktrees: &BTreeMap<PathBuf, Checkout>,
    base: &str,
) -> Result<BTreeMap<String, String>, RosenError> {
    let refs = git(
        repository,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname:lstrip=2)",
            "--no-merged",
            reference,
            "refs/heads/",
        ],
    )?;
    let mut branches = BTreeMap::new();
    for line in refs.lines() {
        let (tip, branch) = line
            .split_once(' ')
            .ok_or_else(|| failure("Invalid Git branch list"))?;
        branches.insert(branch.to_owned(), tip.to_owned());
    }
    for checkout in worktrees.values() {
        if checkout.name.starts_with("detached ") {
            let fork = git(repository, &["merge-base", base, &checkout.tip])?;
            if fork.trim_end() != checkout.tip {
                branches.insert(checkout.name.clone(), checkout.tip.clone());
            }
        }
    }
    Ok(branches)
}

fn worktrees(repository: &Path) -> Result<BTreeMap<PathBuf, Checkout>, RosenError> {
    let list = git(repository, &["worktree", "list", "--porcelain", "-z"])?;
    let mut worktrees = BTreeMap::new();
    for record in list.split("\0\0").filter(|record| !record.is_empty()) {
        let mut fields = record.split('\0');
        let path = fields
            .next()
            .and_then(|field| field.strip_prefix("worktree "))
            .ok_or_else(|| failure("Invalid Git worktree list"))?;
        let tip = fields
            .next()
            .and_then(|field| field.strip_prefix("HEAD "))
            .ok_or_else(|| failure(format!("Git worktree {path} has no current commit")))?;
        let status = git(
            Path::new(path),
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
        )
        .map_err(|error| {
            failure(format!(
                "Cannot inspect {path}. Restore the worktree before retrying. {error}"
            ))
        })?;
        if !status.is_empty() {
            return Err(failure(format!(
                "{path} has uncommitted work. Commit or preserve it to continue migration."
            )));
        }
        let name = fields
            .next()
            .and_then(|field| field.strip_prefix("branch refs/heads/"))
            .map_or_else(|| format!("detached {tip}"), str::to_owned);
        worktrees.insert(
            Path::new(path).canonicalize().map_err(failure)?,
            Checkout {
                name,
                tip: tip.to_owned(),
            },
        );
    }
    Ok(worktrees)
}

fn git(repository: &Path, arguments: &[&str]) -> Result<String, RosenError> {
    let output = git_command(repository)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(failure)?;
    if !output.status.success() {
        return Err(failure(String::from_utf8_lossy(&output.stderr).trim_end()));
    }
    String::from_utf8(output.stdout).map_err(failure)
}

fn git_command(repository: &Path) -> std::process::Command {
    let mut command = crate::process::hidden_command("git");
    command
        .arg("-C")
        .arg(repository)
        .env("GIT_OPTIONAL_LOCKS", "0");
    command
}

fn object_id(value: &str) -> Result<ObjectId, RosenError> {
    value.parse().map_err(failure)
}

fn failure(message: impl std::fmt::Display) -> RosenError {
    RosenError::Migration(message.to_string())
}

pub async fn migration_event_cursor(server: &ProjectServer) -> Result<u64, RosenError> {
    Ok(server.connect().await?.status().await?.last_event)
}
