//! A change Rosen could not rebase onto its moved base (R5): what its agent is told. The
//! change stays on its old base with its conflicts stored on the server, and its workspace
//! is left as it is; the agent makes the files merge and resolves them.

use rosen_client::Client;
use rosen_client::protocol::DiffRequest;

use crate::{ProjectServer, RosenError};

/// Characters of the base's diff a message carries. Past it the diff is cut: the agent can
/// read the files and `rosen` history itself.
const DIFF_LIMIT: usize = 20_000;

/// The message for the agent whose `change` conflicts with the moved base in `paths`, with
/// what the base changed in those files since the base the change sits on.
pub async fn conflict_message(
    server: &ProjectServer,
    change: &str,
    paths: &[String],
) -> Result<String, RosenError> {
    let client = Client::connect(server.endpoint()).await?;
    let from = client
        .merge_base(change, "base")
        .await?
        .merge_base
        .map(|commit| commit.to_string());
    let request = DiffRequest {
        from,
        to: Some("base".to_owned()),
        ..DiffRequest::default()
    };
    // Filtered here rather than through the request's `paths`, which at the pinned Rosen
    // revision drops files at the repository root.
    let mut diff = client.diff(request).await?;
    diff.files.retain(|file| paths.contains(&file.path));
    let diff = crate::duty::unified(&diff);
    Ok(message(paths, Some(&diff)))
}

/// The message when the base's diff can't be read: the files alone.
pub fn conflict_message_without_diff(paths: &[String]) -> String {
    message(paths, None)
}

fn message(paths: &[String], diff: Option<&str>) -> String {
    let mut text = format!(
        "Rosen could not merge your change onto the new base: {} conflict with work that landed there. Your change stays on its old base, and your workspace still holds your own version of those files, with no conflict markers.\n",
        paths.join(", ")
    );
    if let Some(diff) = diff.filter(|diff| !diff.is_empty()) {
        text.push_str(&format!(
            "\nThe new base changed them like this:\n\n```diff\n{}```\n",
            cut(diff)
        ));
    }
    text.push_str(
        "\nRosen lands a conflicted file only once it merges cleanly: keep the new base's version of the lines it changed, and redo your own edit around them. Then run `rosen resolve <path>` for each file, and name the work again with `rosen commit -m \"<what changed>\"`.",
    );
    text
}

fn cut(diff: &str) -> String {
    match diff.char_indices().nth(DIFF_LIMIT) {
        Some((end, _)) => format!("{}\n... (cut: the diff goes on)\n", &diff[..end]),
        None => diff.to_owned(),
    }
}
