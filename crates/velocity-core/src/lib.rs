//! Pure synchronous Velocity domain logic; native I/O belongs at the edges.

pub mod build_gate;
pub mod interview_view;
#[cfg(feature = "bindings")]
pub mod ipc_number;
pub mod layout;
pub mod layout_catalog;
pub mod layout_guard;
pub mod layout_state;
pub mod layout_view;
pub mod navigation;
pub mod new_project;
pub mod projects;
pub mod state;
pub mod theme;
pub mod thread_view;
pub mod thread_view_model;
pub mod window;
pub mod worktree_settings;
pub mod worktree_view;
