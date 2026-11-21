//! A view of an immutable Rosen tree. Equal directory IDs skip entire subtrees.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use rosen_client::protocol::{
    ObjectQuery, OverlapQuery, ReadAnswer, ReadRequest, Request, Response,
};
use rosen_client::snapshot::WorkspaceMarker;
use rosen_client::{Client, Endpoint};
use rosen_core::{FileMode, Object, ObjectId, ObjectKind, Tree};

use crate::Error;

static SHARED_RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();

thread_local! {
    static CONNECT_DURATION: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

pub(crate) fn reset_connect_duration() {
    CONNECT_DURATION.with(|cell| cell.set(Duration::ZERO));
}

pub(crate) fn take_connect_duration() -> Duration {
    CONNECT_DURATION.with(|cell| cell.replace(Duration::ZERO))
}

fn record_connect_duration(elapsed: Duration) {
    CONNECT_DURATION.with(|cell| cell.set(cell.get().saturating_add(elapsed)));
}

fn shared_runtime() -> Result<&'static tokio::runtime::Runtime, Error> {
    SHARED_RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| Error::Invalid(error.clone()))
}

async fn connect_timed(
    endpoint: &Endpoint,
) -> Result<std::sync::Arc<velocity_rosen::connection::Connection>, Error> {
    let (connection, elapsed) = velocity_rosen::connection::requests_timed(endpoint).await?;
    record_connect_duration(elapsed);
    Ok(connection)
}

async fn check_transport<T>(
    endpoint: &Endpoint,
    connection: &std::sync::Arc<velocity_rosen::connection::Connection>,
    result: Result<T, Error>,
) -> Result<T, Error> {
    if let Err(Error::Rosen(rosen_client::Error::Disconnected | rosen_client::Error::Io(_))) =
        &result
    {
        velocity_rosen::connection::invalidate(endpoint, connection).await;
    }
    result
}

pub struct Source {
    endpoint: Endpoint,
    workspace: bool,
    pub tree: Option<ObjectId>,
    directories: HashMap<ObjectId, Tree>,
}

impl Source {
    pub fn use_workspace(&mut self, workspace: bool) {
        self.workspace = workspace;
    }

    pub fn new(endpoint: Endpoint, workspace: bool) -> Result<Self, Error> {
        let _ = shared_runtime()?;
        Ok(Self {
            endpoint,
            workspace,
            tree: None,
            directories: HashMap::new(),
        })
    }

    /// Clones this source's directory cache and current tree so a branched workspace
    /// diffs only its changed subtrees against the parent view (#541).
    pub(crate) fn clone_for_branch(&self, workspace: bool) -> Self {
        Self {
            endpoint: self.endpoint.clone(),
            workspace,
            tree: self.tree,
            directories: self.directories.clone(),
        }
    }

    pub fn workspace_tree(&self, root: &Path) -> Result<Option<ObjectId>, Error> {
        self.workspace
            .then(|| Ok(WorkspaceMarker::read(root)?.tree))
            .transpose()
    }

    pub fn revision_tree(&self, revision: &str) -> Result<ObjectId, Error> {
        self.request(async {
            let client = connect_timed(&self.endpoint).await?;
            let request = ReadRequest {
                at: Some(revision.to_owned()),
                path: String::new(),
            };
            let answer = check_transport(
                &self.endpoint,
                &client,
                client.read_file(request).await.map_err(Error::from),
            )
            .await?;
            match answer {
                ReadAnswer::Directory { id, .. } => Ok(id),
                ReadAnswer::File { .. } => {
                    Err(Error::Invalid("Rosen revision has no root tree".into()))
                }
            }
        })
    }

    pub fn changes(&mut self, tree: ObjectId) -> Result<BTreeMap<String, Option<ObjectId>>, Error> {
        let directories = &mut self.directories;
        let previous = self.tree;
        let endpoint = &self.endpoint;
        wait(shared_runtime()?, async {
            let client = connect_timed(endpoint).await?;
            let result = changes(&client, directories, previous, tree).await;
            check_transport(endpoint, &client, result).await
        })
    }

