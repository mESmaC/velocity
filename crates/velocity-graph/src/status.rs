use std::collections::BTreeMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::graph::RecordKind;
use crate::index::Index;
use crate::text::digest;
use crate::usage::{self, ToolUsageRow};

pub use crate::files::stage_git_base;

#[derive(Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub enum IndexState {
    Indexing,
    Ready,
    Stale,
    Failed,
}

#[derive(Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct GraphStatus {
    pub state: IndexState,
    pub detail: String,
    pub progress: Option<u32>,
    pub base: Option<BaseCoverage>,
    pub catching_up: Vec<String>,
    pub cache_size: String,
    pub tools: Vec<ToolUsageRow>,
    pub access: String,
    pub threads: Vec<usage::ThreadUsageRow>,
}

#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct BaseCoverage {
    pub commit: String,
    pub updated: String,
    pub languages: Vec<LanguageCoverage>,
    pub skipped: Vec<SkippedFiles>,
    pub hit_rate: String,
    parser: u32,
    build: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct LanguageCoverage {
    pub language: String,
    pub files: u32,
    pub symbols: u32,
    pub edges: u32,
}

#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct SkippedFiles {
    pub reason: String,
    pub files: u32,
}

#[derive(Serialize, Deserialize)]
struct Attempt {
    commit: String,
    detail: String,
    progress: Option<u32>,
}

pub struct ProjectStatus {
    cache: PathBuf,
    workspaces: Vec<ThreadWorkspace>,
    usage: usage::ProjectUsage,
}

pub struct ThreadWorkspace {
    pub(crate) thread: u32,
    pub(crate) path: PathBuf,
    pub(crate) title: String,
}

impl ThreadWorkspace {
    pub fn new(thread: u32, path: PathBuf, title: String) -> Self {
        Self {
            thread,
            path,
            title,
        }
    }
}

impl ProjectStatus {
    pub fn new(app_data: &Path, project: &str, workspaces: Vec<ThreadWorkspace>) -> Self {
        Self {
            cache: usage::folder(app_data, project),
            workspaces,
            usage: usage::read(app_data, project),
        }
    }

    pub fn unavailable(&self, error: String) -> GraphStatus {
        self.view(IndexState::Failed, error, None)
    }

    /// Claim the base build across concurrent modal requests. The OS releases the lease
    /// if Velocity exits during a build, so abandoned progress is never shown as live.
    pub fn observe(
        &self,
        commit: &str,
        retry: bool,
    ) -> Result<(GraphStatus, Option<BaseJob>), Error> {
        if !self.usage.enabled {
            return Ok((self.current(commit), None));
        }
        std::fs::create_dir_all(&self.cache)?;
        let lease = open_lock(&self.cache.join("base-index.lock"))?;
        match lease.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok((self.indexing(), None)),
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
        let current = self.current(commit);
        if !retry && !matches!(current.state, IndexState::Stale) {
            return Ok((current, None));
        }
        let job = BaseJob {
            cache: self.cache.clone(),
            commit: commit.to_owned(),
            progress: Mutex::new(0),
            _lease: lease,
        };
        let _ = std::fs::remove_file(self.cache.join("base-error.json"));
        job.update("Preparing the base snapshot", None);
        let view = if retry || current.base.is_none() {
            self.indexing()
        } else {
            current
        };
        Ok((view, Some(job)))
    }

    fn current(&self, commit: &str) -> GraphStatus {
        if let Some(failure) = read_json::<Attempt>(&self.cache.join("base-error.json"))
            && failure.commit == commit
        {
            return self.unavailable(failure.detail);
        }
        if read_json::<BaseCoverage>(&self.cache.join("base-coverage.json")).is_some_and(|base| {
            base.commit == commit
                && base.parser == crate::cache::PARSER_VERSION
                && base.build == env!("CARGO_PKG_VERSION")
        }) {
            return self.view(IndexState::Ready, "Base index is current".into(), None);
        }
        let detail = if self.usage.enabled {
            "Base index needs updating"
        } else {
            "Turn on the code graph to update the base index"
        };
        self.view(IndexState::Stale, detail.into(), None)
    }

    fn indexing(&self) -> GraphStatus {
        let attempt = read_json::<Attempt>(&self.cache.join("base-progress.json"));
        let (detail, progress) = attempt
            .map(|attempt| (attempt.detail, attempt.progress))
            .unwrap_or_else(|| ("Preparing the base snapshot".into(), None));
        self.view(IndexState::Indexing, detail, progress)
    }

    fn view(&self, state: IndexState, detail: String, progress: Option<u32>) -> GraphStatus {
        let catching_up = self
            .workspaces
            .iter()
            .filter(|workspace| WorkspaceStatus::catching_up(&self.cache, &workspace.path))
            .map(|workspace| workspace.title.clone())
            .collect();
        let bytes = self
            .cache
            .join("parses.log")
            .metadata()
            .map_or(0, |file| file.len());
        GraphStatus {
            state,
            detail,
            progress,
            base: read_json(&self.cache.join("base-coverage.json")),
            catching_up,
            cache_size: disk_size(bytes),
            tools: self.usage.tool_rows(),
            access: self.usage.access_summary(),
            threads: self.usage.thread_rows(&self.workspaces),
        }
    }
}

pub struct BaseJob {
    cache: PathBuf,
    commit: String,
    progress: Mutex<usize>,
    _lease: File,
}

