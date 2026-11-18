use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use rosen_client::protocol::{ChangeFilter, ChangeRecord, NewChange, SnapshotRequest};
use rosen_client::{AgentId, Client, ObjectId};
use rosen_core::{ObjectKind, SnapshotKind};
use rosen_store::{LooseObjectStore, ObjectStore};

use super::{failure, git, git_command, object_id};
use crate::RosenError;

pub(super) struct History {
    fork: ObjectId,
    snapshots: Vec<(ObjectId, String)>,
}

pub(super) fn read_history(
    repository: &Path,
    base: &str,
    tip: &str,
    store: &LooseObjectStore,
) -> Result<History, RosenError> {
    let excluded = format!("^{base}");
    let commits = git(
        repository,
        &["rev-list", "--reverse", "--topo-order", tip, &excluded],
    )?;
    if commits.is_empty() {
        let commit = store.read_commit(&object_id(base)?).map_err(failure)?;
        return Ok(History {
            fork: commit.tree,
            snapshots: Vec::new(),
        });
    }
    let fork = git(repository, &["merge-base", base, tip]).map_err(|error| {
        failure(format!(
            "Cannot find a common ancestor for {tip}. Check Git history before retrying. {error}"
        ))
    })?;
    let objects = git(
        repository,
        &["rev-list", "--objects", "--no-object-names", tip, &excluded],
    )?;
    copy_objects(repository, objects.lines(), store)?;
    let fork = store
        .read_commit(&object_id(fork.trim_end())?)
        .map_err(failure)?
        .tree;
    let snapshots = commits
        .lines()
        .map(|id| {
            let commit = store.read_commit(&object_id(id)?).map_err(failure)?;
            let message = String::from_utf8_lossy(&commit.message);
            Ok((
                commit.tree,
                format!("Git commit {id}\n\n{}", message.trim_end()),
            ))
        })
        .collect::<Result<_, RosenError>>()?;
    Ok(History { fork, snapshots })
}

fn copy_objects<'a>(
    repository: &Path,
    ids: impl Iterator<Item = &'a str>,
    store: &LooseObjectStore,
) -> Result<(), RosenError> {
    let mut child = git_command(repository)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(failure)?;
    let result: Result<(), RosenError> = (|| {
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| failure("No Git object input"))?;
        let mut output = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| failure("No Git object output"))?,
        );
        for id in ids {
            if !store.contains_valid(&object_id(id)?).map_err(failure)? {
                copy_object(&mut input, &mut output, store, id)?;
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(failure)?;
    result?;
    if !status.success() {
        return Err(failure("Git could not read the branch's objects"));
    }
    Ok(())
}

fn copy_object(
    input: &mut impl Write,
    output: &mut impl BufRead,
    store: &LooseObjectStore,
    id: &str,
) -> Result<(), RosenError> {
    writeln!(input, "{id}").map_err(failure)?;
    input.flush().map_err(failure)?;
    let mut header = String::new();
    output.read_line(&mut header).map_err(failure)?;
    let mut fields = header.split_ascii_whitespace();
    let (Some(returned), Some(kind), Some(size)) = (fields.next(), fields.next(), fields.next())
    else {
        return Err(failure(format!("Git object {id} is unavailable")));
    };
    let kind = ObjectKind::parse(kind.as_bytes()).map_err(failure)?;
    let size: u64 = size.parse().map_err(failure)?;
    if returned != id
        || !store
            .put_raw_from(kind, object_id(id)?, size, &mut (&mut *output).take(size))
            .map_err(failure)?
    {
        return Err(failure(format!("Git object {id} did not match its hash")));
    }
    let mut delimiter = [0];
    output.read_exact(&mut delimiter).map_err(failure)?;
    if delimiter != [b'\n'] {
        return Err(failure(format!("Git returned invalid data for {id}")));
    }
    Ok(())
}

pub(super) async fn record(
    client: &Client,
    identity: &str,
    branch: &str,
    base: ObjectId,
    history: History,
) -> Result<ChangeRecord, RosenError> {
    let key = ObjectId::hash_object("blob", identity.as_bytes());
    let owner = AgentId::new(format!("git-import-{key}")).map_err(failure)?;
    let matches = client
        .change_list(ChangeFilter {
            owner: Some(owner.clone()),
            state: None,
        })
        .await?;
    let mut change = match matches.into_iter().next() {
        Some(change) => change,
        None => client.change_new(NewChange { owner, on: None }).await?,
    };
    if change.change.state.is_closed() {
        return Err(failure(format!(
            "Imported change for {branch} has closed. Review its Rosen state before retrying."
        )));
    }
    let saved = change
        .change
        .snapshots
        .iter()
        .filter_map(|snapshot| match &snapshot.kind {
            SnapshotKind::Named(message) => Some((&snapshot.tree, message)),
            SnapshotKind::Automatic => None,
        })
        .collect::<Vec<_>>();
    if saved.len() > history.snapshots.len()
        || !saved.iter().zip(&history.snapshots).all(
            |((saved_tree, saved_message), (tree, message))| {
                *saved_tree == tree && *saved_message == message
            },
        )
    {
        return Err(failure(format!(
            "The previous import of {branch} has additional snapshots. Review the Rosen change before retrying."
        )));
    }
    let start = saved.len();
    let count = history.snapshots.len();
    for (index, (tree, message)) in history.snapshots.into_iter().enumerate().skip(start) {
        let mut from = change.change.head;
        if index + 1 == count {
            // Earlier snapshots preserve each original tree. Merge the tip from its Git
            // fork against the Rosen base so intervening base edits are kept as well.
            if from != base {
                change = snapshot(client, &change, base, from, None).await?;
            }
            from = history.fork;
        }
        change = snapshot(client, &change, tree, from, Some(message))
            .await
            .map_err(|error| failure(format!("Could not import branch {branch}. {error}")))?;
    }
    Ok(change)
}

async fn snapshot(
    client: &Client,
    change: &ChangeRecord,
    tree: ObjectId,
    from: ObjectId,
    message: Option<String>,
) -> Result<ChangeRecord, RosenError> {
    Ok(client
        .snapshot(SnapshotRequest {
            change: change.change.id.to_string(),
            tree,
            from: Some(from),
            message,
        })
        .await?
        .change)
}

pub(super) async fn workspace(
    client: &Client,
    parent: &Path,
    thread: u32,
    change: &ChangeRecord,
) -> Result<PathBuf, RosenError> {
    for workspace in client.workspace_list().await? {
        if workspace.change == change.change.id
            && rosen_client::snapshot::WorkspaceMarker::read(&workspace.path)
                .is_ok_and(|marker| marker.change == change.change.id)
        {
            return Ok(workspace.path);
        }
    }
    std::fs::create_dir_all(parent).map_err(failure)?;
    let identifier = change.change.id;
    // A crash during materialisation can leave an unregistered directory. Keep it and
    // try a fresh path, since no persisted thread has switched to this location yet.
    for attempt in 0..100 {
        let root = parent.join(format!("migrated-{thread}-{identifier}-{attempt}"));
        if !root.try_exists().map_err(failure)? {
            client
                .materialise_change(change, &root, Default::default())
                .await?;
            return Ok(root);
        }
    }
    Err(failure(format!(
        "No unused workspace path for thread {thread} in {}",
        parent.display()
    )))
}
