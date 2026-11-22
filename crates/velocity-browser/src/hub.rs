use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::cdp::Frame;
use crate::engine::{Engine, MEMORY_BUDGET};
use crate::input::BrowserInput;
use crate::page::{Page, Tab};
use crate::tools::{self, Output, Wait};
use crate::{Result, error};

const IDLE: Duration = Duration::from_secs(600);
pub type BrowserKey = (u32, u32);

#[derive(Clone)]
pub struct Route {
    pub port: u16,
    pub token: Uuid,
}

#[derive(Clone, PartialEq, Serialize, specta::Type)]
pub struct BrowserView {
    pub view_id: String,
    pub owner: bool,
    pub active: Option<String>,
    pub tabs: Vec<Tab>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingProfileDeletion {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeleteOutcome {
    Removed,
    Pending(PendingProfileDeletion),
}

#[derive(Default)]
struct Registry {
    entries: HashMap<BrowserKey, Entry>,
    pending_deletions: BTreeSet<PathBuf>,
    port: Option<u16>,
}

#[derive(Clone)]
struct Entry {
    token: Option<Uuid>,
    profile: std::result::Result<PathBuf, String>,
    state: Arc<AsyncMutex<ThreadBrowser>>,
    pid: Arc<AtomicU32>,
}

struct Live {
    engine: Engine,
    page: Page,
    _slot: Slot,
}

struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct ThreadBrowser {
    live: Option<Live>,
    last_action: Instant,
    view: Option<String>,
    owner: bool,
    handback: Option<String>,
    retired: bool,
}

impl Default for ThreadBrowser {
    fn default() -> Self {
        Self {
            live: None,
            last_action: Instant::now(),
            view: None,
            owner: false,
            handback: None,
            retired: false,
        }
    }
}

#[derive(Clone, Default)]
pub struct BrowserHub {
    registry: Arc<Mutex<Registry>>,
    running: Arc<AtomicUsize>,
    starting: Arc<AtomicUsize>,
    limit: Arc<AtomicUsize>,
}

impl BrowserHub {
    fn registry(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn start(&self) -> Result<()> {
        let mut registry = self.registry();
        if registry.port.is_none() {
            registry.port = Some(crate::transport::listen(self.clone())?);
        }
        Ok(())
    }

    pub fn limit(&self, limit: u32) {
        self.limit
            .store(limit.clamp(1, 16) as usize, Ordering::Release);
    }

    pub fn register(&self, key: BrowserKey, profile: std::result::Result<PathBuf, String>) {
        let mut registry = self.registry();
        if let Ok(path) = &profile {
            registry.pending_deletions.remove(path);
        }
        let entry = registry.entries.entry(key).or_insert_with(|| Entry {
            token: None,
            profile: profile.clone(),
            state: Arc::default(),
            pid: Arc::default(),
        });
        if entry.profile.is_err() {
            entry.profile = profile;
        }
    }

    pub fn route(&self, key: BrowserKey) -> Result<Route> {
        let mut registry = self.registry();
        let port = registry
            .port
            .ok_or_else(|| error("The browser MCP listener is unavailable"))?;
        let entry = registry
            .entries
            .get_mut(&key)
            .ok_or_else(|| error("The thread has no browser profile location"))?;
        let token = *entry.token.get_or_insert_with(Uuid::new_v4);
        Ok(Route { port, token })
    }

    fn entry(&self, key: BrowserKey) -> Result<Entry> {
        self.registry()
            .entries
            .get(&key)
            .cloned()
            .ok_or_else(|| error("The thread browser has closed"))
    }

    fn authenticate(&self, token: Uuid) -> Result<Entry> {
        self.registry()
            .entries
            .values()
            .find(|entry| entry.token == Some(token))
            .cloned()
            .ok_or_else(|| error("This browser tool session has ended"))
    }

    fn admit(&self) -> Result<(Slot, Slot)> {
        let limit = self.limit.load(Ordering::Acquire).max(1);
        self.running
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < limit).then_some(count + 1)
            })
            .map_err(|_| {
                error("All agent browser slots are in use. Wait for a browser to close and retry.")
            })?;
        let slot = Slot(self.running.clone());
        let starting = self.starting.fetch_add(1, Ordering::AcqRel) as u64 + 1;
        let reservation = Slot(self.starting.clone());
        // Concurrent launches may not show up in machine memory readings yet.
        if !velocity_rosen::memory::has_headroom(starting * MEMORY_BUDGET) {
            return Err(error(
                "Machine memory is low. Wait before opening the browser and retry.",
            ));
        }
        Ok((slot, reservation))
    }

    pub fn processes(&self) -> Vec<(BrowserKey, u32)> {
        self.registry()
            .entries
            .iter()
            .filter_map(|(&key, entry)| {
                let pid = entry.pid.load(Ordering::Acquire);
                (pid != 0).then_some((key, pid))
            })
            .collect()
    }

