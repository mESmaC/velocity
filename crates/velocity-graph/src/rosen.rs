//! A view of an immutable Rosen tree. Equal directory IDs skip entire subtrees.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use rosen_client::protocol::{
    ObjectQuery, OverlapQuery, ReadAnswer, ReadRequest, Request, Response,
};
use rosen_client::snapshot::WorkspaceMarker;
use rosen_client::{Client, Endpoint};
use rosen_core::{FileMode, Object, ObjectId, ObjectKind, Tree};

use crate::Error;

pub struct Source {
    endpoint: Endpoint,
    runtime: tokio::runtime::Runtime,
    workspace: bool,
    pub tree: Option<ObjectId>,
    directories: HashMap<ObjectId, Tree>,
}

impl Source {
    pub fn use_workspace(&mut self, workspace: bool) {
        self.workspace = workspace;
    }

    pub fn new(endpoint: Endpoint, workspace: bool) -> Result<Self, Error> {
        Ok(Self {
            endpoint,
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?,
            workspace,
            tree: None,
            directories: HashMap::new(),
        })
    }

    pub fn workspace_tree(&self, root: &Path) -> Result<Option<ObjectId>, Error> {
        self.workspace
            .then(|| Ok(WorkspaceMarker::read(root)?.tree))
            .transpose()
    }

    pub fn revision_tree(&self, revision: &str) -> Result<ObjectId, Error> {
        self.request(async {
            let client = velocity_rosen::connection::requests(&self.endpoint).await?;
            let request = ReadRequest {
                at: Some(revision.to_owned()),
                path: String::new(),
            };
            match client.read_file(request).await? {
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
        wait(&self.runtime, async {
            let client = velocity_rosen::connection::requests(endpoint).await?;
            changes(&client, directories, previous, tree).await
        })
    }

    pub fn blobs(&self, ids: Vec<ObjectId>) -> Result<HashMap<ObjectId, Vec<u8>>, Error> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        self.request(async {
            let client = velocity_rosen::connection::requests(&self.endpoint).await?;
            objects(&client, ids, ObjectKind::Blob).await
        })
    }

    pub fn read(&self, path: &str) -> Result<String, Error> {
        let tree = self
            .tree
            .ok_or_else(|| Error::Invalid("Rosen graph has no tree".into()))?;
        self.request(async {
            let client = velocity_rosen::connection::requests(&self.endpoint).await?;
            let request = ReadRequest {
                at: Some(tree.to_string()),
                path: path.to_owned(),
            };
            match client.read_file(request).await? {
                ReadAnswer::File { data, .. } => {
                    String::from_utf8(data).map_err(|error| Error::Invalid(error.to_string()))
                }
                ReadAnswer::Directory { .. } => {
                    Err(Error::Invalid("Expected a source file".into()))
                }
            }
        })
    }

    pub fn overlaps(&self, root: &Path, paths: Vec<String>) -> Result<serde_json::Value, Error> {
        let change = WorkspaceMarker::read(root)?.change.to_string();
        self.request(async {
            let client = velocity_rosen::connection::requests(&self.endpoint).await?;
            let query = OverlapQuery { change: Some(change.clone()), paths };
            let answer = client.overlaps(query).await?;
            let mut overlaps = Vec::new();
            for overlap in answer.overlaps.iter().take(8) {
                let symbols = crate::coordination::rosen_overlap_symbols(&client, &change, overlap).await;
                overlaps.push(serde_json::json!({
                    "change": overlap.change, "owner": overlap.owner, "paths": overlap.paths, "symbols": symbols,
                }));
            }
            Ok(serde_json::json!({"overlaps": overlaps, "total": answer.overlaps.len()}))
        })
    }

    fn request<T>(&self, future: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
        wait(&self.runtime, future)
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
