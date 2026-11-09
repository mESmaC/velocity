//! Pure synchronous Velocity domain logic; native I/O belongs at the edges.

pub mod agents_editor;
mod agents_markdown;
pub mod agents_rewrite;
pub mod agents_template;
pub mod build_gate;
pub mod duty;
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
pub mod scout_sweep;
pub mod state;
pub mod theme;
pub mod thread_menu;
pub mod thread_titles;
pub mod thread_view;
pub mod thread_view_model;
pub mod window;
pub mod worktree_settings;
pub mod worktree_view;
