//! One-click switch from `review = "before-land"` to `review = "after-land"` (#524).
//!
//! Rosen reads `rosen.toml` from the base, never from a change, so the switch change itself
//! still lands under the current base's `before-land` gates (approval and pre-land receipt).
//! When the project owner asks Velocity to perform the switch from the Rosen status pane,
//! Velocity proposes the one-line `rosen.toml` edit, records the owner's approval on that
//! exact head, runs any configured pre-land check commands on the staged merge result,
//! submits the receipt and lands the change.

use std::path::Path;

use rosen_client::protocol::{ApproveRequest, SubmitReceipt};
use rosen_client::{AgentId, ChangeParent, Client, ObjectId};
use rosen_core::{ProjectConfig, ReviewMode};
use toml_edit::{DocumentMut, Entry, Item, value};

use crate::checker::{self, CHECKER, Priority};
use crate::connection::requests;
use crate::landing::{self, LandOutcome, LandRefusal};
use crate::post_land::is_after_land_switch;
use crate::workspace::session;
use crate::workspace_settings::{
    SETTINGS_AGENT, WorkspaceSettingsError, invalid, read_config, write_config,
};
use crate::{ProjectServer, RosenError};

const SWITCH_MESSAGE: &str = "Switch to after-land review";
const FALLBACK_OWNER: &str = "velocity-owner";
const OUTPUT_TAIL_LINES: usize = 12;

/// What the switch request did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwitchOutcome {
    AlreadyAfterLand,
    Switched { commit: String },
}

