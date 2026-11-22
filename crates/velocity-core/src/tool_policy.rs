//! Pure synchronous per-role MCP tool policy and on-demand loading rules (#533).
//!
//! Each thread role sees only the tools it is permitted to call. Forbidden tools are
//! omitted from `tools/list` and refused on `tools/call`. Occasional tools are loaded on
//! demand when the harness supports it (Claude Code and Antigravity), keeping a builder
//! thread's initial tool list focused around a dozen core tools.

use serde::{Deserialize, Serialize};

/// The operational role of a Velocity agent session (#533).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ThreadRole {
    /// Standard implementation thread (composer, dispatch, swarm, goal swarm, interview).
    #[default]
    Builder,
    /// Post-land review worker reviewing a landed change or no-change issue.
    Reviewer,
    /// Adversarial base scout sweep worker.
    Scout,
    /// Narrow fix-forward thread (build repair, CI repair, revert recovery).
    FollowUp,
    /// Read-only question or feedback thread with no workspace, change, claim or landable status.
    Question,
}

impl ThreadRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builder => "builder",
            Self::Reviewer => "reviewer",
            Self::Scout => "scout",
            Self::FollowUp => "follow_up",
            Self::Question => "question",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "builder" => Some(Self::Builder),
            "reviewer" => Some(Self::Reviewer),
            "scout" => Some(Self::Scout),
            "follow_up" | "follow-up" | "followup" => Some(Self::FollowUp),
            "question" | "feedback" | "read_only" | "read-only" => Some(Self::Question),
            _ => None,
        }
    }

    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::Question)
    }
}

/// Which Velocity-managed MCP server provides a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum McpToolServer {
    Rosen,
    Airc,
    Graph,
    Run,
    Browser,
}

/// Visibility and call policy for one tool under a [`ThreadRole`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolAccess {
    /// Included in the initial `tools/list` on turn one.
    Eager,
    /// Permitted for this role, but deferred behind an on-demand loader when lazy loading is active.
    Occasional,
    /// Never listed and always refused on `tools/call`.
    Forbidden,
}

impl ToolAccess {
    pub const fn is_allowed(self) -> bool {
        !matches!(self, Self::Forbidden)
    }

    pub const fn is_listed(self, lazy: bool, occasional_loaded: bool) -> bool {
        match self {
            Self::Eager => true,
            Self::Occasional => !lazy || occasional_loaded,
            Self::Forbidden => false,
        }
    }
}

/// How `velocity-browser` is attached to a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BrowserAttachment {
    /// All 16 browser tools are exposed eagerly from turn one.
    Eager,
    /// Only the single on-demand `browser_tools` loader is exposed until needed.
    Lazy,
    /// `velocity-browser` is not attached at all.
    Omitted,
}

/// Whether `role` runs queued background builds/checks (`velocity-run`).
pub const fn allows_queued_runs(role: ThreadRole) -> bool {
    !matches!(role, ThreadRole::Question)
}

/// Whether `role` uses `velocity-browser` eagerly, lazily on demand, or not at all.
pub const fn browser_attachment(role: ThreadRole, touches_ui: bool) -> BrowserAttachment {
    match role {
        ThreadRole::Builder => {
            if touches_ui {
                BrowserAttachment::Eager
            } else {
                BrowserAttachment::Lazy
            }
        }
        ThreadRole::FollowUp => {
            if touches_ui {
                BrowserAttachment::Lazy
            } else {
                BrowserAttachment::Omitted
            }
        }
        ThreadRole::Reviewer | ThreadRole::Scout | ThreadRole::Question => {
            BrowserAttachment::Omitted
        }
    }
}