    pub async fn stop(&self, key: BrowserKey) {
        let entry = self.registry().entries.get_mut(&key).map(|entry| {
            entry.token = None;
            entry.clone()
        });
        if let Some(entry) = entry {
            let mut state = entry.state.lock().await;
            entry.close(&mut state).await;
        }
    }

    pub async fn suspend(&self, key: BrowserKey, pid: u32) {
        if let Ok(entry) = self.entry(key) {
            let mut state = entry.state.lock().await;
            if entry.pid.load(Ordering::Acquire) == pid {
                entry.close(&mut state).await;
            }
        }
    }

    pub fn queue_profile_deletion(&self, path: PathBuf) {
        let mut registry = self.registry();
        if !path_in_use(&registry.entries, &path) {
            registry.pending_deletions.insert(path);
        }
    }

    pub fn forget_profile_deletion(&self, path: &Path) {
        self.registry().pending_deletions.remove(path);
    }

    pub fn pending_profile_deletions(&self) -> Vec<PathBuf> {
        self.registry().pending_deletions.iter().cloned().collect()
    }

    pub async fn delete(&self, key: BrowserKey, path: Option<&Path>) -> DeleteOutcome {
        let entry = self.registry().entries.remove(&key);
        let profile = if let Some(entry) = &entry {
            let mut state = entry.state.lock().await;
            state.retired = true;
            entry.close(&mut state).await;
            entry
                .profile
                .as_deref()
                .ok()
                .or(path)
                .map(Path::to_path_buf)
        } else {
            path.map(Path::to_path_buf)
        };
        let Some(profile) = profile else {
            return DeleteOutcome::Removed;
        };
        match remove_profile(&profile).await {
            Ok(()) => {
                self.forget_profile_deletion(&profile);
                DeleteOutcome::Removed
            }
            Err(error) => {
                self.queue_profile_deletion(profile.clone());
                DeleteOutcome::Pending(PendingProfileDeletion {
                    path: profile,
                    error: error.to_string(),
                })
            }
        }
    }

    pub async fn retry_pending_deletions(&self) -> Vec<PathBuf> {
        let pending: Vec<PathBuf> = {
            let registry = self.registry();
            registry
                .pending_deletions
                .iter()
                .filter(|path| !path_in_use(&registry.entries, path))
                .cloned()
                .collect()
        };
        let mut removed = Vec::new();
        for path in pending {
            if try_remove_profile(&path).await.is_ok() {
                self.forget_profile_deletion(&path);
                removed.push(path);
            }
        }
        removed
    }

    pub async fn reap(&self) -> Vec<PathBuf> {
        let entries: Vec<_> = self.registry().entries.values().cloned().collect();
        for entry in entries {
            if let Ok(mut state) = entry.state.try_lock()
                && state.live.is_some()
                && state.last_action.elapsed() >= IDLE
            {
                entry.close(&mut state).await;
            }
        }
        self.retry_pending_deletions().await
    }

    pub async fn open_view(&self, key: BrowserKey) -> Result<BrowserView> {
        let entry = self.entry(key)?;
        let mut state = entry.state.lock().await;
        let view = Uuid::new_v4().to_string();
        state.hand_back();
        state.view = Some(view.clone());
        state.view(&view).await.map(|(view, _)| view)
    }

    pub async fn view(&self, key: BrowserKey, view: &str) -> Result<(BrowserView, Option<Frame>)> {
        let entry = self.entry(key)?;
        let mut state = entry.state.lock().await;
        state.check_view(view)?;
        entry.close_disconnected(&mut state).await;
        state.view(view).await
    }

    pub async fn close_view(&self, key: BrowserKey, view: &str) -> Result<()> {
        let entry = self.entry(key)?;
        let mut state = entry.state.lock().await;
        state.check_view(view)?;
        state.hand_back();
        state.view = None;
        if let Some(live) = &mut state.live {
            live.page.watch(&live.engine.cdp, false).await?;
        }
        Ok(())
    }

    pub async fn take_over(&self, key: BrowserKey, view: &str) -> Result<()> {
        let entry = self.entry(key)?;
        let mut state = entry.state.lock().await;
        state.check_view(view)?;
        let live = state
            .live
            .as_mut()
            .ok_or_else(|| error("The agent has not opened a browser"))?;
        live.page.invalidate();
        live.engine.cdp.events(|events| {
            events.owner = true;
            events.console.clear();
            events.failures.clear();
        });
        state.owner = true;
        state.last_action = Instant::now();
        Ok(())
    }

