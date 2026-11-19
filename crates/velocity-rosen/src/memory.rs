//! Admission uses Rosen's machine-wide commit reserve and checks physical headroom too.

use rosen_client::run_queue::RunQueue;
use sysinfo::{MemoryRefreshKind, System};

const GIB: u64 = 1024 * 1024 * 1024;

pub fn is_low() -> bool {
    !has_headroom(0)
}

pub fn has_headroom(reserve: u64) -> bool {
    if RunQueue::machine().is_ok_and(|queue| !queue.memory_headroom().fits(reserve)) {
        return false;
    }
    let Ok(mut system) = System::new() else {
        // Existing callers check for known pressure. A new reservation needs a reading.
        return reserve == 0;
    };
    system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
    let total = system.total_memory();
    if total == 0 {
        return reserve == 0;
    }
    system.available_memory() > (total / 5).min(4 * GIB).saturating_add(reserve)
}

pub fn require_headroom() -> Result<(), String> {
    if is_low() {
        return Err("Agent launch paused because machine memory is low".into());
    }
    Ok(())
}
