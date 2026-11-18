use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;
use velocity_graph::token_usage::{Call, Counts, Input, timestamp_millis};

use super::process::AgentLaunch;
use super::token_usage_context::{Context, message_source, tool_source, value_size};
use crate::Provider;

pub(super) struct ProviderLog {
    provider: Provider,
    root: PathBuf,
    suffix: String,
    path: Option<PathBuf>,
    offset: u64,
    context: Context,
    tools: HashMap<String, Input>,
    claude_messages: HashSet<String>,
    codex_total: Option<u64>,
    instructions: usize,
}

impl ProviderLog {
    pub(super) fn new(launch: &AgentLaunch, session: &str, instructions: usize) -> Option<Self> {
        // A provider ID is a filename component, never a path into the profile.
        if session.is_empty()
            || !session
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return None;
        }
        let provider = launch.own_config.or(launch.concurrency_provider)?;
        let (variable, default, directory, suffix) = match provider {
            Provider::Codex => (
                "CODEX_HOME",
                ".codex",
                "sessions",
                format!("-{session}.jsonl"),
            ),
            Provider::Anthropic => (
                "CLAUDE_CONFIG_DIR",
                ".claude",
                "projects",
                format!("{session}.jsonl"),
            ),
            _ => return None,
        };
        let profile = launch
            .env
            .iter()
            .rev()
            .find(|(key, _)| key.eq_ignore_ascii_case(variable))
            .map(|(_, value)| PathBuf::from(value))
            .or_else(|| std::env::var_os(variable).map(PathBuf::from))
            .or_else(|| std::env::home_dir().map(|home| home.join(default)))?;
        Some(Self {
            provider,
            root: profile.join(directory),
            suffix,
            path: None,
            offset: 0,
            context: Context::default(),
            tools: HashMap::new(),
            claude_messages: HashSet::new(),
            codex_total: None,
            instructions,
        })
    }

    pub(super) fn read(&mut self, since: i64) -> Vec<Call> {
        let path = self.path.get_or_insert_with(|| {
            find(&self.root, &self.suffix, self.provider).unwrap_or_default()
        });
        let Ok(file) = File::open(&*path) else {
            self.path = None;
            return Vec::new();
        };
        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut calls = Vec::new();
        let mut claude = BTreeMap::new();
        let mut line = Vec::new();
        while let Ok(bytes) = reader.read_until(b'\n', &mut line) {
            if bytes == 0 || line.last() != Some(&b'\n') {
                break;
            }
            self.offset += bytes as u64;
            if let Ok(value) = serde_json::from_slice(&line) {
                match self.provider {
                    Provider::Anthropic => self.claude(&value, since, &mut claude),
                    Provider::Codex => calls.extend(self.codex(&value, since)),
                    _ => {}
                }
            }
            line.clear();
        }
        for (message, call) in claude {
            self.claude_messages.insert(message);
            calls.extend(call);
        }
        calls.sort_by_key(|call| call.timestamp);
        calls
    }

    fn claude(&mut self, line: &Value, since: i64, calls: &mut BTreeMap<String, Option<Call>>) {
        let message = &line["message"];
        match line["type"].as_str() {
            Some("api-request-shape") => self.claude_system(&line["shape"]["system"]),
            Some("attachment") => {
                if let Some(text) = line["rendered"].as_str() {
                    self.context.add(message_source(text), text.len());
                }
            }
            Some("user") => self.claude_content(&message["content"]),
            Some("assistant") => {
                if let Some(id) = message["id"].as_str()
                    && message["model"].as_str() != Some("<synthetic>")
                    && !self.claude_messages.contains(id)
                    && let Some(counts) = token_counts(&message["usage"], Provider::Anthropic)
                {
                    let timestamp = timestamp(line);
                    // Several content blocks may repeat the same response. Later copies
                    // carry the final output count, but the input preceded the first block.
                    let entry = calls.entry(id.to_owned()).or_insert_with(|| {
                        (timestamp >= since).then(|| call(timestamp, counts.clone(), &self.context))
                    });
                    if let Some(call) = entry {
                        call.counts.output = call.counts.output.max(counts.output);
                    }
                }
                self.claude_content(&message["content"]);
            }
            Some("compact_boundary") | Some("system") if line["subtype"] == "compact_boundary" => {
                self.context.clear_history();
                self.tools.clear();
            }
            _ => {}
        }
    }

    fn claude_system(&mut self, system: &Value) {
        let Some(blocks) = system.as_array() else {
            return;
        };
        let mut base = 0;
        let mut instructions = 0;
        for block in blocks {
            if let Some(text) = block["text"].as_str() {
                if message_source(text) == Input::ProjectInstructions {
                    instructions += text.len();
                } else {
                    base += text.len();
                }
            }
        }
        // Some SDK versions merge appended AGENTS.md into the preset block.
        let embedded = self.instructions.saturating_sub(instructions).min(base);
        self.context.replace(Input::BasePrompt, base - embedded);
        self.context
            .replace(Input::ProjectInstructions, instructions + embedded);
    }

    fn claude_content(&mut self, content: &Value) {
        if let Some(text) = content.as_str() {
            self.context.add(message_source(text), text.len());
        }
        for block in content.as_array().into_iter().flatten() {
            let source = match block["type"].as_str() {
                Some("tool_use") => {
                    let source = tool_source(
                        block["name"].as_str().unwrap_or_default(),
                        None,
                        Some(&block["input"]),
                    );
                    if let Some(id) = block["id"].as_str() {
                        self.tools.insert(id.to_owned(), source);
                    }
                    source
                }
                Some("tool_result") => block["tool_use_id"]
                    .as_str()
                    .and_then(|id| self.tools.get(id).copied())
                    .unwrap_or(Input::Unattributed),
                Some("text") => message_source(block["text"].as_str().unwrap_or_default()),
                _ => continue,
            };
            self.context.add(source, value_size(block));
        }
    }

    fn codex(&mut self, line: &Value, since: i64) -> Option<Call> {
        let payload = &line["payload"];
        match line["type"].as_str() {
            Some("session_meta") => {
                let base = &payload["base_instructions"];
                self.context.replace(
                    Input::BasePrompt,
                    value_size(base.get("text").unwrap_or(base)),
                );
            }
            Some("response_item") => self.codex_item(payload),
            Some("compacted") => {
                self.context.clear_history();
                self.tools.clear();
                self.context.add(
                    Input::Conversation,
                    value_size(&payload["replacement_history"]),
                );
            }
            Some("token_usage_record") => {
                return self.codex_call(
                    line,
                    since,
                    &payload["usage"],
                    &payload["thread_token_usage"],
                );
            }
            Some("event_msg") if payload["type"] == "token_count" => {
                let info = &payload["info"];
                return self.codex_call(
                    line,
                    since,
                    &info["last_token_usage"],
                    &info["total_token_usage"],
                );
            }
            _ => {}
        }
        None
    }

    fn codex_call(
        &mut self,
        line: &Value,
        since: i64,
        usage: &Value,
        running: &Value,
    ) -> Option<Call> {
        let counts = token_counts(usage, Provider::Codex)?;
        let total = running["total_tokens"].as_u64()?;
        if self.codex_total == Some(total) {
            return None;
        }
        self.codex_total = Some(total);
        let timestamp = timestamp(line);
        (timestamp >= since).then(|| call(timestamp, counts, &self.context))
    }

    fn codex_item(&mut self, item: &Value) {
        let kind = item["type"].as_str().unwrap_or_default();
        let id = item["call_id"].as_str().unwrap_or_default();
        match kind {
            "function_call" | "custom_tool_call" => {
                let args = item.get("arguments").unwrap_or(&item["input"]);
                let parsed = args
                    .as_str()
                    .and_then(|text| serde_json::from_str(text).ok());
                let name = item["name"].as_str().unwrap_or_default();
                let source = if item["namespace"]
                    .as_str()
                    .is_some_and(|namespace| namespace.starts_with("mcp"))
                {
                    tool_source(
                        &format!("mcp__{name}"),
                        None,
                        Some(parsed.as_ref().unwrap_or(args)),
                    )
                } else {
                    tool_source(name, None, Some(parsed.as_ref().unwrap_or(args)))
                };
                self.tools.insert(id.to_owned(), source);
                self.context.add(source, value_size(args));
            }
            "function_call_output" | "custom_tool_call_output" => {
                let source = self.tools.get(id).copied().unwrap_or(Input::Unattributed);
                self.context.add(source, value_size(&item["output"]));
            }
            "message" => {
                for block in item["content"].as_array().into_iter().flatten() {
                    let text = block["text"].as_str().unwrap_or_default();
                    let source = if item["role"] == "developer" {
                        Input::BasePrompt
                    } else {
                        message_source(text)
                    };
                    self.context.add(source, text.len());
                }
            }
            "additional_tools" => self
                .context
                .add(Input::McpTools, value_size(&item["tools"])),
            _ => {}
        }
    }
}