    pub async fn hand_back(&self, key: BrowserKey, view: &str) -> Result<Option<String>> {
        let entry = self.entry(key)?;
        let mut state = entry.state.lock().await;
        state.check_view(view)?;
        if !state.owner {
            return Ok(None);
        }
        state.hand_back();
        let current = if let Some(live) = &state.live {
            live.page
                .tabs(&live.engine.cdp)
                .await
                .ok()
                .and_then(|tabs| {
                    let active = tabs
                        .iter()
                        .find(|tab| Some(&tab.id) == live.page.active.as_ref())?;
                    Some(format!(
                        "Active page: {}. Open tabs: {}.",
                        active.url,
                        tabs.len()
                    ))
                })
        } else {
            None
        };
        let message = format!(
            "The owner handed back the browser. {} Take a new snapshot before using element refs.",
            current
                .as_deref()
                .unwrap_or("The page or sign-in state may have changed.")
        );
        state.handback = Some(message.clone());
        Ok(Some(message))
    }

    pub async fn owner_input(
        &self,
        key: BrowserKey,
        view: &str,
        tab: &str,
        input: BrowserInput,
    ) -> Result<()> {
        let entry = self.entry(key)?;
        let mut state = entry.state.lock().await;
        state.check_owner(view)?;
        state.last_action = Instant::now();
        let live = state
            .live
            .as_ref()
            .ok_or_else(|| error("The browser closed"))?;
        if live.page.active.as_deref() != Some(tab) {
            return Err(error("The active browser tab changed"));
        }
        input.dispatch(&live.engine.cdp).await
    }

    pub async fn switch_tab(&self, key: BrowserKey, view: &str, tab: &str) -> Result<()> {
        let entry = self.entry(key)?;
        let mut state = entry.state.lock().await;
        state.check_owner(view)?;
        state.last_action = Instant::now();
        let live = state
            .live
            .as_mut()
            .ok_or_else(|| error("The browser closed"))?;
        live.page.switch(&live.engine.cdp, tab).await
    }

    pub(crate) async fn tool(&self, token: Uuid, name: &str, arguments: &Value) -> Result<Output> {
        let entry = self.authenticate(token)?;
        let profile = entry
            .profile
            .as_ref()
            .map_err(|message| error(message.clone()))?;
        let mut state = entry.state.lock().await;
        self.authenticate(token)?;
        entry.close_disconnected(&mut state).await;
        state.agent_available()?;
        if state.live.is_none() {
            let (slot, reservation) = self.admit()?;
            let engine = Engine::start(profile).await?;
            drop(reservation);
            entry.pid.store(engine.pid(), Ordering::Release);
            state.live = Some(Live {
                engine,
                page: Page::default(),
                _slot: slot,
            });
        }
        state.last_action = Instant::now();
        let watching = state.view.is_some();
        let handback = state.handback.clone();
        let live = state
            .live
            .as_mut()
            .ok_or_else(|| error("The browser closed"))?;
        let tabs = live.page.ensure_active(&live.engine.cdp).await?;
        live.page.watch(&live.engine.cdp, watching).await?;
        let output = if name == "wait_for" {
            let tab = live.page.active.clone();
            drop(state);
            let output = wait(&entry, tab, arguments).await?;
            state = entry.state.lock().await;
            output
        } else {
            tools::call(&mut live.page, &live.engine.cdp, profile, name, arguments).await?
        };
        if let Some(notice) = handback {
            if state.handback.as_ref() == Some(&notice) {
                state.handback = None;
            }
            return Ok(with_notice(output, notice, &tabs));
        }
        Ok(output)
    }
}

impl Entry {
    async fn close_disconnected(&self, state: &mut ThreadBrowser) {
        if state
            .live
            .as_ref()
            .is_some_and(|live| !live.engine.cdp.connected())
        {
            self.close(state).await;
        }
    }

    async fn close(&self, state: &mut ThreadBrowser) {
        state.hand_back();
        if let Some(live) = state.live.take() {
            live.engine.close().await;
        }
        self.pid.store(0, Ordering::Release);
    }
}

impl ThreadBrowser {
    fn check_view(&self, view: &str) -> Result<()> {
        if self.retired || self.view.as_deref() != Some(view) {
            return Err(error("This browser view has closed"));
        }
        Ok(())
    }

    fn check_owner(&self, view: &str) -> Result<()> {
        self.check_view(view)?;
        if !self.owner {
            return Err(error("Take over the browser to interact with it"));
        }
        Ok(())
    }

    fn agent_available(&self) -> Result<()> {
        if self.retired {
            return Err(error("This thread was deleted"));
        }
        if self.owner {
            return Err(error(
                "The owner is using the browser. Continue other work until they hand it back.",
            ));
        }
        Ok(())
    }

    fn hand_back(&mut self) {
        if !std::mem::take(&mut self.owner) {
            return;
        }
        self.last_action = Instant::now();
        if let Some(live) = &mut self.live {
            live.page.invalidate();
            live.engine.cdp.events(|events| {
                events.owner = false;
                events.console.clear();
                events.failures.clear();
            });
        }
        self.handback = Some(
            "The owner handed back the browser. Its page, tabs or sign-in state may have changed."
                .into(),
        );
    }

