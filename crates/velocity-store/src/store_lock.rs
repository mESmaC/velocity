//! Instrumented mutex wrapper around [`StateStore`] that records lock wait and hold times
//! per caller without performing telemetry or logging while holding the lock (#557).

use std::ops::{Deref, DerefMut};
use std::panic::Location;
use std::sync::{LockResult, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::StateStore;

/// Persisted state mutex that records wait and hold durations for every [`Self::lock`] call.
pub struct StoreLock(Mutex<StateStore>);

impl StoreLock {
    pub fn new(store: StateStore) -> Self {
        Self(Mutex::new(store))
    }

    /// Acquires the store mutex, measuring lock wait time and reporting hold time on drop.
    #[track_caller]
    pub fn lock(&self) -> LockResult<StoreGuard<'_>> {
        self.lock_from(Location::caller())
    }

    /// Acquires the store mutex attributing wait and hold telemetry to `caller`, so async
    /// callers hopping through `spawn_blocking` keep their own source location (#554).
    pub fn lock_from(&self, caller: &'static Location<'static>) -> LockResult<StoreGuard<'_>> {
        let started = Instant::now();
        match self.0.lock() {
            Ok(guard) => Ok(StoreGuard::new(guard, caller, started.elapsed())),
            Err(poisoned) => Err(PoisonError::new(StoreGuard::new(
                poisoned.into_inner(),
                caller,
                started.elapsed(),
            ))),
        }
    }

    /// Exposes the underlying mutex for subsystems that measure their own lock spans (such as
    /// background saves in `save_behind`) or expect `&Mutex<StateStore>`.
    pub fn as_mutex(&self) -> &Mutex<StateStore> {
        &self.0
    }
}

impl Deref for StoreLock {
    type Target = Mutex<StateStore>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Guard returned by [`StoreLock::lock`].
///
/// Field declaration order is significant: `guard` drops first so the store mutex is released
/// before `meter` records the hold duration or logs a budget breach.
pub struct StoreGuard<'a> {
    guard: MutexGuard<'a, StateStore>,
    _meter: HoldMeter,
}

impl<'a> StoreGuard<'a> {
    fn new(
        guard: MutexGuard<'a, StateStore>,
        caller: &'static Location<'static>,
        wait: Duration,
    ) -> Self {
        Self {
            guard,
            _meter: HoldMeter {
                caller,
                wait,
                acquired_at: Instant::now(),
            },
        }
    }
}

impl Deref for StoreGuard<'_> {
    type Target = StateStore;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl DerefMut for StoreGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

struct HoldMeter {
    caller: &'static Location<'static>,
    wait: Duration,
    acquired_at: Instant,
}

impl Drop for HoldMeter {
    fn drop(&mut self) {
        velocity_core::performance::record_store_lock(
            self.caller,
            self.wait,
            self.acquired_at.elapsed(),
        );
    }
}