#[derive(Debug, thiserror::Error)]
pub enum SwitchError {
    #[error(transparent)]
    Settings(#[from] WorkspaceSettingsError),
    #[error(transparent)]
    Rosen(#[from] RosenError),
    #[error(transparent)]
    Client(#[from] rosen_client::Error),
    #[error("Could not switch to after-land: merge conflict in {0}")]
    Conflict(String),
    #[error("Pre-land check `{command}` failed:\n{output}")]
    PreLandFailed { command: String, output: String },
    #[error("Rosen refused the after-land switch: {0}")]
    Refused(String),
}

/// Proposes, approves, checks and lands the one-line `review = "after-land"` edit in
/// `rosen.toml` on behalf of the project owner (#524).
pub async fn switch_to_after_land(
    server: &ProjectServer,
    check_folder: &Path,
) -> Result<SwitchOutcome, SwitchError> {
    let client = requests(server.endpoint()).await?;
    if client.project_config(None).await?.config.review == ReviewMode::AfterLand {
        return Ok(SwitchOutcome::AlreadyAfterLand);
    }
    let session = session(server, SETTINGS_AGENT).await?;
    let record = session.new_change(None).await?;
    let change = record.change.id.to_string();
    let result = run_switch(
        &client,
        server,
        &change,
        record.change.parent,
        record.change.head,
        check_folder,
    )
    .await;
    match &result {
        Ok(SwitchOutcome::Switched { .. }) => {}
        Ok(SwitchOutcome::AlreadyAfterLand) | Err(_) => {
            let _ = session.abandon(&change).await;
        }
    }
    result
}

async fn run_switch(
    client: &Client,
    server: &ProjectServer,
    change: &str,
    parent: ChangeParent,
    starting_head: ObjectId,
    check_folder: &Path,
) -> Result<SwitchOutcome, SwitchError> {
    let ChangeParent::Base(parent_base) = parent else {
        return Err(SwitchError::Refused(
            "the switch change did not start on the base".into(),
        ));
    };
    let base_record = client.project_config(Some(parent_base)).await?;
    if base_record.config.review == ReviewMode::AfterLand {
        return Ok(SwitchOutcome::AlreadyAfterLand);
    }
    let source = read_config(client, starting_head).await?;
    let Some(updated) = edit_after_land(&source, &base_record.config)? else {
        return Ok(SwitchOutcome::AlreadyAfterLand);
    };
    write_config(client, change, updated, SWITCH_MESSAGE).await?;
    let head = client.change_show(change).await?.change.head;
    client
        .review_approve(ApproveRequest {
            change: change.to_owned(),
            head,
            reviewer: owner_reviewer()?,
        })
        .await?;
    let pre_land = &base_record.config.checks.pre_land;
    let (receipt_head, receipt_base) = if pre_land.commands.is_empty() {
        (head, base_record.commit)
    } else {
        verify_pre_land(server, change, check_folder, &pre_land.commands).await?
    };
    let submitter =
        AgentId::new(CHECKER).map_err(|error| RosenError::AgentName(error.to_string()))?;
    client
        .submit_receipt(SubmitReceipt {
            change: change.to_owned(),
            head: receipt_head,
            base: receipt_base,
            check_set: pre_land.name.clone(),
            submitter,
            signature: None,
        })
        .await?;
    match landing::land(server, SETTINGS_AGENT, change).await? {
        LandOutcome::Landed { commit } => Ok(SwitchOutcome::Switched { commit }),
        LandOutcome::Refused(refusal) => Err(SwitchError::Refused(refusal_text(refusal))),
    }
}

async fn verify_pre_land(
    server: &ProjectServer,
    change: &str,
    check_folder: &Path,
    commands: &[String],
) -> Result<(ObjectId, ObjectId), SwitchError> {
    let checkout = checker::stage_merge_result(server, change, check_folder).await?;
    if !checkout.conflicts.is_empty() {
        return Err(SwitchError::Conflict(checkout.conflicts.join(", ")));
    }
    let folder = check_folder.to_path_buf();
    let commands = commands.to_vec();
    let failure = tokio::task::spawn_blocking(move || {
        checker::apply_staged(&folder)?;
        checker::run_commands(&folder, &commands, Priority::Landing)
    })
    .await
    .map_err(|error| RosenError::StatusData(error.to_string()))??;
    if let Some(failed) = failure {
        return Err(SwitchError::PreLandFailed {
            command: failed.command,
            output: tail_lines(&failed.output, OUTPUT_TAIL_LINES),
        });
    }
    let head = crate::policy::object(&checkout.head)?;
    let base = crate::policy::object(&checkout.base)?;
    Ok((head, base))
}

/// Sets `review = "after-land"` at the top level of `rosen.toml`, keeping comments and
/// every other setting unchanged. Returns `None` when the file already sets it.
pub fn edit_after_land(
    source: &str,
    base_config: &ProjectConfig,
) -> Result<Option<String>, WorkspaceSettingsError> {
    let mut document = source
        .parse::<DocumentMut>()
        .map_err(|error| invalid(error.to_string()))?;
    if document.get("review").and_then(Item::as_str) == Some("after-land") {
        return Ok(None);
    }
    // `Table::insert` reformats an existing key, which drops comments stored in its
    // leading decor. Replace the entry value in place so leading and inline comments survive.
    let mut replacement = value("after-land");
    match document.entry("review") {
        Entry::Occupied(mut entry) => {
            if let (Some(existing), Some(updated)) =
                (entry.get().as_value(), replacement.as_value_mut())
            {
                *updated.decor_mut() = existing.decor().clone();
            }
            entry.insert(replacement);
        }
        Entry::Vacant(entry) => {
            entry.insert(replacement);
        }
    }
    let updated = document.to_string();
    let parsed = ProjectConfig::parse(&updated).map_err(|error| invalid(error.to_string()))?;
    if parsed.review != ReviewMode::AfterLand || !is_after_land_switch(base_config, &updated) {
        return Err(invalid(
            "review = \"after-land\" did not land as a top-level setting",
        ));
    }
    Ok(Some(updated))
}

fn owner_reviewer() -> Result<AgentId, RosenError> {
    for variable in ["USERNAME", "USER"] {
        if let Ok(value) = std::env::var(variable)
            && !value.is_empty()
            && !value.contains('@')
            && let Ok(agent) = AgentId::new(value)
        {
            return Ok(agent);
        }
    }
    AgentId::new(FALLBACK_OWNER).map_err(|error| RosenError::AgentName(error.to_string()))
}

fn refusal_text(refusal: LandRefusal) -> String {
    match refusal {
        LandRefusal::Regenerate { paths } => format!("regenerate {}", paths.join(", ")),
        LandRefusal::Conflict { message, .. } | LandRefusal::Other { message, .. } => message,
    }
}

fn tail_lines(output: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Risk (#524): the advertised one-line switch must accept a rosen.toml that sets only
    // review = "after-land" and reject changes that alter other policy fields at the same time.
    #[test]
    fn edit_after_land_updates_empty_and_existing_configs() {
        let empty = ProjectConfig::default();
        let added = edit_after_land("", &empty).ok().flatten();
        assert!(added.as_deref().is_some_and(|text| {
            is_after_land_switch(&empty, text) && text.contains("review = \"after-land\"")
        }));

        let existing = "# Keep comments\nreview = \"before-land\"\n\n[warm_pool]\nsize = 4\n";
        let base = ProjectConfig::parse(existing).ok();
        let Some(base) = base else {
            panic!("valid base rosen.toml failed to parse");
        };
        let switched = edit_after_land(existing, &base).ok().flatten();
        assert!(switched.as_deref().is_some_and(|text| {
            is_after_land_switch(&base, text)
                && text.contains("# Keep comments")
                && text.contains("size = 4")
        }));
    }

    #[test]
    fn switch_check_rejects_extra_policy_edits() {
        let existing = "review = \"before-land\"\nsensitive = [\"rosen.toml\"]\n";
        let Ok(base) = ProjectConfig::parse(existing) else {
            panic!("valid base rosen.toml failed to parse");
        };
        let sneaky = "review = \"after-land\"\nsensitive = []\n";
        assert!(!is_after_land_switch(&base, sneaky));
        assert!(!is_after_land_switch(&base, existing));
    }
}