    async fn view(&mut self, id: &str) -> Result<(BrowserView, Option<Frame>)> {
        self.check_view(id)?;
        let (tabs, active, frame) = if let Some(live) = &mut self.live
            && live.engine.cdp.connected()
        {
            let tabs = live.page.ensure_active(&live.engine.cdp).await?;
            live.page.watch(&live.engine.cdp, true).await?;
            let frame = live.page.frame(&live.engine.cdp).await;
            (tabs, live.page.active.clone(), frame)
        } else {
            (Vec::new(), None, None)
        };
        Ok((
            BrowserView {
                view_id: id.to_owned(),
                owner: self.owner,
                tabs,
                active,
            },
            frame,
        ))
    }
}

async fn wait(entry: &Entry, tab: Option<String>, args: &Value) -> Result<Output> {
    let (criterion, timeout) = Wait::parse(args)?;
    let until = Instant::now() + timeout;
    loop {
        {
            let mut state = entry.state.lock().await;
            state.agent_available()?;
            state.last_action = Instant::now();
            let live = state
                .live
                .as_ref()
                .ok_or_else(|| error("The browser closed"))?;
            if live.page.active != tab {
                return Err(error("The active tab changed while waiting"));
            }
            if criterion.matches(&live.engine.cdp).await? {
                return Ok(Output::Text("Condition matched".into()));
            }
        }
        if Instant::now() >= until {
            return Err(error("Timed out waiting for the page condition"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn path_in_use(entries: &HashMap<BrowserKey, Entry>, path: &Path) -> bool {
    entries
        .values()
        .any(|entry| entry.profile.as_deref().ok() == Some(path))
}

pub async fn try_remove_profile(profile: &Path) -> Result<()> {
    match tokio::fs::remove_dir_all(profile).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

async fn remove_profile(profile: &Path) -> Result<()> {
    for attempt in 0..10 {
        match try_remove_profile(profile).await {
            Ok(()) => return Ok(()),
            Err(error) if attempt == 9 => return Err(error),
            Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
    Ok(())
}

fn with_notice(output: Output, notice: String, tabs: &[Tab]) -> Output {
    let notice = format!(
        "{notice}\nTabs after hand back: {}\n",
        crate::bounded(json!(tabs).to_string(), 1200)
    );
    match output {
        Output::Text(content) => Output::Text(crate::bounded(notice + &content, crate::TEXT_LIMIT)),
        Output::Image { path, jpeg } => Output::Image {
            path: notice + &path,
            jpeg,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_profile_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("velocity-browser-{tag}-{}", Uuid::new_v4()))
    }

    // Risk (#634): a transient Windows sharing violation during thread deletion must not lose
    // the browser profile path; BrowserHub must retain it for retry and delete it once the lock
    // clears.
    #[cfg(windows)]
    #[tokio::test]
    async fn delete_retains_and_retries_profile_after_sharing_violation()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use std::os::windows::fs::OpenOptionsExt;

        let profile = unique_profile_dir("sharing-violation");
        std::fs::create_dir_all(&profile)?;
        let marker = profile.join("profile-marker");
        let lease = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&marker)?;

        let hub = BrowserHub::default();
        hub.register((1, 42), Ok(profile.clone()));

        let outcome = hub.delete((1, 42), Some(&profile)).await;
        assert!(matches!(
            &outcome,
            DeleteOutcome::Pending(pending) if pending.path == profile
        ));
        assert_eq!(hub.pending_profile_deletions(), vec![profile.clone()]);

        // Still locked: retry keeps the pending deletion queued.
        assert!(hub.reap().await.is_empty());
        assert!(marker.exists());
        assert_eq!(hub.pending_profile_deletions(), vec![profile.clone()]);

        // Once the handle closes, retry deletes the profile directory and clears the queue.
        drop(lease);
        assert_eq!(hub.reap().await, vec![profile.clone()]);
        assert!(!profile.exists());
        assert!(hub.pending_profile_deletions().is_empty());
        Ok(())
    }

    // Risk (#634): a profile deletion request re-queued after restart must remove the leftover
    // directory once retried.
    #[tokio::test]
    async fn queued_profile_deletion_retries_after_restart()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let profile = unique_profile_dir("restart-retry");
        std::fs::create_dir_all(&profile)?;
        std::fs::write(profile.join("marker"), b"stale")?;

        let hub = BrowserHub::default();
        hub.queue_profile_deletion(profile.clone());
        assert_eq!(hub.reap().await, vec![profile.clone()]);
        assert!(!profile.exists());
        assert!(hub.pending_profile_deletions().is_empty());
        Ok(())
    }
}
