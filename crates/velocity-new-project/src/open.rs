//! Open project in the project switcher (SPEC.md F2): register a project that
//! already exists, from a folder or from your GitHub repositories.

use std::sync::Mutex;
use velocity_collapse::RepositoryActivity;
use velocity_core::layout_state::LayoutState;
use velocity_github::{GitHubRepository, RepositoryName};
use velocity_store::{PendingInterview, StateStore};

use crate::create::{Publication, set_up};
use crate::error::NewProjectError;
use crate::request::{CreatedProject, OpenProjectRequest};
use crate::resolve::resolve_open;

/// How many matching repositories the picker shows; the rest are counted.
const SHOWN_REPOSITORIES: usize = 60;

/// Opens or imports a Rosen project and switches to it. A base with a spec opens with it
/// locked; one without records the spec interview with Settings' defaults, which
/// starts once a provider is signed in (Rule 2, F3).
pub fn open_project(
    request: &OpenProjectRequest,
    store: &Mutex<StateStore>,
    layout: &Mutex<LayoutState>,
    publish: impl FnOnce(Publication) -> Result<(), String>,
) -> Result<CreatedProject, NewProjectError> {
    let resolved = resolve_open(request)?;
    let settings = store
        .lock()
        .map_err(|_| NewProjectError::Poisoned("Persisted state store"))?
        .settings();
    let interview = PendingInterview {
        // No model: the interview takes the signed-in provider's model when it starts.
        model_name: None,
        effort: settings.providers.default_effort,
        preferred_speed: settings.providers.default_speed,
        // The scan reads the existing code, so there is no brief to give.
        brief: String::new(),
    };
    set_up(&resolved, interview, store, layout, publish)
}

/// The switcher's GitHub picker: your repositories, read once through `gh` and then
/// searched here on every keystroke.
#[derive(Default)]
pub struct RepositoryPicker {
    repositories: Option<Vec<GitHubRepository>>,
}

/// What the picker shows for the current search.
#[derive(Clone, Debug, serde::Serialize, specta::Type)]
pub struct RepositoryPickerView {
    /// The matching repositories, most recently updated first.
    pub rows: Vec<GitHubRepository>,
    /// Matches left out of `rows`; typing more narrows them down.
    pub hidden: u32,
    /// The search is an `owner/name` that is not in your list, offered as its own
    /// row so any repository you can see still opens.
    pub typed: Option<String>,
}

impl RepositoryPicker {
    /// Whether the list must be read from GitHub before it can be searched.
    pub fn needs_fetch(&self, refresh: bool) -> bool {
        refresh || self.repositories.is_none()
    }

    pub fn replace(&mut self, repositories: Vec<GitHubRepository>) {
        self.repositories = Some(repositories);
    }

    pub fn view(&self, query: &str) -> RepositoryPickerView {
        let repositories = self.repositories.as_deref().unwrap_or_default();
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut matching = repositories
            .iter()
            .filter(|repository| matches_every_word(repository, &words));
        let rows: Vec<GitHubRepository> = matching
            .by_ref()
            .take(SHOWN_REPOSITORIES)
            .cloned()
            .collect();
        let hidden = u32::try_from(matching.count()).unwrap_or(u32::MAX);
        RepositoryPickerView {
            rows,
            hidden,
            typed: typed_repository(repositories, query),
        }
    }
}

fn matches_every_word(repository: &GitHubRepository, words: &[String]) -> bool {
    let name = repository.full_name.to_lowercase();
    let description = repository
        .description
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    words
        .iter()
        .all(|word| name.contains(word.as_str()) || description.contains(word.as_str()))
}

fn typed_repository(repositories: &[GitHubRepository], query: &str) -> Option<String> {
    if !query.contains('/') {
        return None;
    }
    let typed = RepositoryName::parse(query).ok()?.full_name();
    let listed = repositories
        .iter()
        .any(|repository| repository.full_name.eq_ignore_ascii_case(&typed));
    (!listed).then_some(typed)
}

/// One repository from the signed-in Collapse account's dashboard (#575).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct CollapseRepository {
    /// `owner/name`.
    pub full_name: String,
    pub description: Option<String>,
    pub private: bool,
    pub archived: bool,
    /// True when Collapse reports a linked Rosen server; Velocity offers machine
    /// enrollment instead of cloning a second independent server.
    pub has_linked_server: bool,
    /// RFC 3339 time of the last land, when Collapse has recorded one.
    pub last_land_at: Option<String>,
}

impl CollapseRepository {
    pub fn from_activity(activity: &RepositoryActivity) -> Self {
        Self {
            full_name: activity.full_name(),
            description: activity
                .description
                .clone()
                .filter(|text| !text.trim().is_empty()),
            private: activity.is_private(),
            archived: activity.is_archived(),
            has_linked_server: activity.has_linked_server(),
            last_land_at: activity.last_land.as_ref().map(|land| land.time.clone()),
        }
    }
}

/// The switcher's Collapse picker: repositories from Collapse's dashboard read, cached
/// and searched here on every keystroke (#575).
#[derive(Default)]
pub struct CollapseRepositoryPicker {
    repositories: Option<Vec<CollapseRepository>>,
}

/// What the Collapse picker shows for the current search (#575).
#[derive(Clone, Debug, serde::Serialize, specta::Type)]
pub struct CollapseRepositoryPickerView {
    pub rows: Vec<CollapseRepository>,
    pub hidden: u32,
    pub typed: Option<String>,
}

impl CollapseRepositoryPicker {
    pub fn needs_fetch(&self, refresh: bool) -> bool {
        refresh || self.repositories.is_none()
    }

    pub fn replace(&mut self, repositories: Vec<CollapseRepository>) {
        self.repositories = Some(repositories);
    }

    pub fn view(&self, query: &str) -> CollapseRepositoryPickerView {
        let repositories = self.repositories.as_deref().unwrap_or_default();
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut matching = repositories
            .iter()
            .filter(|repository| collapse_matches_every_word(repository, &words));
        let rows: Vec<CollapseRepository> = matching
            .by_ref()
            .take(SHOWN_REPOSITORIES)
            .cloned()
            .collect();
        let hidden = u32::try_from(matching.count()).unwrap_or(u32::MAX);
        CollapseRepositoryPickerView {
            rows,
            hidden,
            typed: typed_collapse_repository(repositories, query),
        }
    }
}

fn collapse_matches_every_word(repository: &CollapseRepository, words: &[String]) -> bool {
    let name = repository.full_name.to_lowercase();
    let description = repository
        .description
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    words
        .iter()
        .all(|word| name.contains(word.as_str()) || description.contains(word.as_str()))
}

fn typed_collapse_repository(repositories: &[CollapseRepository], query: &str) -> Option<String> {
    if !query.contains('/') {
        return None;
    }
    let (owner, name) = velocity_core::collapse::parse_repository(query)?;
    let typed = format!("{owner}/{name}");
    let listed = repositories
        .iter()
        .any(|repository| repository.full_name.eq_ignore_ascii_case(&typed));
    (!listed).then_some(typed)
}
