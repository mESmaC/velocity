//! Starting an ACP session (SPEC.md §3.2): which MCP servers `session/new` receives, and
//! whether the session needs a `session/set_mode`.
//!
//! AIRC is always passed, with every shared server's name and no name twice, and the start
//! sends the fewest requests that leave the session in its bypass mode.

/// Marks the first server of each name, in order.
///
/// The client lists AIRC first and the shared servers after it (§3.2 Tool access), then
/// keeps only the marked ones. Agents key MCP servers by name, so a later server with the
/// same name would replace or clash with the earlier one; keeping the first means AIRC can
/// never be displaced by a shared server that happens to share its name.
pub fn keep_first_by_name<Name: PartialEq>(names: &[Name]) -> Vec<bool> {
    let mut keep = Vec::new();
    let mut index = 0;
    while index < names.len() {
        keep.push(!appears_before(names, index));
        index += 1;
    }
    keep
}

/// Whether `names[index]` also appears before `index`.
fn appears_before<Name: PartialEq>(names: &[Name], index: usize) -> bool {
    let mut earlier = 0;
    while earlier < index {
        if names[earlier] == names[index] {
            return true;
        }
        earlier += 1;
    }
    false
}

/// The mode to select with `session/set_mode` when the session starts: the agent's bypass
/// mode (`bypass`, from [`crate::choose_session_mode`]), unless the session already starts in
/// it (`current`). Both are indices into the session's available modes.
pub fn mode_to_select(bypass: Option<usize>, current: Option<usize>) -> Option<usize> {
    if needs_no_change(bypass, current) {
        None
    } else {
        bypass
    }
}

/// True when there is no bypass mode, or the session already starts in it.
fn needs_no_change(bypass: Option<usize>, current: Option<usize>) -> bool {
    match bypass {
        Some(mode) => is_mode(current, mode),
        None => true,
    }
}

fn is_mode(current: Option<usize>, mode: usize) -> bool {
    match current {
        Some(current) => current == mode,
        None => false,
    }
}
