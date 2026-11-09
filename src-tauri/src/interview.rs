//! Spec interview wiring (SPEC.md F3): hands `velocity-interview` the managed state, the
//! `layout://state` emitter and Tauri's async runtime.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutIntent, LayoutSnapshot};
use velocity_interview::{Host, Interviews};
use velocity_store::StateStore;

use crate::layout::{LayoutStateChanged, MainLayout};
use crate::state::ManagedStateStore;

pub struct AppHost(AppHandle);

impl Host for AppHost {
    fn store(&self) -> &Mutex<StateStore> {
        self.0.state::<ManagedStateStore>().inner()
    }

    fn layout(&self) -> &Mutex<LayoutState> {
        self.0.state::<MainLayout>().inner()
    }

    fn interviews(&self) -> &Interviews {
        self.0.state::<Interviews>().inner()
    }

    fn publish(&self, snapshot: LayoutSnapshot) {
        if let Err(error) = (LayoutStateChanged { state: snapshot }).emit(&self.0) {
            eprintln!("Could not publish main-window state: {error}");
        }
    }

    fn spawn(&self, task: Pin<Box<dyn Future<Output = ()> + Send>>) {
        tauri::async_runtime::spawn(task);
    }
}

fn host(application: &AppHandle) -> Arc<AppHost> {
    Arc::new(AppHost(application.clone()))
}

/// Starts the interviews Create recorded, and resumes ones a restart cut off.
pub fn start(application: &AppHandle) {
    velocity_interview::start_interviews(&host(application));
}

/// Carries out `intent` when it belongs to the open spec interview.
pub fn handle_intent(application: &AppHandle, intent: &LayoutIntent) -> bool {
    velocity_interview::handle_intent(&host(application), intent)
}