/// Heuristic for whether a task's title or opening prompt touches UI or browser pages.
pub fn prompt_touches_ui(title: &str, prompt: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "ui/",
        "ui\\",
        ".svelte",
        ".css",
        ".html",
        "svelte",
        "browser",
        "frontend",
        "webview",
        "sidebar",
        "composer",
        "modal",
        "dialog",
        "toast",
        "tooltip",
        "button",
        "theme",
        "tokens.css",
        "reference.css",
        "main-window.html",
        "visual",
        "screenshot",
        "css",
        "styling",
        "style",
        "viewport",
        "scrollbar",
        "tab bar",
        "layout",
        "click",
        "hover",
        "render",
        "dom",
    ];
    let combined = format!(
        "{}\n{}",
        title.to_ascii_lowercase(),
        prompt.to_ascii_lowercase()
    );
    KEYWORDS.iter().any(|keyword| combined.contains(keyword))
}

/// Tool visibility and permission for `(role, server, tool)`.
pub fn tool_access(role: ThreadRole, server: McpToolServer, tool: &str) -> ToolAccess {
    match server {
        McpToolServer::Rosen => rosen_access(role, tool),
        McpToolServer::Airc => airc_access(role, tool),
        McpToolServer::Graph => graph_access(role, tool),
        McpToolServer::Run => run_access(role, tool),
        McpToolServer::Browser => browser_access(role, tool),
    }
}

fn rosen_access(role: ThreadRole, tool: &str) -> ToolAccess {
    match role {
        ThreadRole::Builder => match tool {
            "status" | "diff" | "commit" | "claim" | "rebase" | "resolve" => ToolAccess::Eager,
            "share" | "release" | "overlaps" | "wait" | "log" | "change_show" | "config_show"
            | "review_list" | "review_comment" | "review_resolve" => ToolAccess::Occasional,
            _ => ToolAccess::Forbidden,
        },
        ThreadRole::Reviewer => match tool {
            "status" | "diff" | "log" | "commit" | "claim" | "rebase" | "resolve"
            | "review_list" | "review_comment" | "review_resolve" | "review_failures"
            | "review_status" => ToolAccess::Eager,
            "share" | "release" | "overlaps" | "wait" | "change_show" | "change_list"
            | "review_landed" | "config_show" | "workspace_list" => ToolAccess::Occasional,
            _ => ToolAccess::Forbidden,
        },
        ThreadRole::Scout => match tool {
            "status" | "diff" | "log" | "commit" | "claim" | "review_failures"
            | "review_status" => ToolAccess::Eager,
            "share" | "release" | "overlaps" | "rebase" | "resolve" | "change_show"
            | "change_list" | "review_landed" | "config_show" => ToolAccess::Occasional,
            _ => ToolAccess::Forbidden,
        },
        ThreadRole::FollowUp => match tool {
            "status" | "diff" | "commit" | "claim" | "rebase" | "resolve" | "review_failures"
            | "review_status" => ToolAccess::Eager,
            "share" | "release" | "overlaps" | "wait" | "log" | "change_show" | "config_show" => {
                ToolAccess::Occasional
            }
            _ => ToolAccess::Forbidden,
        },
        ThreadRole::Question => match tool {
            "log" | "change_list" | "review_status" | "config_show" => ToolAccess::Eager,
            "change_show" | "review_failures" => ToolAccess::Occasional,
            _ => ToolAccess::Forbidden,
        },
    }
}

