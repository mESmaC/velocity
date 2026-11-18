//! One worktree's graph and how it stays current (F19 "Each agent's graph"): files hashed,
//! parses taken from the cache or made, and the graph rebuilt when any file changes.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rosen_client::Endpoint;
use rosen_client::snapshot::WorkspaceMarker;
use rosen_core::ObjectId;

use crate::Error;
use crate::cache::{Cache, key};
use crate::extract::{FileParse, extract};
use crate::files::{SourceFile, from_blob, generated_name, inspect, list, read, walk};
use crate::graph::{FileInput, Graph};
use crate::language::Language;
use crate::subsystems::detect;

struct FileState {
    hash: String,
    language: Option<Language>,
    parse: Arc<FileParse>,
}

/// What a build or refresh did, for the tools' first answer and for tests of speed.
#[derive(Clone, Copy, Debug, Default)]
pub struct BuildReport {
    pub files: usize,
    pub parsed: usize,
    pub cached: usize,
    pub milliseconds: u128,
}

pub struct Index {
    root: PathBuf,
    cache: Cache,
    files: BTreeMap<String, FileState>,
    manifests: HashMap<String, String>,
    rosen: Option<crate::rosen::Source>,
    pub(crate) skipped: BTreeMap<&'static str, u32>,
    pub graph: Graph,
    pub report: BuildReport,
}

impl Index {
    /// Indexes the worktree at `root`, with the project's parse cache in `cache_folder`.
    pub fn open(root: &Path, cache_folder: &Path) -> Result<Self, Error> {
        let endpoint = std::env::var("ROSEN_ENDPOINT").ok().map(Endpoint::new);
        Self::open_workspace(root, cache_folder, endpoint)
    }

    pub fn open_workspace(
        root: &Path,
        cache_folder: &Path,
        endpoint: Option<Endpoint>,
    ) -> Result<Self, Error> {
        if WorkspaceMarker::path(root).is_file() {
            let endpoint = endpoint
                .ok_or_else(|| Error::Invalid("Rosen workspace needs a server endpoint".into()))?;
            let mut index = Self::empty(root, cache_folder);
            let mut source = crate::rosen::Source::new(endpoint, true)?;
            let tree = WorkspaceMarker::read(root)?.tree;
            index.load_tree(&mut source, tree, |_, _| {})?;
            index.rosen = Some(source);
            return Ok(index);
        }
        Self::open_paths(root, cache_folder, list(root)?, |_, _| {})
    }

    pub(crate) fn open_snapshot(
        root: &Path,
        cache_folder: &Path,
        progress: impl Fn(usize, usize) + Sync,
    ) -> Result<Self, Error> {
        Self::open_paths(root, cache_folder, walk(root)?, progress)
    }

    fn open_paths(
        root: &Path,
        cache_folder: &Path,
        paths: Vec<String>,
        progress: impl Fn(usize, usize) + Sync,
    ) -> Result<Self, Error> {
        let started = std::time::Instant::now();
        let mut index = Self::empty(root, cache_folder);
        let (parsed, cached) = index.load(&paths, progress)?;
        index.rebuild();
        index.report = BuildReport {
            files: index.files.len(),
            parsed,
            cached,
            milliseconds: started.elapsed().as_millis(),
        };
        Ok(index)
    }

    /// Opens an immutable commit or tree. Call from a blocking thread.
    pub fn open_revision(
        endpoint: Endpoint,
        revision: &str,
        cache_folder: &Path,
    ) -> Result<Self, Error> {
        Self::open_revision_with_progress(endpoint, revision, cache_folder, |_, _| {})
    }

    pub(crate) fn open_revision_with_progress(
        endpoint: Endpoint,
        revision: &str,
        cache_folder: &Path,
        progress: impl Fn(usize, usize) + Sync,
    ) -> Result<Self, Error> {
        let mut index = Self::empty(Path::new(revision), cache_folder);
        let mut source = crate::rosen::Source::new(endpoint, false)?;
        let tree = source.revision_tree(revision)?;
        index.load_tree(&mut source, tree, progress)?;
        index.rosen = Some(source);
        Ok(index)
    }

    /// Fills the shared cache without building the graph's search and edges.
    pub fn warm_base(
        endpoint: Endpoint,
        cache_folder: &Path,
        previous: Option<Self>,
    ) -> Result<Self, Error> {
        let mut index = previous.unwrap_or_else(|| Self::empty(Path::new("base"), cache_folder));
        let mut source = match index.rosen.take() {
            Some(source) => source,
            None => crate::rosen::Source::new(endpoint, false)?,
        };
        let tree = source.revision_tree("base")?;
        if source.tree != Some(tree) {
            index.update_tree(&mut source, tree, |_, _| {})?;
        }
        index.rosen = Some(source);
        Ok(index)
    }

