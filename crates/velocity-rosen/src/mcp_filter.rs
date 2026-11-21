//! Role-based tool filtering and on-demand loader helpers for the `rosen` MCP proxy (#533).

use serde_json::{Value, json};
use velocity_core::tool_policy::{McpToolServer, ThreadRole, ToolAccess, tool_access};

pub const ROLE_FLAG: &str = "--role";
pub const LAZY_FLAG: &str = "--lazy";
pub const LOADER_TOOL: &str = "rosen_tools";

/// Role and lazy-loading policy applied by [`crate::mcp_proxy`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilterPolicy {
    pub role: ThreadRole,
    pub lazy: bool,
}

/// Parses optional leading `--role <role>` and `--lazy` flags from `--rosen-mcp` arguments,
/// returning `(policy, remaining_rosen_args)`. When `--role` is omitted, no filtering is applied.
pub fn parse_policy_args(arguments: &[String]) -> (Option<FilterPolicy>, Vec<String>) {
    let mut role = None;
    let mut lazy = false;
    let mut index = 0;
    while let Some(arg) = arguments.get(index) {
        match arg.as_str() {
            ROLE_FLAG => {
                if let Some(value) = arguments.get(index + 1) {
                    role = ThreadRole::parse(value);
                    index += 2;
                } else {
                    break;
                }
            }
            LAZY_FLAG => {
                lazy = true;
                index += 1;
            }
            "--" => {
                index += 1;
                break;
            }
            _ => break,
        }
    }
    let rest = if index < arguments.len() {
        arguments[index..].to_vec()
    } else {
        vec!["mcp".to_owned()]
    };
    (role.map(|role| FilterPolicy { role, lazy }), rest)
}

/// Advertises `capabilities.tools.listChanged = true` on an `initialize` response when lazy.
pub fn enable_list_changed(response: &mut Value) {
    if let Some(tools) = response
        .get_mut("result")
        .and_then(|result| result.get_mut("capabilities"))
        .and_then(|caps| caps.get_mut("tools"))
        .and_then(Value::as_object_mut)
    {
        tools.insert("listChanged".into(), json!(true));
    }
}

/// Filters an augmented `tools/list` response to tools allowed for `policy.role`, deferring
/// occasional tools behind [`LOADER_TOOL`] when `policy.lazy && !occasional_loaded`.
pub fn filter_tools_list(response: &mut Value, policy: FilterPolicy, occasional_loaded: bool) {
    let Some(tools) = response
        .get_mut("result")
        .and_then(|result| result.get_mut("tools"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    let mut occasional_names = Vec::new();
    let mut listed = Vec::with_capacity(tools.len());
    for tool in tools.drain(..) {
        let Some(name) = tool.get("name").and_then(Value::as_str) else {
            continue;
        };
        let access = tool_access(policy.role, McpToolServer::Rosen, name);
        if access == ToolAccess::Occasional {
            occasional_names.push(name.to_owned());
        }
        if access.is_listed(policy.lazy, occasional_loaded) {
            listed.push(tool);
        }
    }
    if policy.lazy && !occasional_loaded && !occasional_names.is_empty() {
        listed.push(loader_tool_definition(&occasional_names));
    }
    *tools = listed;
}

/// Extracts only the `Occasional` tool schemas for `role` from an augmented `tools/list` response.
pub fn extract_occasional_tools(response: &Value, role: ThreadRole) -> Vec<Value> {
    response
        .get("result")
        .and_then(|result| result.get("tools"))
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter(|tool| {
                    tool.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|name| {
                            tool_access(role, McpToolServer::Rosen, name) == ToolAccess::Occasional
                        })
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

pub fn refusal_text(role: ThreadRole, tool: &str) -> String {
    format!(
        "Tool `rosen.{tool}` is not allowed for `{}` threads.",
        role.as_str()
    )
}

pub fn loader_tool_definition(names: &[String]) -> Value {
    json!({
        "name": LOADER_TOOL,
        "description": format!(
            "Load or call occasional Rosen tools on demand ({}). Call with action=\"list\" to inspect schemas and register them for this session, or action=\"call\" with tool and arguments to run one directly.",
            names.join(", ")
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "call"],
                    "default": "list"
                },
                "tool": {
                    "type": "string",
                    "description": "Occasional Rosen tool name (for action=\"call\")"
                },
                "arguments": {
                    "type": "object",
                    "description": "Arguments for the tool (for action=\"call\")"
                }
            }
        }
    })
}