fn airc_access(role: ThreadRole, tool: &str) -> ToolAccess {
    match role {
        ThreadRole::Builder => match tool {
            "airc_claims"
            | "airc_claim"
            | "issue_comment"
            | "issue_ready_to_close"
            | "read_ledger"
            | "append_ledger" => ToolAccess::Eager,
            "airc_share"
            | "airc_release"
            | "airc_wait"
            | "airc_rebase"
            | "read_feed"
            | "read_events"
            | "read_messages"
            | "send_message"
            | "list_nicks"
            | "task_list"
            | "task_add"
            | "task_update"
            | "issue_read"
            | "issue_create"
            | "issue_blocked_upstream"
            | "issue_hold"
            | "issue_close_not_planned"
            | "next_work" => ToolAccess::Occasional,
            // Explicitly forbidden: join, leave, task_delete and any unknown tool.
            _ => ToolAccess::Forbidden,
        },
        ThreadRole::Reviewer => match tool {
            "airc_claims" | "airc_claim" | "read_ledger" | "append_ledger" | "issue_read" => {
                ToolAccess::Eager
            }
            "airc_share" | "airc_release" | "airc_wait" | "read_feed" | "read_events"
            | "read_messages" | "send_message" | "list_nicks" | "task_list" | "issue_create"
            | "issue_comment" => ToolAccess::Occasional,
            _ => ToolAccess::Forbidden,
        },
        ThreadRole::Scout => match tool {
            "airc_claims" | "read_ledger" | "append_ledger" | "read_feed" | "issue_read" => {
                ToolAccess::Eager
            }
            "airc_claim" | "airc_share" | "airc_release" | "read_events" | "read_messages"
            | "send_message" | "list_nicks" | "task_list" | "issue_create" => {
                ToolAccess::Occasional
            }
            _ => ToolAccess::Forbidden,
        },
        ThreadRole::FollowUp => match tool {
            "airc_claims" | "airc_claim" | "read_ledger" | "append_ledger" => ToolAccess::Eager,
            "airc_share" | "airc_release" | "airc_wait" | "airc_rebase" | "read_feed"
            | "read_events" | "read_messages" | "send_message" | "list_nicks" | "issue_read"
            | "issue_comment" => ToolAccess::Occasional,
            _ => ToolAccess::Forbidden,
        },
        ThreadRole::Question => match tool {
            "read_feed" | "read_ledger" | "airc_claims" | "task_list" | "list_nicks"
            | "issue_read" => ToolAccess::Eager,
            "read_events" | "read_messages" => ToolAccess::Occasional,
            _ => ToolAccess::Forbidden,
        },
    }
}

fn graph_access(role: ThreadRole, tool: &str) -> ToolAccess {
    match role {
        ThreadRole::Question => match tool {
            "graph_find" | "graph_read" | "graph_impact" | "graph_spec" | "project_read" => {
                ToolAccess::Eager
            }
            "graph_grep" | "graph_glob" | "graph_run" => ToolAccess::Occasional,
            _ => ToolAccess::Forbidden,
        },
        ThreadRole::Builder | ThreadRole::Reviewer | ThreadRole::Scout | ThreadRole::FollowUp => {
            match tool {
                "graph_find" | "graph_read" | "graph_impact" | "graph_overlaps"
                | "project_read" => ToolAccess::Eager,
                "graph_spec" | "graph_grep" | "graph_glob" | "graph_run" => ToolAccess::Occasional,
                _ => ToolAccess::Forbidden,
            }
        }
    }
}

fn run_access(role: ThreadRole, tool: &str) -> ToolAccess {
    match role {
        ThreadRole::Builder | ThreadRole::FollowUp => match tool {
            "preflight" | "queue_run" => ToolAccess::Eager,
            "run_status" | "run_cancel" | "workflow_run" => ToolAccess::Occasional,
            _ => ToolAccess::Forbidden,
        },
        ThreadRole::Reviewer | ThreadRole::Scout => match tool {
            "queue_run" | "preflight" => ToolAccess::Eager,
            "run_status" | "run_cancel" => ToolAccess::Occasional,
            _ => ToolAccess::Forbidden,
        },
        ThreadRole::Question => ToolAccess::Forbidden,
    }
}