impl BaseJob {
    pub fn index(&self, snapshot: &Path) -> Result<(), Error> {
        self.update("Scanning tracked files", None);
        let index = Index::open_snapshot(snapshot, &self.cache, |done, total| {
            self.progress(done, total)
        })?;
        self.store_coverage(&index)
    }

    pub fn index_rosen(&self, endpoint: rosen_client::Endpoint) -> Result<(), Error> {
        self.update("Reading the Rosen base tree", None);
        let index = Index::open_revision_with_progress(
            endpoint,
            &self.commit,
            &self.cache,
            |done, total| self.progress(done, total),
        )?;
        self.store_coverage(&index)
    }

    fn progress(&self, done: usize, total: usize) {
        let mut latest = self
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if done < *latest {
            return;
        }
        *latest = done;
        let detail = if done == total {
            "Resolving graph edges".into()
        } else {
            format!("{done} / {total} files processed")
        };
        let percent = (done * 100).checked_div(total).unwrap_or(100);
        self.update(&detail, Some(percent as u32));
    }

    fn store_coverage(&self, index: &Index) -> Result<(), Error> {
        write_json(
            &self.cache.join("base-coverage.json"),
            &self.coverage(index),
        )
    }

    pub fn fail(&self, detail: String) {
        let error = Attempt {
            commit: self.commit.clone(),
            detail,
            progress: None,
        };
        let _ = write_json(&self.cache.join("base-error.json"), &error);
    }

    fn update(&self, detail: &str, progress: Option<u32>) {
        let attempt = Attempt {
            commit: self.commit.clone(),
            detail: detail.into(),
            progress,
        };
        let _ = write_json(&self.cache.join("base-progress.json"), &attempt);
    }

    fn coverage(&self, index: &Index) -> BaseCoverage {
        let mut languages = BTreeMap::<String, LanguageCoverage>::new();
        for record in &index.graph.records {
            let language = language_name(index, &record.path);
            if record.kind == RecordKind::Subsystem {
                continue;
            }
            let row = languages.entry(language).or_default();
            match record.kind {
                RecordKind::File => row.files += 1,
                RecordKind::Symbol(_) => row.symbols += 1,
                RecordKind::Subsystem => {}
            }
        }
        for edge in &index.graph.edges {
            let origin = &index.graph.records[edge.from];
            let language = if origin.kind == RecordKind::Subsystem {
                "Subsystem".into()
            } else {
                language_name(index, &origin.path)
            };
            languages.entry(language).or_default().edges += 1;
        }
        BaseCoverage {
            commit: self.commit.clone(),
            updated: chrono::Local::now()
                .format("%-d %b %Y, %H:%M:%S")
                .to_string(),
            languages: languages
                .into_iter()
                .map(|(language, mut row)| {
                    row.language = language;
                    row
                })
                .collect(),
            skipped: index
                .skipped
                .iter()
                .map(|(reason, files)| SkippedFiles {
                    reason: (*reason).into(),
                    files: *files,
                })
                .collect(),
            hit_rate: usage::share(
                index.report.cached as u64,
                (index.report.parsed + index.report.cached) as u64,
            ),
            parser: crate::cache::PARSER_VERSION,
            build: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

fn language_name(index: &Index, path: &str) -> String {
    match index.language(path) {
        Some(crate::Language::CSharp) => "C#".into(),
        Some(crate::Language::Cpp) => "C++".into(),
        Some(crate::Language::Tsx) => "TSX".into(),
        Some(language) => format!("{language:?}"),
        None => "File records only".into(),
    }
}

fn disk_size(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.1} KiB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MiB", bytes as f64 / 1_048_576.0),
    }
}

pub(crate) struct WorkspaceStatus {
    path: PathBuf,
    lease: Option<File>,
}

impl WorkspaceStatus {
    pub(crate) fn track(cache: &Path, root: &Path) -> Self {
        let path = Self::path(cache, root);
        let lease = path
            .parent()
            .and_then(|folder| std::fs::create_dir_all(folder).ok())
            .and_then(|()| open_lock(&path.with_extension("lock")).ok())
            .filter(|lock| lock.try_lock().is_ok());
        let status = Self { path, lease };
        status.mark(false);
        status
    }

    fn path(cache: &Path, root: &Path) -> PathBuf {
        cache
            .join("workspaces")
            .join(format!("{}.json", workspace_key(root)))
    }

    fn catching_up(cache: &Path, root: &Path) -> bool {
        let path = Self::path(cache, root);
        let lock_path = path.with_extension("lock");
        lock_path.is_file()
            && open_lock(&lock_path)
                .is_ok_and(|lock| matches!(lock.try_lock(), Err(TryLockError::WouldBlock)))
            && read_json::<bool>(&path) != Some(true)
    }

    pub(crate) fn mark(&self, ready: bool) {
        if self.lease.is_some() {
            let _ = write_json(&self.path, &ready);
        }
    }
}

pub(crate) fn workspace_key(root: &Path) -> String {
    let key = root.to_string_lossy().replace('\\', "/");
    let key = if cfg!(windows) {
        key.to_ascii_lowercase()
    } else {
        key
    };
    digest(key.as_bytes())
}

fn open_lock(path: &Path) -> Result<File, Error> {
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?)
}

fn read_json<Value: serde::de::DeserializeOwned>(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temporary, serde_json::to_vec(value)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}
