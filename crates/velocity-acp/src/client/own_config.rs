//! The MCP server names already in an agent's own config (§3.2 Tool access: "When a name is
//! also in an agent's own config, the agent's own entry wins").
//!
//! Agents merge the servers a client passes in `session/new` over the ones from their config,
//! so a shared server with the same name would replace yours. Velocity reads the names from
//! the files each built-in agent loads and leaves those servers out of `session/new`. A file
//! that is missing or unreadable adds no names: the shared server is passed as usual.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use agent_client_protocol::v1::schema::McpServer;

use super::session::server_name;
use crate::Provider;

/// How a config file holds its servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfigShape {
    /// JSON with an `mcpServers` object (Claude's `.mcp.json`, Gemini, Antigravity, Kimi).
    JsonServers,
    /// Claude's `~/.claude.json`: `mcpServers`, plus `projects.<folder>.mcpServers` for the
    /// folder the agent runs in.
    ClaudeUser,
    /// TOML with an `mcp_servers` table (Codex).
    TomlServers,
}

/// The shared servers, minus every one whose name the agent's own config already has.
pub(crate) fn without_own_servers(
    shared: &[McpServer],
    provider: Option<Provider>,
    worktree: &Path,
) -> Vec<McpServer> {
    let own = provider
        .map(|provider| own_server_names(provider, worktree))
        .unwrap_or_default();
    shared
        .iter()
        .filter(|server| server_name(server).is_none_or(|name| !own.contains(&lowercase(name))))
        .cloned()
        .collect()
}

/// Names lowercased: agents on Windows treat config keys loosely, so a near match counts.
fn own_server_names(provider: Provider, worktree: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for (path, shape) in config_files(provider, worktree, home().as_deref()) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            names.extend(
                server_names(&text, shape, worktree)
                    .iter()
                    .map(|n| lowercase(n)),
            );
        }
    }
    names
}

fn lowercase(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn home() -> Option<PathBuf> {
    ["USERPROFILE", "HOME"]
        .into_iter()
        .find_map(|variable| std::env::var_os(variable).filter(|value| !value.is_empty()))
        .map(PathBuf::from)
}

fn environment_folder(variable: &str) -> Option<PathBuf> {
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The config files `provider`'s agent reads MCP servers from, user files first.
fn config_files(
    provider: Provider,
    worktree: &Path,
    home: Option<&Path>,
) -> Vec<(PathBuf, ConfigShape)> {
    let mut files = Vec::new();
    let mut user = |folder: Option<PathBuf>, file: &str, shape| {
        if let Some(folder) = folder {
            files.push((folder.join(file), shape));
        }
    };
    let home_folder = |child: &str| home.map(|home| home.join(child));
    match provider {
        Provider::Anthropic => {
            let folder = environment_folder("CLAUDE_CONFIG_DIR").or(home.map(Path::to_path_buf));
            user(folder, ".claude.json", ConfigShape::ClaudeUser);
            files.push((worktree.join(".mcp.json"), ConfigShape::JsonServers));
        }
        Provider::Codex => {
            let folder = environment_folder("CODEX_HOME").or(home_folder(".codex"));
            user(folder, "config.toml", ConfigShape::TomlServers);
            files.push((
                worktree.join(".codex/config.toml"),
                ConfigShape::TomlServers,
            ));
        }
        Provider::Antigravity => {
            let folder = home_folder(".gemini");
            user(folder.clone(), "settings.json", ConfigShape::JsonServers);
            user(
                folder,
                "antigravity/mcp_config.json",
                ConfigShape::JsonServers,
            );
            files.push((
                worktree.join(".gemini/settings.json"),
                ConfigShape::JsonServers,
            ));
        }
        // Kimi Code keeps its servers in `~/.kimi-code/`; the older Kimi CLI in `~/.kimi/`.
        Provider::KimiCode => {
            user(
                home_folder(".kimi-code"),
                "mcp.json",
                ConfigShape::JsonServers,
            );
            user(home_folder(".kimi"), "mcp.json", ConfigShape::JsonServers);
        }
        Provider::Custom(_) => {}
    }
    files
}

/// The server names one config file holds. Text that doesn't parse holds none.
fn server_names(text: &str, shape: ConfigShape, worktree: &Path) -> Vec<String> {
    match shape {
        ConfigShape::TomlServers => toml::from_str::<toml::Table>(text)
            .ok()
            .and_then(|table| table.get("mcp_servers")?.as_table().cloned())
            .map(|servers| servers.keys().cloned().collect())
            .unwrap_or_default(),
        ConfigShape::JsonServers | ConfigShape::ClaudeUser => {
            let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
                return Vec::new();
            };
            let mut names = json_keys(json.get("mcpServers"));
            if shape == ConfigShape::ClaudeUser {
                let projects = json.get("projects").and_then(serde_json::Value::as_object);
                let entry = projects
                    .into_iter()
                    .flatten()
                    .find(|(folder, _)| same_folder(folder, worktree));
                names.extend(json_keys(
                    entry.and_then(|(_, project)| project.get("mcpServers")),
                ));
            }
            names
        }
    }
}

fn json_keys(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_object)
        .map(|servers| servers.keys().cloned().collect())
        .unwrap_or_default()
}

/// Claude keys projects by folder, with forward slashes on Windows (`C:/Users/me/repo`).
fn same_folder(key: &str, worktree: &Path) -> bool {
    let normalize = |text: &str| {
        text.replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    };
    normalize(key) == normalize(&worktree.to_string_lossy())
}