    pub fn blobs(&self, ids: Vec<ObjectId>) -> Result<HashMap<ObjectId, Vec<u8>>, Error> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        self.request(async {
            let client = connect_timed(&self.endpoint).await?;
            let result = objects(&client, ids, ObjectKind::Blob).await;
            check_transport(&self.endpoint, &client, result).await
        })
    }

    pub fn read(&self, path: &str) -> Result<String, Error> {
        let tree = self
            .tree
            .ok_or_else(|| Error::Invalid("Rosen graph has no tree".into()))?;
        self.request(async {
            let client = connect_timed(&self.endpoint).await?;
            let request = ReadRequest {
                at: Some(tree.to_string()),
                path: path.to_owned(),
            };
            let answer = check_transport(
                &self.endpoint,
                &client,
                client.read_file(request).await.map_err(Error::from),
            )
            .await?;
            match answer {
                ReadAnswer::File { data, .. } => {
                    String::from_utf8(data).map_err(|error| Error::Invalid(error.to_string()))
                }
                ReadAnswer::Directory { .. } => {
                    Err(Error::Invalid("Expected a source file".into()))
                }
            }
        })
    }

    pub fn overlaps(
        &self,
        root: &Path,
        graph: &crate::graph::Graph,
        paths: Vec<String>,
    ) -> Result<serde_json::Value, Error> {
        let change = WorkspaceMarker::read(root)?.change.to_string();
        let filter_paths = paths.clone();
        self.request(async {
            let client = connect_timed(&self.endpoint).await?;
            let query = OverlapQuery {
                change: Some(change.clone()),
                paths,
            };
            let answer = check_transport(
                &self.endpoint,
                &client,
                client.overlaps(query).await.map_err(Error::from),
            )
            .await?;
            let mut overlaps = Vec::new();
            for overlap in answer.overlaps.iter().take(8) {
                let symbols =
                    crate::coordination::rosen_overlap_symbols(&client, &change, overlap).await;
                overlaps.push(serde_json::json!({
                    "change": overlap.change, "owner": overlap.owner, "paths": overlap.paths, "symbols": symbols,
                }));
            }
            let signature_warnings = crate::signature_impact::change_stale_reference_warnings(
                &client,
                graph,
                &change,
                &filter_paths,
            )
            .await;
            Ok(serde_json::json!({
                "overlaps": overlaps,
                "total": answer.overlaps.len(),
                "signature_warnings": signature_warnings,
            }))
        })
    }

    pub fn change_signature_warnings(
        &self,
        graph: &crate::graph::Graph,
        change: &str,
    ) -> Result<Vec<crate::signature_impact::StaleReferenceWarning>, Error> {
        let change = change.to_owned();
        self.request(async {
            let client = connect_timed(&self.endpoint).await?;
            Ok(crate::signature_impact::change_stale_reference_warnings(
                &client,
                graph,
                &change,
                &[],
            )
            .await)
        })
    }

    pub fn dispatch_signature_warnings(
        &self,
        graph: &crate::graph::Graph,
        texts: &[String],
        exclude_change: Option<&str>,
    ) -> Result<Vec<crate::signature_impact::StaleReferenceWarning>, Error> {
        let texts = texts.to_vec();
        let exclude = exclude_change.map(str::to_owned);
        self.request(async {
            let client = connect_timed(&self.endpoint).await?;
            Ok(crate::signature_impact::dispatch_stale_reference_warnings(
                &client,
                graph,
                &texts,
                exclude.as_deref(),
            )
            .await)
        })
    }

    pub fn pieces(&self) -> Result<Vec<crate::spec_outline::LiveClaim>, Error> {
        let endpoint = &self.endpoint;
        shared_runtime()?.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), async {
                let client = connect_timed(endpoint).await?;
                let pieces = check_transport(
                    endpoint,
                    &client,
                    client.pieces().await.map_err(Error::from),
                )
                .await?;
                Ok(pieces
                    .into_iter()
                    .map(|claim| crate::spec_outline::LiveClaim {
                        piece: claim.piece,
                        task: claim.task,
                        nick: claim.nick,
                    })
                    .collect())
            })
            .await
            .map_err(|_| Error::Invalid("Timed out reading Rosen piece claims".into()))?
        })
    }

    fn request<T>(&self, future: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
        wait(shared_runtime()?, future)
    }
}

fn wait<T>(
    runtime: &tokio::runtime::Runtime,
    future: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), future)
            .await
            .map_err(|_| Error::Invalid("Timed out reading the Rosen graph tree".into()))?
    })
}

