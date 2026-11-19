//! Harness sources spliced from live Velocity code when the harness binary is built (#499).
//! pr14-deleted-setup and pr4-goal-stack-crash test code that cannot be a dependency: the
//! app's native completion functions and a patched copy of the goal swarm smoke example.
//! Their scripts used to splice it at run time. The guards stay: when the code they need
//! has moved, the harness is built as a stub that fails with the script's old message,
//! so the rest of the suite still builds and runs.

use std::{fs, path::Path};

/// The native items pr14-deleted-setup.rs includes as `native_functions.rs`.
const NATIVE_SIGNATURES: [&str; 5] = [
    "pub(crate) fn lock",
    "struct ThreadFolder",
    "fn folder_ready",
    "fn mark_worktree_ready",
    "fn record_names",
];

const EVENTS_LINE: &str = "    let mut events = EventStream::open(server, 0).await?;";
const HANDOFF_LINE: &str = "    store\n        .lock()\n        .map_err(|e| e.to_string())?\n        .update_thread(1, members[1].thread, |saved| {";
const ABANDON_LINE: &str = "    members[1].workspace = workspace;";
const RECOVERY_ASSERTION: &str = r#"    let directory = original.root.parent().ok_or("missing project folder")?
        .parent().ok_or("missing workspace root")?
        .parent().ok_or("missing fixture root")?.join("state");
    let recovered = StateStore::load_or_default(directory)?;
    let saved = recovered.project(1).ok_or("missing recovered project")?
        .threads.iter().find(|thread| thread.id == members[1].thread)
        .ok_or("missing recovered thread")?;
    println!("after abandon: original_exists={}, recovered_change={:?}, child={}",
        original.root.exists(), saved.rosen_change, workspace.change);
    if saved.rosen_change.as_deref() != Some(workspace.change.as_str()) {
        return Err("FAIL: crash restores abandoned source instead of the live stacked child".into());
    }"#;

/// The native items pr14-deleted-setup runs, taken from the app's threads module.
pub fn native_functions(root: &Path) -> Result<String, String> {
    let path = root.join("src-tauri/src/threads.rs");
    let native = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    let items = NATIVE_SIGNATURES
        .iter()
        .map(|signature| native_item(&native, signature))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(items.join("\n"))
}

/// The one item whose line starts with `signature`, through the first later line that
/// starts with `}`: the script's `(?ms)^<signature>\b.*?^}` match, which had to be unique.
fn native_item(native: &str, signature: &str) -> Result<String, String> {
    let line_starts =
        std::iter::once(0).chain(native.match_indices('\n').map(|(index, _)| index + 1));
    let items: Vec<&str> = line_starts
        .filter_map(|start| {
            let rest = native[start..].strip_prefix(signature)?;
            let at_word_boundary = !rest
                .chars()
                .next()
                .is_some_and(|next| next.is_alphanumeric() || next == '_');
            let closing = rest.find("\n}")?;
            at_word_boundary.then(|| &native[start..start + signature.len() + closing + 2])
        })
        .collect();
    match items.as_slice() {
        [item] => Ok((*item).to_owned()),
        _ => Err(format!("Native completion item changed: {signature}")),
    }
}

/// The goal swarm smoke example, stopped where it abandons the original change and
/// checking that a crash there restores the live stacked child.
pub fn goal_stack_crash(root: &Path) -> Result<String, String> {
    let path = root.join("crates/velocity-threads/examples/rosen_goal_swarm.rs");
    let example = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    let source = example
        .replace(
            EVENTS_LINE,
            &format!("    return Ok(());\n    #[allow(unreachable_code)]\n{EVENTS_LINE}"),
        )
        .replace("\r\n", "\n");
    if !source.contains(HANDOFF_LINE) {
        return Err("Smoke stack handoff moved; update this evidence without weakening it".into());
    }
    let source = source.replace(
        HANDOFF_LINE,
        &format!(
            "    velocity_store::save_pending(store)?;\n    store.lock().map_err(|e| e.to_string())?.defer_saves();\n{HANDOFF_LINE}"
        ),
    );
    if !source.contains(ABANDON_LINE) {
        return Err("Smoke abandonment moved; update this evidence without weakening it".into());
    }
    Ok(source.replace(
        ABANDON_LINE,
        &format!("{RECOVERY_ASSERTION}\n{ABANDON_LINE}"),
    ))
}
