//! The warm worktree pool's workers (SPEC.md F5 Warm pool): per project, keep Settings →
//! Worktrees' "Keep N worktrees ready" checkouts at the tip of `master`, prebuilt, and hand
//! one to each new thread.
//!
//! Two background threads share the pool. The checkout worker fetches the tip, makes,
//! removes and catches slots up one at a time, so swarm launches never race each other on
//! setup. The prebuild worker builds one slot at a time below normal priority: Lean
//! toolchain and Mathlib's cache for Lake projects, then the Cargo build. A take marks
//! its slot taken under the pool's lock, so two threads never get the same worktree.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use velocity_core::ready_pool::{
    CheckoutWork, ReadySlot, SlotState, checkout_work, choose_ready, prebuild_work, wanted_slots,
};
use velocity_core::worktree_settings::WorktreePreferences;
use velocity_git::{
    FoundSlot, ReadyTake, WorktreeError, WorktreeSetupRequest, catch_up_ready_worktree,
    create_ready_worktree, prebuild_ready_worktree, ready_slot_path, remove_ready_worktree,
    scan_ready_slots, sweep_base,
};
use velocity_store::ProjectItem;

use crate::launch::{Location, base_branch, setup_settings};

/// How often the checkout worker fetches the base branch to see whether `master` moved.
const FETCH_EVERY: Duration = Duration::from_secs(60);
/// How long a worker sleeps when nothing wakes it sooner.
const IDLE: Duration = Duration::from_secs(30);
/// How long a swarm member waits for a ready worktree before making its own.
const SWARM_WAIT: Duration = Duration::from_secs(120);
/// A swarm's reservation lapses if its members never come for their worktrees.
const RESERVATION_LIFETIME: Duration = Duration::from_secs(300);

/// One project's pool settings, read from the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolProject {
    pub identifier: u32,
    pub repository_root: PathBuf,
    pub project_root: PathBuf,
    pub base_branch: String,
    /// "Keep N worktrees ready".
    pub standing: u32,
    pub copy_files: String,
    pub share_build_cache: bool,
}

impl PoolProject {
    /// The pool settings for `project` under Settings → Worktrees as they apply to it.
    pub fn new(
        project: &ProjectItem,
        worktrees: &WorktreePreferences,
    ) -> Result<Self, WorktreeError> {
        Ok(Self {
            identifier: project.id,
            repository_root: PathBuf::from(&project.repository_path),
            project_root: Location::new(project, worktrees).project_root()?,
            base_branch: base_branch(project),
            standing: worktrees.ready_count,
            copy_files: worktrees.copy_files.clone(),
            share_build_cache: worktrees.share_build_cache,
        })
    }

    fn slot_request(
        &self,
        index: u32,
        tip: &str,
        worktrees: &WorktreePreferences,
    ) -> WorktreeSetupRequest {
        WorktreeSetupRequest::detached(
            self.repository_root.clone(),
            ready_slot_path(&self.project_root, index),
            tip.to_owned(),
            setup_settings(worktrees),
            self.copy_files.clone(),
            String::new(),
            self.project_root.clone(),
        )
    }
}

/// Where the pool reads its projects and reports changes.
pub trait PoolHost: Send + Sync + 'static {
    /// The projects that keep a pool now, with their Settings → Worktrees.
    fn projects(&self) -> Vec<(PoolProject, WorktreePreferences)>;
    /// A slot was made, built, taken or removed: Settings → Worktrees lists them.
    fn changed(&self);
    /// A ready worktree now sits at the base tip, made or caught up after a merge: the code
    /// graph parses what changed, so new agents start with a full cache (F19 Warming).
    fn slot_at_tip(&self, _project: u32, _slot: &Path) {}
}

#[derive(Default)]
struct ProjectPool {
    project: Option<PoolProject>,
    preferences: Option<WorktreePreferences>,
    slots: Vec<ReadySlot>,
    tip: Option<String>,
    fetched: Option<Instant>,
    scanned: bool,
    reserved: u32,
    reserved_at: Option<Instant>,
}

impl ProjectPool {
    fn reserved(&self) -> u32 {
        match self.reserved_at {
            Some(at) if at.elapsed() < RESERVATION_LIFETIME => self.reserved,
            _ => 0,
        }
    }