async fn changes(
    client: &Client,
    directories: &mut HashMap<ObjectId, Tree>,
    previous: Option<ObjectId>,
    tree: ObjectId,
) -> Result<BTreeMap<String, Option<ObjectId>>, Error> {
    let mut changed = BTreeMap::new();
    let mut retired = HashSet::new();
    let mut current = HashSet::new();
    let mut pending = vec![(String::new(), previous, Some(tree))];
    while !pending.is_empty() {
        pending.retain(|(_, old, new)| old != new);
        let ids = pending
            .iter()
            .flat_map(|(_, old, new)| [*old, *new])
            .flatten()
            .filter(|id| !directories.contains_key(id))
            .collect();
        for (id, bytes) in objects(client, ids, ObjectKind::Tree).await? {
            let Ok(Object::Tree(entries)) = Object::decode_body(ObjectKind::Tree, &bytes) else {
                return Err(Error::Invalid(format!("Invalid Rosen directory {id}")));
            };
            directories.insert(id, entries);
        }
        for (folder, old, new) in std::mem::take(&mut pending) {
            retired.extend(old);
            current.extend(new);
            let old = old.and_then(|id| directories.get(&id));
            let new = new.and_then(|id| directories.get(&id));
            compare_directory(&folder, old, new, &mut pending, &mut changed);
        }
    }
    for directory in retired.difference(&current) {
        directories.remove(directory);
    }
    Ok(changed)
}

type DirectoryChange = (String, Option<ObjectId>, Option<ObjectId>);

fn compare_directory(
    folder: &str,
    old: Option<&Tree>,
    new: Option<&Tree>,
    pending: &mut Vec<DirectoryChange>,
    changed: &mut BTreeMap<String, Option<ObjectId>>,
) {
    let mut entries = BTreeMap::new();
    for entry in old.into_iter().flat_map(Tree::entries) {
        entries.entry(&entry.name).or_insert((None, None)).0 = Some(entry);
    }
    for entry in new.into_iter().flat_map(Tree::entries) {
        entries.entry(&entry.name).or_insert((None, None)).1 = Some(entry);
    }
    for (name, (old, new)) in entries {
        if old == new {
            continue;
        }
        let Ok(name) = std::str::from_utf8(name) else {
            continue;
        };
        let path = if folder.is_empty() {
            name.to_owned()
        } else {
            format!("{folder}/{name}")
        };
        let directory = |entry: Option<&rosen_core::TreeEntry>| {
            entry
                .filter(|entry| entry.mode.is_tree())
                .map(|entry| entry.id)
        };
        let old_tree = directory(old);
        let new_tree = directory(new);
        let blob = |entry: Option<&rosen_core::TreeEntry>| {
            entry.filter(|entry| matches!(entry.mode, FileMode::Regular | FileMode::Executable)
                || matches!(entry.mode, FileMode::Legacy(mode) if mode & 0o170000 == 0o100000))
                .map(|entry| entry.id)
        };
        if old_tree != new_tree {
            pending.push((path.clone(), old_tree, new_tree));
        }
        if blob(old) != blob(new) {
            changed.insert(path, blob(new));
        }
    }
}

async fn objects(
    client: &Client,
    mut ids: Vec<ObjectId>,
    kind: ObjectKind,
) -> Result<HashMap<ObjectId, Vec<u8>>, Error> {
    ids.sort_unstable();
    ids.dedup();
    let mut result = HashMap::new();
    for chunk in ids.chunks(128) {
        let mut pending = chunk.to_vec();
        while !pending.is_empty() {
            let request = Request::GetObjects(ObjectQuery {
                ids: pending.clone(),
            });
            let Response::Objects(batch) = client.request(request).await? else {
                return Err(Error::Invalid("Expected Rosen objects".into()));
            };
            if !batch.missing.is_empty() || (kind == ObjectKind::Tree && !batch.large.is_empty()) {
                return Err(Error::Invalid(
                    "Rosen graph tree references unavailable objects".into(),
                ));
            }
            let previous = pending.len();
            for object in batch.objects {
                if ObjectKind::from(object.kind) != kind {
                    return Err(Error::Invalid(
                        "Unexpected object kind in Rosen graph tree".into(),
                    ));
                }
                result.insert(
                    ObjectId::hash_object(kind.as_str(), &object.data),
                    object.data,
                );
            }
            pending.retain(|id| !result.contains_key(id) && !batch.large.contains(id));
            if pending.len() == previous {
                return Err(Error::Invalid("Rosen object batch did not advance".into()));
            }
        }
    }
    Ok(result)
}
