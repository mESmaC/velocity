//! Admission uses Rosen's machine-wide commit reserve and checks physical headroom too.

use rosen_client::run_queue::RunQueue;
use sysinfo::{MemoryRefreshKind, System};

const GIB: u64 = 1024 * 1024 * 1024;

pub fn is_low() -> bool {
    if RunQueue::machine().is_ok_and(|queue| !queue.memory_headroom().fits(0)) {
        return true;
    }
    let Ok(mut system) = System::new() else {
        return false;
    };
    system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
    let total = system.total_memory();
    total != 0 && system.available_memory() <= (total / 5).min(4 * GIB)
}

pub fn require_headroom() -> Result<(), String> {
    if is_low() {
        return Err("Agent launch paused because machine memory is low".into());
    }
    Ok(())
}
