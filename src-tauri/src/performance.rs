//! Tauri IPC for runtime performance diagnostics and budget breaches (#557).

use velocity_core::performance::{self, PerformanceDiagnostics};

/// Returns the rolling performance measurements, active budgets and recent budget breaches.
/// Runs directly without taking the persisted store lock or waiting behind the UI command
/// thread so diagnostics stay responsive even under heavy contention.
#[tauri::command]
#[specta::specta]
pub fn get_performance_diagnostics() -> PerformanceDiagnostics {
    performance::snapshot()
}
