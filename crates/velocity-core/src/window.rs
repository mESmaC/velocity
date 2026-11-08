//! Dispatch model for the custom window controls (SPEC.md F1).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowAction {
    Minimize,
    ToggleMaximize,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowOperation {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// Decide once from the native state, including maximize changes made by the OS.
pub fn window_operation(action: WindowAction, maximized: bool) -> WindowOperation {
    match action {
        WindowAction::Minimize => WindowOperation::Minimize,
        WindowAction::ToggleMaximize => {
            if maximized {
                WindowOperation::Restore
            } else {
                WindowOperation::Maximize
            }
        }
        WindowAction::Close => WindowOperation::Close,
    }
}
