//! One worktree's graph and how it stays current (F19 "Each agent's graph"): files hashed,
//! parses taken from the cache or made, and the graph rebuilt when any file changes.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::Error;
use crate::cache::{Cache, key};
use crate::extract::{FileParse, extract};
use crate::files::{SourceFile, inspect, list, read, walk};
use crate::graph::{FileInput, Graph};
use crate::language::Language;
use crate::subsystems::detect;

/// Parses at least this large take the project's build lock (see `load`).
const COLD_BUILD: usize = 64;

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
    pub(crate) skipped: BTreeMap<&'static str, u32>,
    pub graph: Graph,
    pub report: BuildReport,
}

impl Index {
    /// Indexes the worktree at `root`, with the project's parse cache in `cache_folder`.
    pub fn open(root: &Path, cache_folder: &Path) -> Result<Self, Error> {
        Self::open_paths(root, cache_folder, list(root)?, |_, _| {})
    }

    pub(crate) fn open_snapshot(
        root: &Path,
        cache_folder: &Path,
        progress: impl Fn(usize, usize) + Sync,
    ) -> Result<Self, Error> {
        Self::open_paths(root, cache_folder, walk(root, false)?, progress)
    }

    fn open_paths(
        root: &Path,
        cache_folder: &Path,
        paths: Vec<String>,
        progress: impl Fn(usize, usize) + Sync,
    ) -> Result<Self, Error> {
        let started = std::time::Instant::now();
        let mut index = Self {
            root: root.to_path_buf(),
            cache: Cache::open(cache_folder),
            files: BTreeMap::new(),
            manifests: HashMap::new(),
            skipped: BTreeMap::new(),
            graph: Graph::build(&[], Vec::new(), &HashMap::new()),
            report: BuildReport::default(),
        };
        let (parsed, cached) = index.load(&paths, progress)?;
        index.rebuild();
        let live: Vec<String> = index.live_keys();
        index.cache.compact(&live)?;
        // The log holds parses of every version of every file any agent saw; this agent
        // needs only its own files' (an edit's new parse is made, not looked up).
        index.cache.retain(&live);
        index.report = BuildReport {
            files: index.files.len(),
            parsed,
            cached,
            milliseconds: started.elapsed().as_millis(),
        };
        Ok(index)
    }

    pub(crate) fn language(&self, path: &str) -> Option<Language> {
        self.files.get(path).and_then(|state| state.language)
    }

    pub fn root(&self) -> &Path {
        &self.root
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
                None => changed |= self.files.remove(path).is_some(),
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
        let paths = list(&self.root)?;
        let listed: std::collections::HashSet<&String> = paths.iter().collect();
        let mut changed: Vec<String> = self
            .files
            .keys()
            .filter(|path| !listed.contains(path))
            .cloned()
            .collect();
        changed.extend(
            paths
                .iter()
                .filter(|path| !self.files.contains_key(*path))
                .cloned(),
        );
        self.refresh(&changed)
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
        let _build = if to_parse.len() >= COLD_BUILD {
            let guard = self.cache.build_lock()?;
            self.cache.reload();
            let (misses, hits) = self.take_cached(to_parse);
            to_parse = misses;
            cached += hits;
            Some(guard)
        } else {
            None
        };
        let total = paths.len();
        let already_processed = total.saturating_sub(to_parse.len());
        progress(already_processed, total);
        let completed = std::sync::atomic::AtomicUsize::new(already_processed);
        let parses = in_parallel(&to_parse, |source| {
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
        let mut fresh = Vec::new();
        for (source, parse) in to_parse.iter().zip(parses) {
            if let (Some(parse), Some(language)) = (&parse, source.language) {
                fresh.push((key(&source.hash, language), parse.clone()));
            }
            if parse.is_none() {
                *self.skipped.entry("Parser failed").or_default() += 1;
            }
            let parse = parse.unwrap_or_default();
            self.insert(source, parse);
        }
        self.cache.append(&fresh)?;
        Ok((to_parse.len(), cached))
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
            && let Ok(text) = std::fs::read_to_string(self.root.join(&source.path))
        {
            self.manifests.insert(source.path.clone(), text);
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
        self.graph = Graph::build(&inputs, subsystems, &self.manifests);
    }

    fn live_keys(&self) -> Vec<String> {
        self.files
            .values()
            .filter_map(|state| state.language.map(|language| key(&state.hash, language)))
            .collect()
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