fn browser_access(role: ThreadRole, _tool: &str) -> ToolAccess {
    match role {
        ThreadRole::Builder | ThreadRole::FollowUp => ToolAccess::Eager,
        ThreadRole::Reviewer | ThreadRole::Scout | ThreadRole::Question => ToolAccess::Forbidden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_excludes_forbidden_destructive_and_irrelevant_tools() {
        // Risk (#533): a builder thread receiving destructive or irrelevant tools wastes context
        // and leaves AGENTS.md-forbidden operations one mistaken call away.
        for forbidden in [
            "workspace_delete",
            "change_abandon",
            "change_recover",
            "review_mark",
            "review_landed",
            "change_new",
            "workspace_limits",
            "server_status",
        ] {
            assert_eq!(
                tool_access(ThreadRole::Builder, McpToolServer::Rosen, forbidden),
                ToolAccess::Forbidden,
                "Builder must not be allowed rosen.{forbidden}"
            );
        }
        for forbidden in ["join", "leave", "task_delete"] {
            assert_eq!(
                tool_access(ThreadRole::Builder, McpToolServer::Airc, forbidden),
                ToolAccess::Forbidden,
                "Builder must not be allowed airc.{forbidden}"
            );
        }
        assert_eq!(
            tool_access(ThreadRole::Builder, McpToolServer::Rosen, "status"),
            ToolAccess::Eager
        );
        assert_eq!(
            tool_access(ThreadRole::Builder, McpToolServer::Rosen, "commit"),
            ToolAccess::Eager
        );
        assert_eq!(
            tool_access(ThreadRole::Builder, McpToolServer::Rosen, "review_list"),
            ToolAccess::Occasional
        );
    }

    #[test]
    fn question_role_is_strictly_read_only() {
        // Risk (#533): a question thread runs without a workspace, change or claim and must not
        // expose mutation tools.
        for write_tool in [
            "commit",
            "claim",
            "share",
            "release",
            "rebase",
            "resolve",
            "change_new",
            "change_abandon",
            "workspace_delete",
            "review_mark",
        ] {
            assert_eq!(
                tool_access(ThreadRole::Question, McpToolServer::Rosen, write_tool),
                ToolAccess::Forbidden
            );
        }
        for write_tool in [
            "airc_claim",
            "airc_release",
            "airc_share",
            "airc_wait",
            "append_ledger",
            "send_message",
            "task_add",
            "task_update",
            "task_delete",
            "issue_create",
            "issue_comment",
            "issue_ready_to_close",
            "next_work",
        ] {
            assert_eq!(
                tool_access(ThreadRole::Question, McpToolServer::Airc, write_tool),
                ToolAccess::Forbidden
            );
        }
        assert!(!allows_queued_runs(ThreadRole::Question));
        assert_eq!(
            browser_attachment(ThreadRole::Question, true),
            BrowserAttachment::Omitted
        );
        assert_eq!(
            tool_access(ThreadRole::Question, McpToolServer::Graph, "graph_find"),
            ToolAccess::Eager
        );
        assert_eq!(
            tool_access(ThreadRole::Question, McpToolServer::Graph, "project_read"),
            ToolAccess::Eager
        );
        assert_eq!(
            tool_access(ThreadRole::Builder, McpToolServer::Graph, "project_read"),
            ToolAccess::Eager
        );
        assert_eq!(
            tool_access(ThreadRole::Question, McpToolServer::Airc, "read_feed"),
            ToolAccess::Eager
        );
    }

    #[test]
    fn browser_is_lazy_on_non_ui_builder_work_and_eager_on_ui_work() {
        assert!(!prompt_touches_ui(
            "Fix claim lock race",
            "Update crates/velocity-airc/src/claims.rs"
        ));
        assert!(prompt_touches_ui(
            "Fix composer pill spacing",
            "Update ui/src/lib/Composer.svelte"
        ));
        assert_eq!(
            browser_attachment(ThreadRole::Builder, false),
            BrowserAttachment::Lazy
        );
        assert_eq!(
            browser_attachment(ThreadRole::Builder, true),
            BrowserAttachment::Eager
        );
        assert_eq!(
            browser_attachment(ThreadRole::Reviewer, true),
            BrowserAttachment::Omitted
        );
    }
}