    fn empty(root: &Path, cache_folder: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            cache: Cache::open(cache_folder),
            files: BTreeMap::new(),
            manifests: HashMap::new(),
            rosen: None,
            skipped: BTreeMap::new(),
            graph: Graph::build(&[], Vec::new(), &HashMap::new()),
            report: BuildReport::default(),
        }
    }

    pub fn is_rosen(&self) -> bool {
        self.rosen.is_some()
    }

    pub fn source(&self, path: &str) -> Result<String, Error> {
        match &self.rosen {
            Some(source) => source.read(path),
            None => Ok(std::fs::read_to_string(self.root.join(path))?),
        }
    }

    pub fn rosen_overlaps(&self, paths: Vec<String>) -> Result<serde_json::Value, Error> {
        let source = self
            .rosen
            .as_ref()
            .ok_or_else(|| Error::Invalid("graph_overlaps requires a Rosen workspace".into()))?;
        source.overlaps(&self.root, paths)
    }

    pub fn refresh_snapshot(&mut self) -> Result<bool, Error> {
        let Some(mut source) = self.rosen.take() else {
            return Ok(false);
        };
        let result = source
            .workspace_tree(&self.root)
            .and_then(|tree| match tree {
                Some(tree) if source.tree != Some(tree) => {
                    self.load_tree(&mut source, tree, |_, _| {}).map(|()| true)
                }
                _ => Ok(false),
            });
        self.rosen = Some(source);
        result
    }

    fn load_tree(
        &mut self,
        source: &mut crate::rosen::Source,
        tree: ObjectId,
        progress: impl Fn(usize, usize) + Sync,
    ) -> Result<(), Error> {
        let started = std::time::Instant::now();
        self.update_tree(source, tree, progress)?;
        self.rebuild();
        self.report.milliseconds = started.elapsed().as_millis();
        Ok(())
    }

    fn update_tree(
        &mut self,
        source: &mut crate::rosen::Source,
        tree: ObjectId,
        progress: impl Fn(usize, usize) + Sync,
    ) -> Result<(), Error> {
        let started = std::time::Instant::now();
        let paths = source.changes(tree)?;
        let build = self.cache.build_lock()?;
        if paths
            .iter()
            .any(|(path, id)| id.is_some_and(|id| self.needs_blob(path, id)))
        {
            self.cache.reload();
        }
        let total = paths.len();
        let paths: Vec<_> = paths.into_iter().collect();
        let (mut parsed, mut cached) = (0, 0);
        for (batch, paths) in paths.chunks(32).enumerate() {
            let (fresh, hits) = self.load_tree_batch(source, paths, |done, _| {
                progress(batch * 32 + done, total);
            })?;
            parsed += fresh;
            cached += hits;
        }
        drop(build);
        source.tree = Some(tree);
        self.report = BuildReport {
            files: self.files.len(),
            parsed,
            cached,
            milliseconds: started.elapsed().as_millis(),
        };
        Ok(())
    }

    fn load_tree_batch(
        &mut self,
        source: &crate::rosen::Source,
        paths: &[(String, Option<ObjectId>)],
        progress: impl Fn(usize, usize) + Sync,
    ) -> Result<(usize, usize), Error> {
        let blobs = source.blobs(
            paths
                .iter()
                .filter_map(|(path, id)| id.filter(|id| self.needs_blob(path, *id)))
                .collect(),
        )?;
        let mut sources = Vec::new();
        for (path, id) in paths {
            let (file, skipped) = id.map_or((None, None), |id| self.tree_file(path, id, &blobs));
            if let Some(reason) = skipped {
                *self.skipped.entry(reason).or_default() += 1;
            }
            if let Some(file) = file {
                sources.push(file);
            } else {
                self.files.remove(path);
                self.manifests.remove(path);
            }
        }
        let (misses, cached) = self.take_cached(sources);
        Ok((self.parse(misses, paths.len(), progress)?, cached))
    }

    fn needs_blob(&self, path: &str, id: ObjectId) -> bool {
        !generated_name(path)
            && (crate::subsystems::is_manifest(path)
                || Language::of(path)
                    .is_some_and(|language| !self.cache.contains(&key(&id.to_string(), language))))
    }

    fn tree_file(
        &self,
        path: &str,
        id: ObjectId,
        blobs: &HashMap<ObjectId, Vec<u8>>,
    ) -> (Option<SourceFile>, Option<&'static str>) {
        if generated_name(path) {
            return (None, Some("Generated"));
        }
        let hash = id.to_string();
        if let Some(bytes) = blobs.get(&id) {
            return from_blob(path, hash, bytes.clone());
        }
        let recognized = Language::of(path);
        let language = recognized.filter(|language| self.cache.contains(&key(&hash, *language)));
        let skipped = if language.is_none() {
            Some(if recognized.is_some() {
                "Over 1 MiB"
            } else {
                "No parser"
            })
        } else {
            None
        };
        (
            Some(SourceFile {
                path: path.to_owned(),
                language,
                hash,
                text: None,
            }),
            skipped,
        )
    }

    pub(crate) fn language(&self, path: &str) -> Option<Language> {
        self.files.get(path).and_then(|state| state.language)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn select_workspace(&mut self, root: &Path) -> Result<bool, Error> {
        let switched = self.root != root;
        self.root = root.to_path_buf();
        let refreshed = if let Some(source) = &mut self.rosen {
            source.use_workspace(true);
            self.refresh_snapshot()?
        } else if switched {
            self.refresh_all()?
        } else {
            false
        };
        if switched {
            self.graph.stale_addresses.clear();
        }
        Ok(refreshed)
    }

    pub(crate) fn select_base(&mut self) -> Result<bool, Error> {
        let Some(mut source) = self.rosen.take() else {
            return Ok(false);
        };
        let result = source.revision_tree("base").and_then(|tree| {
            if source.tree == Some(tree) {
                return Ok(false);
            }
            self.load_tree(&mut source, tree, |_, _| {}).map(|()| true)
        });
        self.root = PathBuf::from("base");
        self.graph.stale_addresses.clear();
        source.use_workspace(false);
        self.rosen = Some(source);
        result
    }

    pub fn cache_bytes(&self) -> u64 {
        self.cache.size()
    }

    /// Whether `path` is one of the indexed files.
    pub fn contains(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    /// Re-reads `paths` and rebuilds the graph if any changed. Returns whether any did.
    pub fn refresh(&mut self, paths: &[String]) -> Result<bool, Error> {
        let mut changed = false;
        let mut reread = Vec::new();
        for path in paths {
            match read(&self.root, path) {
                Some(file)
                    if self
                        .files
                        .get(path)
                        .is_some_and(|state| state.hash == file.hash) => {}
                Some(_) => reread.push(path.clone()),
                None => {
                    changed |= self.files.remove(path).is_some();
                    self.manifests.remove(path);
                }
            }
        }
        if !reread.is_empty() {
            self.load(&reread, |_, _| {})?;
            changed = true;
        }
        if changed {
            self.rebuild();
        }
        Ok(changed)
    }

    /// Lists the worktree again (new files, deleted files) and refreshes what differs.
    pub fn refresh_listing(&mut self) -> Result<bool, Error> {
        self.refresh_paths(false)
    }

    pub(crate) fn refresh_all(&mut self) -> Result<bool, Error> {
        self.refresh_paths(true)
    }

    fn refresh_paths(&mut self, all: bool) -> Result<bool, Error> {
        let paths = list(&self.root)?;
        let listed: std::collections::HashSet<&String> = paths.iter().collect();
        let previous = self.files.len();
        self.files.retain(|path, _| listed.contains(path));
        self.manifests.retain(|path, _| listed.contains(path));
        let removed = self.files.len() != previous;
        let changed: Vec<_> = paths
            .into_iter()
            .filter(|path| all || !self.files.contains_key(path))
            .collect();
        let refreshed = self.refresh(&changed)?;
        if removed && !refreshed {
            self.rebuild();
        }
        Ok(removed || refreshed)
    }

    /// The paths among `paths` whose bytes on disk no longer match the graph (the staleness
    /// check before an answer leaves the tools).
    pub fn stale(&self, paths: &[&str]) -> Vec<String> {
        paths
            .iter()
            .filter(|path| {
                let on_disk = read(&self.root, path).map(|file| file.hash);
                on_disk.as_ref() != self.files.get(**path).map(|state| &state.hash)
            })
            .map(|path| (*path).to_owned())
            .collect()
    }

    /// Reads and parses `paths` (in parallel), taking cached parses where they exist.
    fn load(
        &mut self,
        paths: &[String],
        progress: impl Fn(usize, usize) + Sync,
    ) -> Result<(usize, usize), Error> {
        let sources = in_parallel(paths, |path| Some(inspect(&self.root, path)));
        let sources: Vec<_> = sources
            .into_iter()
            .map(|entry| {
                let (source, skipped) = entry.unwrap_or((None, Some("Read failed")));
                if let Some(reason) = skipped {
                    *self.skipped.entry(reason).or_default() += 1;
                }
                source
            })
            .collect();
        for (path, source) in paths.iter().zip(&sources) {
            if source.is_none() {
                self.files.remove(path);
            }
        }
        let (mut to_parse, mut cached) = self.take_cached(sources.into_iter().flatten().collect());
        // A large parse (a new project, a cold cache) runs under the project's build lock, so
        // a swarm's agents starting together parse once: the others wait, then find the
        // parses in the cache.
        let _build = if !to_parse.is_empty() {
            let guard = self.cache.build_lock()?;
            self.cache.reload();
            let (misses, hits) = self.take_cached(to_parse);
            to_parse = misses;
            cached += hits;
            Some(guard)
        } else {
            None
        };
        Ok((self.parse(to_parse, paths.len(), progress)?, cached))
    }

    fn parse(
        &mut self,
        to_parse: Vec<SourceFile>,
        total: usize,
        progress: impl Fn(usize, usize) + Sync,
    ) -> Result<usize, Error> {
        let mut unique = BTreeMap::new();
        for source in &to_parse {
            if let Some(language) = source.language {
                unique.entry(key(&source.hash, language)).or_insert(source);
            }
        }
        let sources: Vec<_> = unique.into_iter().collect();
        let already_processed = total.saturating_sub(sources.len());
        progress(already_processed, total);
        let completed = std::sync::atomic::AtomicUsize::new(already_processed);
        let parses = in_parallel(&sources, |(_, source)| {
            let result = source
                .language
                .zip(source.text.as_deref())
                .map(|(language, text)| Arc::new(extract(language, text)));
            let done = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if done.is_multiple_of(32) || done == total {
                progress(done, total);
            }
            result
        });
        let fresh: Vec<_> = sources
            .into_iter()
            .zip(parses)
            .filter_map(|((key, _), parse)| parse.map(|parse| (key, parse)))
            .collect();
        self.cache.append(&fresh)?;
        if !fresh.is_empty() {
            self.cache.compact()?;
        }
        for source in to_parse {
            let parse = source
                .language
                .and_then(|language| self.cache.get(&key(&source.hash, language)));
            if parse.is_none() {
                *self.skipped.entry("Parser failed").or_default() += 1;
            }
            self.insert(&source, parse.unwrap_or_default());
        }
        Ok(fresh.len())
    }

    /// Takes every source the cache already holds (and every file with no parser), and
    /// returns the rest, with how many came from the cache.
    fn take_cached(&mut self, sources: Vec<SourceFile>) -> (Vec<SourceFile>, usize) {
        let mut misses = Vec::new();
        let mut cached = 0;
        for source in sources {
            let hit = source
                .language
                .and_then(|language| self.cache.get(&key(&source.hash, language)));
            match (hit, source.language) {
                (Some(parse), _) => {
                    cached += 1;
                    self.insert(&source, parse);
                }
                (None, Some(_)) => misses.push(source),
                (None, None) => self.insert(&source, Arc::new(FileParse::default())),
            }
        }
        (misses, cached)
    }

    fn insert(&mut self, source: &SourceFile, parse: Arc<FileParse>) {
        if crate::subsystems::is_manifest(&source.path)
            && let Some(text) = &source.text
        {
            self.manifests.insert(source.path.clone(), text.clone());
        }
        self.files.insert(
            source.path.clone(),
            FileState {
                hash: source.hash.clone(),
                language: source.language,
                parse,
            },
        );
    }

    fn rebuild(&mut self) {
        let paths: Vec<String> = self.files.keys().cloned().collect();
        self.manifests
            .retain(|path, _| self.files.contains_key(path));
        let subsystems = detect(&paths, |path| self.manifests.get(path).cloned());
        let inputs: Vec<FileInput> = self
            .files
            .iter()
            .map(|(path, state)| FileInput {
                path: path.clone(),
                hash: state.hash.clone(),
                language: state.language,
                parse: state.parse.clone(),
            })
            .collect();
        let mut graph = Graph::build(&inputs, subsystems, &self.manifests);
        graph.track_stale_addresses(&self.graph);
        self.graph = graph;
    }
}

/// `work` over `items` on up to eight threads, results in order.
fn in_parallel<Item: Sync, Output: Send>(
    items: &[Item],
    work: impl Fn(&Item) -> Option<Output> + Sync,
) -> Vec<Option<Output>> {
    let threads = std::thread::available_parallelism()
        .map_or(4, |count| count.get())
        .min(8);
    let chunk = items.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|part| {
                let handle = scope.spawn(|| part.iter().map(&work).collect::<Vec<_>>());
                (handle, part.len())
            })
            .collect();
        // A thread that panicked (a parser bug on one file) yields nothing for its files,
        // keeping the results in line with `items`.
        handles
            .into_iter()
            .flat_map(|(handle, length)| {
                handle
                    .join()
                    .unwrap_or_else(|_| (0..length).map(|_| None).collect())
            })
            .collect()
    })
}