    fn wanted(&self) -> u32 {
        let standing = self.project.as_ref().map_or(0, |project| project.standing);
        // "Keep 0 ready" turns the pool off, swarms included.
        if standing == 0 {
            return 0;
        }
        wanted_slots(standing, self.reserved())
    }

    fn set_state(&mut self, index: u32, state: SlotState) {
        match self.slots.iter_mut().find(|slot| slot.index == index) {
            Some(slot) => slot.state = state,
            None => self.slots.push(ReadySlot::new(index, state)),
        }
    }

    fn forget(&mut self, index: u32) {
        self.slots.retain(|slot| slot.index != index);
    }
}

#[derive(Default)]
struct Shared {
    pools: Mutex<BTreeMap<u32, ProjectPool>>,
    changed: Condvar,
}

/// The warm pool for every project. Cheap to clone; every clone shares one pool.
#[derive(Clone, Default)]
pub struct ReadyPools {
    shared: Arc<Shared>,
}

/// A ready worktree handed to one thread. Keep it until the thread's checkout is done:
/// dropping it frees the slot's index for the next ready worktree.
pub struct PoolTicket {
    pools: ReadyPools,
    project: u32,
    index: u32,
    pub ready: ReadyTake,
}

impl Drop for PoolTicket {
    fn drop(&mut self) {
        let mut pools = self.pools.lock();
        if let Some(pool) = pools.get_mut(&self.project) {
            pool.forget(self.index);
        }
        drop(pools);
        self.pools.shared.changed.notify_all();
    }
}

/// One slot as Settings → Worktrees lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolRow {
    pub path: PathBuf,
    pub state: SlotState,
}

impl ReadyPools {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts the checkout and prebuild workers; they run until the app exits.
    pub fn start<Host: PoolHost>(&self, host: Arc<Host>) -> std::io::Result<()> {
        let checkout = (self.clone(), Arc::clone(&host));
        std::thread::Builder::new()
            .name("velocity-ready-checkout".into())
            .spawn(move || checkout.0.checkout_loop(checkout.1.as_ref()))?;
        let prebuild = (self.clone(), host);
        std::thread::Builder::new()
            .name("velocity-ready-prebuild".into())
            .spawn(move || prebuild.0.prebuild_loop(prebuild.1.as_ref()))?;
        Ok(())
    }

    /// Looks again now: settings changed, or `master` moved.
    pub fn wake(&self, project: Option<u32>) {
        if let Some(project) = project {
            // The next pass fetches the tip again.
            if let Some(pool) = self.lock().get_mut(&project) {
                pool.fetched = None;
            }
        }
        self.shared.changed.notify_all();
    }

    /// A swarm is launching `count` agents: top the pool up to that many, and make them wait
    /// for a ready worktree rather than race each other on setup (F5 Swarm launches).
    pub fn reserve(&self, project: u32, count: u32) {
        let mut pools = self.lock();
        let pool = pools.entry(project).or_default();
        pool.reserved = pool.reserved().saturating_add(count);
        pool.reserved_at = Some(Instant::now());
        drop(pools);
        self.shared.changed.notify_all();
    }

