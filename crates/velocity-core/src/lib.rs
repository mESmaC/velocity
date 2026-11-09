//! Pure synchronous Velocity domain logic; native I/O belongs at the edges.

#[cfg(feature = "bindings")]
pub mod ipc_number;
pub mod layout;
pub mod layout_catalog;
pub mod layout_state;
pub mod layout_view;
pub mod navigation;
pub mod state;
pub mod theme;
pub mod thread_view;
pub mod thread_view_model;
pub mod window;