fn timestamp(line: &Value) -> i64 {
    line["timestamp"]
        .as_str()
        .and_then(timestamp_millis)
        .unwrap_or_default()
}

fn call(timestamp: i64, counts: Counts, context: &Context) -> Call {
    Call {
        timestamp: timestamp / 1000,
        counts: context.attribute(counts),
    }
}

fn token_counts(usage: &Value, provider: Provider) -> Option<Counts> {
    let mut input = usage["input_tokens"].as_u64()?;
    let output = usage["output_tokens"].as_u64()?;
    let cached = match provider {
        Provider::Anthropic => {
            let cached = usage["cache_read_input_tokens"]
                .as_u64()
                .unwrap_or_default();
            input = input.saturating_add(cached).saturating_add(
                usage["cache_creation_input_tokens"]
                    .as_u64()
                    .unwrap_or_default(),
            );
            cached
        }
        _ => usage["cached_input_tokens"]
            .as_u64()
            .unwrap_or_default()
            .min(input),
    };
    (input > 0 || output > 0).then(|| Counts {
        input,
        cached,
        output,
        sources: BTreeMap::new(),
    })
}

fn find(root: &Path, suffix: &str, provider: Provider) -> Option<PathBuf> {
    let depth = if provider == Provider::Codex { 3 } else { 1 };
    matching_file(root, suffix, provider, depth)
}

fn matching_file(root: &Path, suffix: &str, provider: Provider, depth: usize) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let kind = entry.file_type().ok();
        if kind.is_some_and(|kind| kind.is_file()) {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if (provider == Provider::Codex
                && name.starts_with("rollout-")
                && name.ends_with(suffix))
                || (provider == Provider::Anthropic && name == suffix)
            {
                return Some(entry.path());
            }
        } else if depth > 0
            && kind.is_some_and(|kind| kind.is_dir())
            && let Some(path) = matching_file(&entry.path(), suffix, provider, depth - 1)
        {
            return Some(path);
        }
    }
    None
}