    /// Takes a ready worktree for a new thread in `project`, or `None` when the pool has none
    /// (the thread makes its own). While a swarm's reservation is open, waits for one instead.
    pub fn take(&self, project: u32) -> Option<PoolTicket> {
        let deadline = Instant::now() + SWARM_WAIT;
        let mut pools = self.lock();
        loop {
            let pool = pools.get_mut(&project)?;
            let project_root = pool.project.as_ref()?.project_root.clone();
            if let Some(index) = choose_ready(&pool.slots, pool.tip.as_deref()) {
                pool.set_state(index, SlotState::Taken);
                pool.reserved = pool.reserved().saturating_sub(1);
                let ready = ReadyTake {
                    slot: ready_slot_path(&project_root, index),
                    tip: pool.tip.clone(),
                };
                drop(pools);
                self.shared.changed.notify_all();
                return Some(PoolTicket {
                    pools: self.clone(),
                    project,
                    index,
                    ready,
                });
            }
            let now = Instant::now();
            if pool.wanted() == 0 || pool.reserved() == 0 || now >= deadline {
                return None;
            }
            pools = self
                .shared
                .changed
                .wait_timeout(pools, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The project's slots, for Settings → Worktrees.
    pub fn rows(&self, project: u32) -> Vec<PoolRow> {
        let pools = self.lock();
        let Some(pool) = pools.get(&project) else {
            return Vec::new();
        };
        let Some(settings) = &pool.project else {
            return Vec::new();
        };
        pool.slots
            .iter()
            .filter(|slot| slot.state != SlotState::Taken)
            .map(|slot| PoolRow {
                path: ready_slot_path(&settings.project_root, slot.index),
                state: slot.state.clone(),
            })
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<u32, ProjectPool>> {
        // The pool holds plain data; a panic elsewhere leaves it usable.
        self.shared
            .pools
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn idle(&self) {
        let pools = self.lock();
        let _ = self.shared.changed.wait_timeout(pools, IDLE);
    }

    fn checkout_loop(&self, host: &dyn PoolHost) {
        loop {
            self.sync(host);
            let mut worked = false;
            for project in self.unscanned() {
                self.scan(&project);
                worked = true;
            }
            self.fetch_tips();
            while let Some((project, work)) = self.next_checkout() {
                self.run_checkout(host, &project, work);
                host.changed();
                worked = true;
            }
            if worked {
                self.shared.changed.notify_all();
            }
            self.idle();
        }
    }

    fn prebuild_loop(&self, host: &dyn PoolHost) {
        loop {
            match self.next_prebuild() {
                Some((project, index, base)) => {
                    let slot = ready_slot_path(&project.project_root, index);
                    let prebuilt = prebuild_ready_worktree(
                        &project.repository_root,
                        &slot,
                        project.share_build_cache,
                    );
                    if let Err(error) = prebuilt {
                        eprintln!("Velocity warm pool prebuild of {}: {error}", slot.display());
                    }
                    let mut pools = self.lock();
                    if let Some(pool) = pools.get_mut(&project.identifier) {
                        pool.set_state(index, SlotState::Ready { base, built: true });
                    }
                    drop(pools);
                    host.changed();
                    self.shared.changed.notify_all();
                }
                None => self.idle(),
            }
        }
    }

    /// Takes the host's projects: new ones join, settings changes apply, and projects that
    /// left keep their folders untouched (their slots are just no longer handed out).
    fn sync(&self, host: &dyn PoolHost) {
        let projects = host.projects();
        let mut pools = self.lock();
        for pool in pools.values_mut() {
            pool.project = None;
        }
        for (project, preferences) in projects {
            let pool = pools.entry(project.identifier).or_default();
            let moved = pool
                .project
                .as_ref()
                .is_some_and(|known| known.project_root != project.project_root);
            if moved {
                // A new location: slots under the old one are left as they are.
                pool.slots.clear();
                pool.scanned = false;
            }
            pool.project = Some(project);
            pool.preferences = Some(preferences);
        }
    }

    fn unscanned(&self) -> Vec<PoolProject> {
        self.lock()
            .values()
            .filter(|pool| !pool.scanned)
            .filter_map(|pool| pool.project.clone())
            .collect()
    }

    /// Picks up slots left by an earlier run: finished ones rejoin the pool, unfinished ones
    /// (setup interrupted by an exit) are removed and made again.
    fn scan(&self, project: &PoolProject) {
        let found =
            scan_ready_slots(&project.repository_root, &project.project_root).unwrap_or_default();
        let mut slots = Vec::new();
        for FoundSlot { index, path, mark } in found {
            match mark {
                Some(mark) => slots.push(ReadySlot::new(
                    index,
                    SlotState::Ready {
                        base: mark.base,
                        built: mark.built,
                    },
                )),
                None => {
                    if let Err(error) = remove_ready_worktree(&project.repository_root, &path) {
                        eprintln!("Velocity warm pool cleanup of {}: {error}", path.display());
                    }
                }
            }
        }
        if let Some(pool) = self.lock().get_mut(&project.identifier) {
            pool.slots = slots;
            pool.scanned = true;
        }
    }

    /// Fetches the base branch of every pooled project that hasn't been fetched lately.
    fn fetch_tips(&self) {
        let due: Vec<PoolProject> = self
            .lock()
            .values()
            .filter(|pool| pool.wanted() > 0)
            .filter(|pool| pool.fetched.is_none_or(|at| at.elapsed() >= FETCH_EVERY))
            .filter_map(|pool| pool.project.clone())
            .collect();
        for project in due {
            let tip = sweep_base(&project.repository_root, &project.base_branch)
                .map(|(_, commit)| commit);
            if let Some(pool) = self.lock().get_mut(&project.identifier) {
                pool.fetched = Some(Instant::now());
                if tip.is_some() {
                    pool.tip = tip;
                }
            }
        }
    }

    /// The next checkout job anywhere in the pool, with its slot marked as being prepared.
    fn next_checkout(&self) -> Option<(PoolProject, CheckoutWork)> {
        let mut pools = self.lock();
        pools.values_mut().find_map(|pool| {
            if !pool.scanned {
                return None;
            }
            let project = pool.project.clone()?;
            let work = checkout_work(&pool.slots, pool.wanted(), pool.tip.as_deref())?;
            let index = match &work {
                CheckoutWork::Create { index }
                | CheckoutWork::CatchUp { index, .. }
                | CheckoutWork::Remove { index } => *index,
            };
            pool.set_state(index, SlotState::Preparing);
            Some((project, work))
        })
    }

    fn run_checkout(&self, host: &dyn PoolHost, project: &PoolProject, work: CheckoutWork) {
        let preferences = self
            .lock()
            .get(&project.identifier)
            .and_then(|pool| pool.preferences.clone());
        let outcome = match (&work, preferences) {
            (CheckoutWork::Create { index }, Some(preferences)) => {
                let tip = self.tip(project.identifier).unwrap_or_default();
                let request = project.slot_request(*index, &tip, &preferences);
                create_ready_worktree(&request).map(|()| Some(tip))
            }
            (CheckoutWork::CatchUp { index, tip }, _) => {
                let slot = ready_slot_path(&project.project_root, *index);
                catch_up_ready_worktree(&slot, tip).map(|()| Some(tip.clone()))
            }
            (CheckoutWork::Remove { index }, _) => {
                let slot = ready_slot_path(&project.project_root, *index);
                remove_ready_worktree(&project.repository_root, &slot).map(|()| None)
            }
            (CheckoutWork::Create { .. }, None) => Ok(None),
        };
        let at_tip = matches!(outcome, Ok(Some(_)));
        self.settle(project, &work, outcome);
        if let (true, CheckoutWork::Create { index } | CheckoutWork::CatchUp { index, .. }) =
            (at_tip, &work)
        {
            let slot = ready_slot_path(&project.project_root, *index);
            host.slot_at_tip(project.identifier, &slot);
        }
    }

    fn tip(&self, project: u32) -> Option<String> {
        self.lock().get(&project).and_then(|pool| pool.tip.clone())
    }

    /// Records how a checkout job went. A failed slot is dropped from the pool; its folder
    /// is made again (or cleaned up) on the next pass.
    fn settle(
        &self,
        project: &PoolProject,
        work: &CheckoutWork,
        outcome: Result<Option<String>, WorktreeError>,
    ) {
        let mut pools = self.lock();
        let Some(pool) = pools.get_mut(&project.identifier) else {
            return;
        };
        let index = match work {
            CheckoutWork::Create { index }
            | CheckoutWork::CatchUp { index, .. }
            | CheckoutWork::Remove { index } => *index,
        };
        match outcome {
            Ok(Some(base)) => pool.set_state(index, SlotState::Ready { base, built: false }),
            Ok(None) => pool.forget(index),
            Err(error) => {
                eprintln!("Velocity warm pool slot {index}: {error}");
                pool.forget(index);
                // Don't spin on a broken repository: wait for the next fetch.
                pool.fetched = Some(Instant::now());
                pool.tip = None;
            }
        }
    }

    fn next_prebuild(&self) -> Option<(PoolProject, u32, String)> {
        let mut pools = self.lock();
        pools.values_mut().find_map(|pool| {
            // Runs with the Rust build cache off too: Lean setup (toolchain and Mathlib's
            // cache) is part of every prebuild, and only the Cargo build follows the setting.
            let project = pool.project.clone()?;
            let index = prebuild_work(&pool.slots, project.standing)?;
            let base = pool.slots.iter().find_map(|slot| match &slot.state {
                SlotState::Ready { base, .. } if slot.index == index => Some(base.clone()),
                _ => None,
            })?;
            pool.set_state(index, SlotState::Building { base: base.clone() });
            Some((project, index, base))
        })
    }
}
