//! REA's MCP server (SPEC.md F18 "Pack specifics"): turning REA on adds its server to the
//! shared MCP servers every agent of every provider gets in `session/new` (§3.2 Tool access).
//! Velocity starts the server from the pinned clone itself rather than running
//! `rea-agents setup`, so nothing edits an agent's own config.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::SkillLibraryError;
use crate::library::InstalledSkillPack;

pub const REA_PACK: &str = "rea";
/// The server name REA's own client declarations use, so an agent that already has REA in
/// its config keeps its own entry (§3.2: the agent's own entry wins).
pub const REA_SERVER_NAME: &str = "rea";
const REA_PACKAGE: &str = "rea-agents";

/// A stdio MCP server a skill pack contributes to the shared MCP servers.
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillMcpServer {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub arguments: Vec<String>,
}

/// The command that starts REA's MCP server for an installed REA pack.
///
/// A clone with a built runtime (`dist/mcp/main.js`) runs its own `scripts/rea.mjs mcp` under
/// Node. A source clone has no build output, so the server runs the published `rea-agents`
/// package at exactly the version the pinned commit's `package.json` declares; the pin still
/// decides which REA every agent gets.
pub fn rea_mcp_server(pack: &InstalledSkillPack) -> Result<SkillMcpServer, SkillLibraryError> {
    let clone = PathBuf::from(&pack.clone_path);
    let launcher = clone.join("scripts").join("rea.mjs");
    if clone.join("dist").join("mcp").join("main.js").is_file() && launcher.is_file() {
        return Ok(SkillMcpServer {
            name: REA_SERVER_NAME.to_owned(),
            command: String::from("node"),
            arguments: vec![plain_path(&launcher), String::from("mcp")],
        });
    }
    let version = pinned_version(&clone)?;
    Ok(npx_server(&format!("{REA_PACKAGE}@{version}")))
}

// Agents start stdio servers with a plain process spawn, which on Windows finds `npx.cmd`
// only through the shell.
fn npx_server(package: &str) -> SkillMcpServer {
    let mut arguments = Vec::new();
    let command = if cfg!(windows) {
        arguments.extend([String::from("/c"), String::from("npx")]);
        String::from("cmd")
    } else {
        String::from("npx")
    };
    arguments.extend([String::from("-y"), package.to_owned(), String::from("mcp")]);
    SkillMcpServer {
        name: REA_SERVER_NAME.to_owned(),
        command,
        arguments,
    }
}

fn pinned_version(clone: &Path) -> Result<String, SkillLibraryError> {
    let path = clone.join("package.json");
    let text = fs::read_to_string(&path).map_err(|source| SkillLibraryError::Io {
        path: path.clone(),
        source,
    })?;
    let manifest: serde_json::Value = serde_json::from_str(&text)?;
    let version = manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .filter(|version| is_exact_version(version))
        .ok_or_else(|| SkillLibraryError::MissingPackFile {
            pack: REA_PACK.to_owned(),
            file: String::from("package.json version"),
        })?;
    Ok(version.to_owned())
}

// Only an exact `x.y.z[-pre]` version: anything else would let npx pick a newer release than
// the pin, or smuggle extra arguments onto the command line.
fn is_exact_version(version: &str) -> bool {
    let (core, prerelease) = version.split_once('-').unwrap_or((version, ""));
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
        })
        && prerelease.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '.' || character == '-'
        })
}

fn plain_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
}
