//! Signature and IPC contract impact analysis (#542): warns when a change alters a public
//! symbol's signature, an IPC command, or an IPC event/payload type that another open or
//! recently landed change references, and surfaces the same warnings at dispatch.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use rosen_client::Client;
use rosen_client::protocol::{
    Diff, DiffRequest, FileDiff, LineKind, LogRequest, ReadAnswer, ReadRequest,
};
use serde::{Deserialize, Serialize};

use crate::SymbolKind;
use crate::extract::{Definition, FileParse, extract};
use crate::graph::{Graph, RecordKind, Relation};
use crate::language::Language;
use crate::query::impact_records;

const MAX_DIFF_FILES: usize = 16;
const MAX_PEER_DIFF_FILES: usize = 10;
const MAX_OPEN_PEERS: usize = 12;
const MAX_RECENT_LANDED: u32 = 8;
const MAX_ALTERED_PER_FILE: usize = 24;
const MAX_WARNINGS: usize = 16;
const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_SIGNATURE_BYTES: usize = 2048;

/// Category of altered API surface detected in a code diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureChangeKind {
    PublicSignature,
    IpcCommand,
    IpcEvent,
    IpcPayload,
}

impl SignatureChangeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::PublicSignature => "public signature",
            Self::IpcCommand => "IPC command",
            Self::IpcEvent => "IPC event",
            Self::IpcPayload => "IPC payload",
        }
    }
}

/// Lifecycle status of the peer change named in a warning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeStatus {
    Open,
    Ready,
    Landed,
}

impl ChangeStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Ready => "ready",
            Self::Landed => "recently landed",
        }
    }
}

/// Which side altered the signature and which side references it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceDirection {
    /// This change altered the symbol; the peer change references it.
    CurrentAlteredPeerReferences,
    /// The peer change altered the symbol; this change references it.
    PeerAlteredCurrentReferences,
    /// The peer change altered the symbol; the issue/task being dispatched touches it.
    PeerAlteredWorkTouches,
}

/// One altered public signature, IPC command, or IPC event/payload type in a file diff.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlteredSignature {
    pub path: String,
    pub symbol: String,
    pub name: String,
    pub kind: SignatureChangeKind,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_signature: Option<String>,
}

/// Advisory warning about a potentially stale reference across parallel or recent changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleReferenceWarning {
    pub symbol: String,
    pub kind: SignatureChangeKind,
    pub defined_in: String,
    pub detail: String,
    pub direction: ReferenceDirection,
    pub peer_change: String,
    pub peer_owner: String,
    pub peer_status: ChangeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_commit: Option<String>,
    pub referencing_paths: Vec<String>,
}

/// Lightweight peer metadata shared by change checks and dispatch checks.
#[derive(Clone, Debug)]
pub struct PeerSummary {
    pub change: String,
    pub owner: String,
    pub status: ChangeStatus,
    pub commit: Option<String>,
}

struct PeerChange {
    summary: PeerSummary,
    diff: Diff,
    claimed_paths: Vec<String>,
}

/// Detects altered public symbol signatures, IPC commands, and IPC event/payload types
/// between `before` and `after` versions of `path`. Pure body-only edits return empty.
pub fn altered_signatures(
    path: &str,
    before: Option<&str>,
    after: Option<&str>,
) -> Vec<AlteredSignature> {
    let Some(language) = Language::of(path).filter(|lang| *lang != Language::Markdown) else {
        return Vec::new();
    };
    let Some(before_text) = before.filter(|text| text.len() <= MAX_SOURCE_BYTES) else {
        return Vec::new();
    };
    let after_text = after.filter(|text| text.len() <= MAX_SOURCE_BYTES);
    let old_parse = extract(language, before_text);
    let new_parse = after_text.map(|text| extract(language, text));

    let old_map = index_top_definitions(language, before_text, &old_parse);
    let new_map = new_parse
        .as_ref()
        .zip(after_text)
        .map(|(parse, text)| index_top_definitions(language, text, parse))
        .unwrap_or_default();

    let mut altered = BTreeMap::<String, AlteredSignature>::new();
    collect_top_signature_changes(path, &old_map, &new_map, &mut altered);
    if let (Some(new_parse), Some(after_text)) = (new_parse.as_ref(), after_text) {
        collect_container_shape_changes(
            path,
            language,
            (before_text, &old_parse, &old_map),
            (after_text, new_parse, &new_map),
            &mut altered,
        );
    }
    altered.into_values().take(MAX_ALTERED_PER_FILE).collect()
}

struct IndexedDef {
    symbol: String,
    name: String,
    kind: SymbolKind,
    public_kind: Option<SignatureChangeKind>,
    command: bool,
    display_signature: String,
    full_signature: String,
    ipc_or_serde: bool,
}

type DefKey = (String, &'static str);

fn index_top_definitions(
    language: Language,
    source: &str,
    parse: &FileParse,
) -> BTreeMap<DefKey, IndexedDef> {
    let public_traits = public_trait_names(language, source, parse);
    let mut map = BTreeMap::new();
    for def in &parse.definitions {
        if def.test
            || def.kind.is_doc()
            || matches!(def.kind, SymbolKind::Field | SymbolKind::Variant)
        {
            continue;
        }
        let symbol = qualified_name(def, language);
        let public_kind = classify_definition(language, def, source, &public_traits);
        let ipc_or_serde = language == Language::Rust
            && matches!(def.kind, SymbolKind::Class)
            && rust_has_serde_or_ipc(source, def.start_line);
        map.insert(
            (symbol.clone(), def.kind.label()),
            IndexedDef {
                symbol,
                name: def.name.clone(),
                kind: def.kind,
                public_kind,
                command: def.command,
                display_signature: def.signature.clone(),
                full_signature: collapsed_full_signature(
                    source,
                    def.start_byte as usize,
                    def.end_byte as usize,
                ),
                ipc_or_serde,
            },
        );
    }
    map
}

fn collect_top_signature_changes(
    path: &str,
    old_map: &BTreeMap<DefKey, IndexedDef>,
    new_map: &BTreeMap<DefKey, IndexedDef>,
    out: &mut BTreeMap<String, AlteredSignature>,
) {
    for (key, old) in old_map {
        let Some(old_kind) = old.public_kind else {
            continue;
        };
        match new_map.get(key) {
            None => {
                out.insert(
                    old.symbol.clone(),
                    AlteredSignature {
                        path: path.to_owned(),
                        symbol: old.symbol.clone(),
                        name: old.name.clone(),
                        kind: old_kind,
                        detail: "removed".to_owned(),
                        before_signature: Some(old.display_signature.clone()),
                        after_signature: None,
                    },
                );
            }
            Some(new) if new.public_kind.is_none() => {
                out.insert(
                    old.symbol.clone(),
                    AlteredSignature {
                        path: path.to_owned(),
                        symbol: old.symbol.clone(),
                        name: old.name.clone(),
                        kind: old_kind,
                        detail: "made private".to_owned(),
                        before_signature: Some(old.display_signature.clone()),
                        after_signature: Some(new.display_signature.clone()),
                    },
                );
            }
            Some(new) if old.full_signature != new.full_signature || old.command != new.command => {
                let kind = new.public_kind.unwrap_or(old_kind).max(old_kind);
                out.insert(
                    old.symbol.clone(),
                    AlteredSignature {
                        path: path.to_owned(),
                        symbol: old.symbol.clone(),
                        name: old.name.clone(),
                        kind,
                        detail: "signature changed".to_owned(),
                        before_signature: Some(old.display_signature.clone()),
                        after_signature: Some(new.display_signature.clone()),
                    },
                );
            }
            Some(_) => {}
        }
    }
}

fn collect_container_shape_changes(
    path: &str,
    language: Language,
    before: (&str, &FileParse, &BTreeMap<DefKey, IndexedDef>),
    after: (&str, &FileParse, &BTreeMap<DefKey, IndexedDef>),
    out: &mut BTreeMap<String, AlteredSignature>,
) {
    let (before_src, old_parse, old_map) = before;
    let (after_src, new_parse, new_map) = after;
    let old_members = container_members(language, before_src, old_parse, old_map);
    let new_members = container_members(language, after_src, new_parse, new_map);
    for ((symbol, kind_label), old) in old_map {
        if old.kind != SymbolKind::Class || out.contains_key(symbol) {
            continue;
        }
        let Some(public_kind) = old.public_kind else {
            continue;
        };
        let Some(new) = new_map.get(&(symbol.clone(), *kind_label)) else {
            continue;
        };
        let prev = old_members.get(&old.name);
        let next = new_members.get(&old.name);
        if prev == next {
            continue;
        }
        let detail = describe_member_diff(prev, next);
        let kind = new.public_kind.unwrap_or(public_kind).max(public_kind);
        out.insert(
            symbol.clone(),
            AlteredSignature {
                path: path.to_owned(),
                symbol: symbol.clone(),
                name: old.name.clone(),
                kind,
                detail,
                before_signature: Some(old.display_signature.clone()),
                after_signature: Some(new.display_signature.clone()),
            },
        );
    }
}

fn container_members(
    language: Language,
    source: &str,
    parse: &FileParse,
    top: &BTreeMap<DefKey, IndexedDef>,
) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut by_container: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for def in &parse.definitions {
        if def.test || !matches!(def.kind, SymbolKind::Field | SymbolKind::Variant) {
            continue;
        }
        let Some(container) = &def.container else {
            continue;
        };
        let Some(parent) = top.get(&(container.clone(), SymbolKind::Class.label())) else {
            continue;
        };
        if parent.public_kind.is_none() {
            continue;
        }
        let relevant = match def.kind {
            SymbolKind::Variant => true,
            SymbolKind::Field => {
                parent.ipc_or_serde || language != Language::Rust || is_rust_pub(&def.signature)
            }
            _ => false,
        };
        if !relevant {
            continue;
        }
        let full = collapsed_full_signature(source, def.start_byte as usize, def.end_byte as usize);
        by_container
            .entry(container.clone())
            .or_default()
            .insert(def.name.clone(), full);
    }
    by_container
}

fn describe_member_diff(
    prev: Option<&BTreeMap<String, String>>,
    next: Option<&BTreeMap<String, String>>,
) -> String {
    let empty = BTreeMap::new();
    let prev = prev.unwrap_or(&empty);
    let next = next.unwrap_or(&empty);
    let mut notes = Vec::new();
    for (name, sig) in next {
        match prev.get(name) {
            None => notes.push(format!("+{name}")),
            Some(old) if old != sig => notes.push(format!("~{name}")),
            Some(_) => {}
        }
    }
    for name in prev.keys() {
        if !next.contains_key(name) {
            notes.push(format!("-{name}"));
        }
    }
    if notes.is_empty() {
        "fields/variants changed".to_owned()
    } else {
        let preview: Vec<_> = notes.into_iter().take(4).collect();
        format!("fields/variants changed ({})", preview.join(", "))
    }
}

fn public_trait_names(language: Language, source: &str, parse: &FileParse) -> HashSet<String> {
    if language != Language::Rust {
        return HashSet::new();
    }
    parse
        .definitions
        .iter()
        .filter(|def| {
            def.kind == SymbolKind::Interface
                && (is_rust_pub(&def.signature) || rust_has_ipc_event(source, def.start_line))
        })
        .map(|def| def.name.clone())
        .collect()
}

fn classify_definition(
    language: Language,
    def: &Definition,
    source: &str,
    public_traits: &HashSet<String>,
) -> Option<SignatureChangeKind> {
    match language {
        Language::Rust => classify_rust_definition(def, source, public_traits),
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Svelte => {
            (def.signature.starts_with("export ")
                || line_starts_with_export(source, def.start_line))
            .then_some(SignatureChangeKind::PublicSignature)
        }
        Language::Go => def
            .name
            .chars()
            .next()
            .is_some_and(char::is_uppercase)
            .then_some(SignatureChangeKind::PublicSignature),
        Language::Java | Language::CSharp => (def.signature.starts_with("public ")
            || def.signature.starts_with("protected "))
        .then_some(SignatureChangeKind::PublicSignature),
        Language::Python => {
            (!def.name.starts_with('_')).then_some(SignatureChangeKind::PublicSignature)
        }
        Language::C | Language::Cpp => {
            (!def.signature.starts_with("static ")).then_some(SignatureChangeKind::PublicSignature)
        }
        Language::Lean => {
            (!def.signature.starts_with("private ")).then_some(SignatureChangeKind::PublicSignature)
        }
        Language::Markdown => None,
    }
}

fn classify_rust_definition(
    def: &Definition,
    source: &str,
    public_traits: &HashSet<String>,
) -> Option<SignatureChangeKind> {
    if def.command {
        return Some(SignatureChangeKind::IpcCommand);
    }
    if matches!(def.kind, SymbolKind::Class | SymbolKind::Type) {
        if rust_has_ipc_event(source, def.start_line) {
            return Some(SignatureChangeKind::IpcEvent);
        }
        if rust_has_ipc_payload(source, def.start_line) {
            return Some(SignatureChangeKind::IpcPayload);
        }
    }
    if is_rust_pub(&def.signature) || def.trait_name.is_some() {
        return Some(SignatureChangeKind::PublicSignature);
    }
    if def.kind == SymbolKind::Method
        && def
            .container
            .as_ref()
            .is_some_and(|container| public_traits.contains(container))
    {
        return Some(SignatureChangeKind::PublicSignature);
    }
    None
}

fn is_rust_pub(signature: &str) -> bool {
    signature.starts_with("pub ") || signature.starts_with("pub(")
}

fn rust_attributes_above(source: &str, start_line: u32) -> Vec<&str> {
    let lines: Vec<&str> = source.lines().collect();
    let end = (start_line as usize).saturating_sub(1).min(lines.len());
    lines[..end]
        .iter()
        .rev()
        .map(|line| line.trim())
        .take_while(|line| {
            line.starts_with("#[")
                || line.starts_with("///")
                || line.starts_with("//")
                || line.is_empty()
        })
        .filter(|line| line.starts_with("#["))
        .collect()
}

fn rust_has_ipc_event(source: &str, start_line: u32) -> bool {
    rust_attributes_above(source, start_line)
        .into_iter()
        .any(|attr| {
            attr.contains("tauri_specta::Event")
                || attr.contains("tauri_specta(")
                || (attr.contains("derive(") && contains_ident(attr, "Event"))
        })
}

fn rust_has_ipc_payload(source: &str, start_line: u32) -> bool {
    rust_attributes_above(source, start_line)
        .into_iter()
        .any(|attr| {
            attr.contains("specta::Type")
                || (attr.contains("derive(") && contains_ident(attr, "Type"))
        })
}

fn rust_has_serde_or_ipc(source: &str, start_line: u32) -> bool {
    rust_has_ipc_event(source, start_line)
        || rust_has_ipc_payload(source, start_line)
        || rust_attributes_above(source, start_line)
            .into_iter()
            .any(|attr| {
                attr.contains("Serialize")
                    || attr.contains("Deserialize")
                    || attr.contains("serde(")
            })
}

fn line_starts_with_export(source: &str, start_line: u32) -> bool {
    source
        .lines()
        .nth((start_line as usize).saturating_sub(1))
        .is_some_and(|line| line.trim_start().starts_with("export "))
}

fn collapsed_full_signature(source: &str, start: usize, end: usize) -> String {
    let end = end.min(source.len());
    let text = source.get(start..end).unwrap_or_default();
    let brace = text.find('{').unwrap_or(text.len());
    let semicolon = text.find(';').map_or(text.len(), |at| at + 1);
    let arrow = text.find("=>").map_or(text.len(), |at| at + 2);
    let stop = brace.min(semicolon).min(arrow).min(MAX_SIGNATURE_BYTES);
    let slice = &text[..text.floor_char_boundary(stop)];
    slice.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn qualified_name(definition: &Definition, language: Language) -> String {
    let separator = if language == Language::Lean {
        "."
    } else {
        "::"
    };
    definition.container.as_ref().map_or_else(
        || definition.name.clone(),
        |container| format!("{container}{separator}{}", definition.name),
    )
}

/// All code file paths (excluding docs, tests, and the defining file itself) that `graph_impact`
/// reports as depending on `altered`.
pub fn impacted_code_paths(graph: &Graph, altered: &AlteredSignature) -> BTreeSet<String> {
    let targets = locate_symbol_records(graph, altered);
    if targets.is_empty() {
        return BTreeSet::new();
    }
    let mut paths = BTreeSet::new();
    for hit in impact_records(graph, &targets, 2) {
        if hit.relation == Relation::Mentions {
            continue;
        }
        let record = &graph.records[hit.record];
        if record.test || matches!(record.kind, RecordKind::Symbol(kind) if kind.is_doc()) {
            continue;
        }
        if !record.path.is_empty() && record.path != altered.path {
            paths.insert(record.path.clone());
        }
    }
    paths
}

fn locate_symbol_records(graph: &Graph, altered: &AlteredSignature) -> Vec<usize> {
    let mut targets = Vec::new();
    if let Some(&file_index) = graph.by_path.get(&altered.path) {
        for &child in &graph.records[file_index].down {
            let record = &graph.records[child];
            if !record.test && (record.qualified == altered.symbol || record.name == altered.symbol)
            {
                targets.push(child);
            }
        }
    }
    if targets.is_empty() {
        for &index in graph.named(&altered.symbol) {
            let record = &graph.records[index];
            if record.path == altered.path && !record.test {
                targets.push(index);
            }
        }
    }
    targets.sort_unstable();
    targets.dedup();
    targets
}

/// Candidate identifier names by which callers in Rust or TypeScript reference `altered`.
pub fn symbol_reference_names(altered: &AlteredSignature) -> Vec<String> {
    let mut names = vec![altered.name.clone()];
    if altered.kind == SignatureChangeKind::IpcCommand {
        let camel = camel_case(&altered.name);
        if camel != altered.name {
            names.push(camel);
        }
    }
    if matches!(
        altered.kind,
        SignatureChangeKind::IpcEvent | SignatureChangeKind::IpcPayload
    ) {
        names.push(format!("{}_Serialize", altered.name));
        names.push(format!("{}_Deserialize", altered.name));
    }
    names
}

/// Whether `source` (parsed as its language) contains a reference or import matching `altered`.
pub fn file_references_symbol(path: &str, source: &str, altered: &AlteredSignature) -> bool {
    let Some(language) = Language::of(path).filter(|lang| *lang != Language::Markdown) else {
        return false;
    };
    if source.len() > MAX_SOURCE_BYTES {
        return false;
    }
    let names = symbol_reference_names(altered);
    let parse = extract(language, source);
    let referenced = parse
        .references
        .iter()
        .any(|reference| names.contains(&reference.name));
    if referenced {
        return true;
    }
    parse.imports.iter().any(|import| {
        let rust_leaf = import.source.rsplit("::").next().unwrap_or_default();
        let original = import.original.as_deref().unwrap_or(&import.name);
        names
            .iter()
            .any(|name| import.name == *name || original == *name || rust_leaf == *name)
    })
}

fn diff_mentions_symbol(file: &FileDiff, altered: &AlteredSignature) -> bool {
    let names = symbol_reference_names(altered);
    file.hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| matches!(line.kind, LineKind::Added | LineKind::Removed))
        .any(|line| names.iter().any(|name| contains_ident(&line.text, name)))
}

/// Matches an altered signature from `peer` against issue/task `texts` at dispatch time.
pub fn match_altered_against_issue(
    graph: &Graph,
    altered: &AlteredSignature,
    issue_texts: &[String],
    peer: &PeerSummary,
) -> Option<StaleReferenceWarning> {
    let impacted = impacted_code_paths(graph, altered);
    let mut mentioned = BTreeSet::new();
    for text in issue_texts {
        mentioned.extend(graph.mentioned_paths(text));
    }
    let mut referencing_paths: Vec<String> = mentioned
        .iter()
        .filter(|path| **path == altered.path || impacted.contains(*path))
        .cloned()
        .collect();
    let names = symbol_reference_names(altered);
    let directly_named = issue_texts.iter().any(|text| {
        contains_ident(text, &altered.symbol)
            || names
                .iter()
                .filter(|name| is_distinctive_symbol_name(name))
                .any(|name| contains_ident(text, name))
    });
    if referencing_paths.is_empty() && !directly_named {
        return None;
    }
    if referencing_paths.is_empty() {
        referencing_paths.push(altered.path.clone());
    }
    Some(StaleReferenceWarning {
        symbol: altered.symbol.clone(),
        kind: altered.kind,
        defined_in: altered.path.clone(),
        detail: altered.detail.clone(),
        direction: ReferenceDirection::PeerAlteredWorkTouches,
        peer_change: peer.change.clone(),
        peer_owner: peer.owner.clone(),
        peer_status: peer.status,
        peer_commit: peer.commit.clone(),
        referencing_paths,
    })
}

/// Checks `change` against other open/ready changes and recently landed commits on `base`
/// for stale references to altered public signatures, IPC commands, and IPC event/payload types.
pub async fn change_stale_reference_warnings(
    client: &Client,
    graph: &Graph,
    change: &str,
    filter_paths: &[String],
) -> Vec<StaleReferenceWarning> {
    let Ok(current_diff) = client
        .diff(DiffRequest {
            to: Some(change.to_owned()),
            context: Some(0),
            ..DiffRequest::default()
        })
        .await
    else {
        return Vec::new();
    };
    let current_altered = diff_altered_signatures(client, &current_diff, MAX_DIFF_FILES).await;
    let peers = load_peers(client, Some(change)).await;
    let mut warnings = Vec::new();
    check_current_altered_against_peers(client, graph, &current_altered, &peers, &mut warnings)
        .await;
    check_peers_altered_against_current(client, graph, &current_diff, &peers, &mut warnings).await;
    finalize_warnings(warnings, filter_paths)
}

/// Checks whether open/ready or recently landed Rosen changes alter any public signature,
/// IPC command, or IPC event/payload type touched by an issue or task (`texts`) at dispatch.
pub async fn dispatch_stale_reference_warnings(
    client: &Client,
    graph: &Graph,
    texts: &[String],
    exclude_change: Option<&str>,
) -> Vec<StaleReferenceWarning> {
    if texts.iter().all(|text| text.trim().is_empty()) {
        return Vec::new();
    }
    let candidate_files = issue_provider_files(graph, texts);
    let peers = load_peers(client, exclude_change).await;
    let mut warnings = Vec::new();
    for peer in &peers {
        let relevant: Vec<&FileDiff> = peer
            .diff
            .files
            .iter()
            .filter(|file| {
                !file.binary
                    && Language::of(&file.path).is_some_and(|lang| lang != Language::Markdown)
                    && (candidate_files.contains(&file.path)
                        || diff_mentions_any_issue_token(file, texts))
            })
            .take(MAX_PEER_DIFF_FILES)
            .collect();
        for file in relevant {
            let altered_list = file_altered_signatures(client, &peer.diff, file).await;
            for altered in &altered_list {
                if let Some(warning) =
                    match_altered_against_issue(graph, altered, texts, &peer.summary)
                {
                    warnings.push(warning);
                }
            }
        }
    }
    finalize_warnings(warnings, &[])
}

async fn check_current_altered_against_peers(
    client: &Client,
    graph: &Graph,
    current_altered: &[AlteredSignature],
    peers: &[PeerChange],
    warnings: &mut Vec<StaleReferenceWarning>,
) {
    for altered in current_altered {
        let impacted = impacted_code_paths(graph, altered);
        for peer in peers {
            let mut refs = BTreeSet::new();
            for path in peer
                .diff
                .files
                .iter()
                .map(|file| file.path.as_str())
                .chain(peer.claimed_paths.iter().map(String::as_str))
            {
                if impacted.contains(path) {
                    refs.insert(path.to_owned());
                }
            }
            for file in &peer.diff.files {
                if file.binary || file.path == altered.path || refs.contains(&file.path) {
                    continue;
                }
                if !diff_mentions_symbol(file, altered) {
                    continue;
                }
                let revision = peer.diff.to.to_string();
                if let Some(text) = read_snapshot(client, &revision, &file.path).await
                    && file_references_symbol(&file.path, &text, altered)
                {
                    refs.insert(file.path.clone());
                }
            }
            if !refs.is_empty() {
                warnings.push(StaleReferenceWarning {
                    symbol: altered.symbol.clone(),
                    kind: altered.kind,
                    defined_in: altered.path.clone(),
                    detail: altered.detail.clone(),
                    direction: ReferenceDirection::CurrentAlteredPeerReferences,
                    peer_change: peer.summary.change.clone(),
                    peer_owner: peer.summary.owner.clone(),
                    peer_status: peer.summary.status,
                    peer_commit: peer.summary.commit.clone(),
                    referencing_paths: refs.into_iter().collect(),
                });
            }
        }
    }
}

async fn check_peers_altered_against_current(
    client: &Client,
    graph: &Graph,
    current_diff: &Diff,
    peers: &[PeerChange],
    warnings: &mut Vec<StaleReferenceWarning>,
) {
    if current_diff.files.is_empty() {
        return;
    }
    let (current_paths, current_texts) = load_current_code_texts(client, current_diff).await;
    let candidate_providers = current_provider_files(graph, &current_paths, &current_texts);
    for peer in peers {
        let files: Vec<&FileDiff> = peer
            .diff
            .files
            .iter()
            .filter(|file| {
                !file.binary
                    && Language::of(&file.path).is_some_and(|lang| lang != Language::Markdown)
                    && candidate_providers.contains(&file.path)
            })
            .take(MAX_PEER_DIFF_FILES)
            .collect();
        for file in files {
            let altered_list = file_altered_signatures(client, &peer.diff, file).await;
            for altered in &altered_list {
                let impacted = impacted_code_paths(graph, altered);
                let mut refs = BTreeSet::new();
                for path in &current_paths {
                    if *path != altered.path && impacted.contains(path) {
                        refs.insert(path.clone());
                    }
                }
                for (path, text) in &current_texts {
                    if *path != altered.path
                        && !refs.contains(path)
                        && file_references_symbol(path, text, altered)
                    {
                        refs.insert(path.clone());
                    }
                }
                if !refs.is_empty() {
                    warnings.push(StaleReferenceWarning {
                        symbol: altered.symbol.clone(),
                        kind: altered.kind,
                        defined_in: altered.path.clone(),
                        detail: altered.detail.clone(),
                        direction: ReferenceDirection::PeerAlteredCurrentReferences,
                        peer_change: peer.summary.change.clone(),
                        peer_owner: peer.summary.owner.clone(),
                        peer_status: peer.summary.status,
                        peer_commit: peer.summary.commit.clone(),
                        referencing_paths: refs.into_iter().collect(),
                    });
                }
            }
        }
    }
}

async fn load_current_code_texts(
    client: &Client,
    diff: &Diff,
) -> (BTreeSet<String>, Vec<(String, String)>) {
    let paths: BTreeSet<String> = diff.files.iter().map(|file| file.path.clone()).collect();
    let revision = diff.to.to_string();
    let mut texts = Vec::new();
    for file in diff
        .files
        .iter()
        .filter(|file| {
            !file.binary
                && file.new.is_some()
                && Language::of(&file.path).is_some_and(|lang| lang != Language::Markdown)
        })
        .take(MAX_DIFF_FILES)
    {
        if let Some(text) = read_snapshot(client, &revision, &file.path).await {
            texts.push((file.path.clone(), text));
        }
    }
    (paths, texts)
}

fn current_provider_files(
    graph: &Graph,
    current_paths: &BTreeSet<String>,
    current_texts: &[(String, String)],
) -> HashSet<String> {
    let mut providers = outgoing_dependency_files(graph, current_paths);
    for (path, text) in current_texts {
        let Some(language) = Language::of(path) else {
            continue;
        };
        if text.len() > MAX_SOURCE_BYTES {
            continue;
        }
        let parse = extract(language, text);
        for reference in &parse.references {
            add_symbol_defining_files(graph, &reference.name, &mut providers);
            if reference.kind == crate::extract::ReferenceKind::Command {
                let snake = snake_case(&reference.name);
                add_symbol_defining_files(graph, &snake, &mut providers);
            }
        }
        for import in &parse.imports {
            let leaf = import.source.rsplit("::").next().unwrap_or(&import.name);
            let raw = import.original.as_deref().unwrap_or(leaf);
            let stripped = raw
                .strip_suffix("_Serialize")
                .or_else(|| raw.strip_suffix("_Deserialize"))
                .unwrap_or(raw);
            add_symbol_defining_files(graph, stripped, &mut providers);
        }
    }
    providers
}

fn issue_provider_files(graph: &Graph, texts: &[String]) -> HashSet<String> {
    let mut mentioned_paths = BTreeSet::new();
    for text in texts {
        mentioned_paths.extend(graph.mentioned_paths(text));
    }
    let mut providers = outgoing_dependency_files(graph, &mentioned_paths);
    providers.extend(mentioned_paths);
    for text in texts {
        for token in text.split(|ch: char| !(ch.is_alphanumeric() || ch == '_')) {
            if is_distinctive_symbol_name(token) {
                add_symbol_defining_files(graph, token, &mut providers);
                let snake = snake_case(token);
                if snake != token {
                    add_symbol_defining_files(graph, &snake, &mut providers);
                }
            }
        }
    }
    providers
}

fn outgoing_dependency_files(graph: &Graph, paths: &BTreeSet<String>) -> HashSet<String> {
    let mut queue = VecDeque::new();
    let mut seen = HashSet::new();
    for path in paths {
        if let Some(&file_idx) = graph.by_path.get(path) {
            if seen.insert(file_idx) {
                queue.push_back((file_idx, 0usize));
            }
            for &child in &graph.records[file_idx].down {
                if seen.insert(child) {
                    queue.push_back((child, 0usize));
                }
            }
        }
    }
    let mut providers = HashSet::new();
    while let Some((node, depth)) = queue.pop_front() {
        if depth >= 2 {
            continue;
        }
        for &edge_idx in &graph.outgoing[node] {
            let edge = &graph.edges[edge_idx];
            if matches!(edge.relation, Relation::Depends | Relation::Mentions) {
                continue;
            }
            let target = &graph.records[edge.to];
            if !target.path.is_empty() && !target.test {
                providers.insert(target.path.clone());
            }
            if seen.insert(edge.to) {
                queue.push_back((edge.to, depth + 1));
            }
        }
    }
    providers
}

fn add_symbol_defining_files(graph: &Graph, name: &str, out: &mut HashSet<String>) {
    let matches = graph.named(name);
    if matches.len() > 16 {
        return;
    }
    for &index in matches {
        let record = &graph.records[index];
        if !record.test && matches!(record.kind, RecordKind::Symbol(kind) if !kind.is_doc()) {
            out.insert(record.path.clone());
        }
    }
}

fn diff_mentions_any_issue_token(file: &FileDiff, texts: &[String]) -> bool {
    let tokens: HashSet<&str> = texts
        .iter()
        .flat_map(|text| text.split(|ch: char| !(ch.is_alphanumeric() || ch == '_')))
        .filter(|token| is_distinctive_symbol_name(token))
        .take(64)
        .collect();
    if tokens.is_empty() {
        return false;
    }
    file.hunks.iter().flat_map(|hunk| &hunk.lines).any(|line| {
        line.text
            .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
            .any(|word| tokens.contains(word))
    })
}

async fn load_peers(client: &Client, exclude_change: Option<&str>) -> Vec<PeerChange> {
    let mut peers = Vec::new();
    let mut seen_changes = HashSet::new();
    if let Some(excluded) = exclude_change {
        seen_changes.insert(excluded.to_owned());
    }
    load_open_peers(client, &mut seen_changes, &mut peers).await;
    load_recent_landed_peers(client, &mut seen_changes, &mut peers).await;
    peers
}

async fn load_open_peers(
    client: &Client,
    seen_changes: &mut HashSet<String>,
    peers: &mut Vec<PeerChange>,
) {
    let Ok((mut records, claims)) = tokio::try_join!(
        client.change_list(Default::default()),
        client.claim_list(Default::default()),
    ) else {
        return;
    };
    let mut claims_by_change: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for claim in claims {
        claims_by_change
            .entry(claim.change.to_string())
            .or_default()
            .push(claim.path);
    }
    records.retain(|record| !record.change.state.is_closed());
    records.sort_by_key(|record| std::cmp::Reverse(record.updated_at));
    for record in records.into_iter().take(MAX_OPEN_PEERS) {
        let id = record.change.id.to_string();
        if !seen_changes.insert(id.clone()) {
            continue;
        }
        let Ok(diff) = client
            .diff(DiffRequest {
                to: Some(id.clone()),
                context: Some(0),
                ..DiffRequest::default()
            })
            .await
        else {
            continue;
        };
        let status = if record.change.state.as_str() == "ready" {
            ChangeStatus::Ready
        } else {
            ChangeStatus::Open
        };
        peers.push(PeerChange {
            summary: PeerSummary {
                change: id.clone(),
                owner: record.change.owner.to_string(),
                status,
                commit: None,
            },
            diff,
            claimed_paths: claims_by_change.remove(&id).unwrap_or_default(),
        });
    }
}

async fn load_recent_landed_peers(
    client: &Client,
    seen_changes: &mut HashSet<String>,
    peers: &mut Vec<PeerChange>,
) {
    let Ok(log) = client
        .log(LogRequest {
            at: None,
            paths: Vec::new(),
            limit: Some(MAX_RECENT_LANDED),
        })
        .await
    else {
        return;
    };
    for entry in log.entries {
        let Some(parent) = entry.parents.first() else {
            continue;
        };
        let commit = entry.commit.to_string();
        let change_id = entry
            .change
            .map(|id| id.to_string())
            .unwrap_or_else(|| commit.clone());
        if !seen_changes.insert(change_id.clone()) {
            continue;
        }
        let Ok(diff) = client
            .diff(DiffRequest {
                from: Some(parent.to_string()),
                to: Some(commit.clone()),
                context: Some(0),
                ..DiffRequest::default()
            })
            .await
        else {
            continue;
        };
        let owner = entry
            .author
            .split('@')
            .next()
            .unwrap_or(&entry.author)
            .trim()
            .to_owned();
        peers.push(PeerChange {
            summary: PeerSummary {
                change: change_id,
                owner,
                status: ChangeStatus::Landed,
                commit: Some(commit),
            },
            diff,
            claimed_paths: Vec::new(),
        });
    }
}

async fn diff_altered_signatures(
    client: &Client,
    diff: &Diff,
    limit: usize,
) -> Vec<AlteredSignature> {
    let mut results = Vec::new();
    for file in diff
        .files
        .iter()
        .filter(|file| {
            !file.binary
                && file.old.is_some()
                && Language::of(&file.path).is_some_and(|lang| lang != Language::Markdown)
        })
        .take(limit)
    {
        results.extend(file_altered_signatures(client, diff, file).await);
    }
    results
}

async fn file_altered_signatures(
    client: &Client,
    diff: &Diff,
    file: &FileDiff,
) -> Vec<AlteredSignature> {
    if file.old.is_none() {
        return Vec::new();
    }
    let before = read_snapshot(client, &diff.from.to_string(), &file.path).await;
    let after = if file.new.is_some() {
        read_snapshot(client, &diff.to.to_string(), &file.path).await
    } else {
        None
    };
    if file.old.is_some() != before.is_some() || file.new.is_some() != after.is_some() {
        return Vec::new();
    }
    altered_signatures(&file.path, before.as_deref(), after.as_deref())
}

async fn read_snapshot(client: &Client, revision: &str, path: &str) -> Option<String> {
    let answer = client
        .read_file(ReadRequest {
            at: Some(revision.to_owned()),
            path: path.to_owned(),
        })
        .await
        .ok()?;
    let ReadAnswer::File { data, .. } = answer else {
        return None;
    };
    String::from_utf8(data).ok()
}

fn finalize_warnings(
    warnings: Vec<StaleReferenceWarning>,
    filter_paths: &[String],
) -> Vec<StaleReferenceWarning> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for warning in warnings {
        if !filter_paths.is_empty() {
            let matches_filter = filter_paths.iter().any(|path| {
                let normalized = path.replace('\\', "/");
                warning.defined_in == normalized
                    || warning.referencing_paths.iter().any(|p| p == &normalized)
            });
            if !matches_filter {
                continue;
            }
        }
        let key = (
            warning.symbol.clone(),
            warning.defined_in.clone(),
            warning.peer_change.clone(),
            warning.direction,
        );
        if seen.insert(key) {
            out.push(warning);
            if out.len() >= MAX_WARNINGS {
                break;
            }
        }
    }
    out
}

/// Formats one warning as a single concise line for preflight, land feed, or task briefs.
pub fn format_warning_line(warning: &StaleReferenceWarning) -> String {
    let short_peer = warning.peer_change.get(..8).unwrap_or(&warning.peer_change);
    let commit_note = warning
        .peer_commit
        .as_deref()
        .map(|commit| format!(" @ {}", commit.get(..10).unwrap_or(commit)))
        .unwrap_or_default();
    let paths: Vec<_> = warning.referencing_paths.iter().take(4).cloned().collect();
    let more = warning.referencing_paths.len().saturating_sub(4);
    let paths_label = if more > 0 {
        format!("{} (+{more} more)", paths.join(", "))
    } else {
        paths.join(", ")
    };
    match warning.direction {
        ReferenceDirection::CurrentAlteredPeerReferences => format!(
            "`{}` ({}, {} in `{}`) is referenced by {} change `{short_peer}` ({}{commit_note}) in {paths_label}",
            warning.symbol,
            warning.kind.label(),
            warning.detail,
            warning.defined_in,
            warning.peer_status.label(),
            warning.peer_owner,
        ),
        ReferenceDirection::PeerAlteredCurrentReferences => format!(
            "`{}` ({}, {} in `{}`) was altered by {} change `{short_peer}` ({}{commit_note}) and is referenced here in {paths_label}",
            warning.symbol,
            warning.kind.label(),
            warning.detail,
            warning.defined_in,
            warning.peer_status.label(),
            warning.peer_owner,
        ),
        ReferenceDirection::PeerAlteredWorkTouches => format!(
            "`{}` ({}, {} in `{}`) is altered by {} change `{short_peer}` ({}{commit_note}); this work touches {paths_label}",
            warning.symbol,
            warning.kind.label(),
            warning.detail,
            warning.defined_in,
            warning.peer_status.label(),
            warning.peer_owner,
        ),
    }
}

/// Formats a compact one-line summary of `warnings` for toasts and duty activity lines.
pub fn format_warning_summary(warnings: &[StaleReferenceWarning]) -> String {
    let symbols: Vec<_> = warnings
        .iter()
        .map(|warning| warning.symbol.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(3)
        .collect();
    let peers: Vec<_> = warnings
        .iter()
        .map(|warning| {
            let short = warning.peer_change.get(..8).unwrap_or(&warning.peer_change);
            format!("{} ({short})", warning.peer_owner)
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(3)
        .collect();
    format!(
        "{} stale-reference warning(s) for {} against {}",
        warnings.len(),
        symbols.join(", "),
        peers.join(", ")
    )
}

fn is_distinctive_symbol_name(name: &str) -> bool {
    name.len() >= 4
        && (name.contains('_') || name.contains("::") || name.chars().any(char::is_uppercase))
}

fn contains_ident(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = haystack.as_bytes();
    let mut start = 0;
    while let Some(offset) = haystack[start..].find(needle) {
        let pos = start + offset;
        let before_ok = pos == 0 || !is_ident_byte(bytes[pos - 1]);
        let after = pos + needle.len();
        let after_ok = after == bytes.len() || !is_ident_byte(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
        start = after;
    }
    false
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn camel_case(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper = false;
    for ch in snake.chars() {
        if ch == '_' {
            upper = true;
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

fn snake_case(camel: &str) -> String {
    let mut out = String::with_capacity(camel.len() + 4);
    for (index, ch) in camel.chars().enumerate() {
        if ch.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::graph::FileInput;
    use crate::subsystems::detect;

    fn build_graph(files: &[(&str, &str)]) -> Graph {
        let paths: Vec<String> = files.iter().map(|(path, _)| (*path).to_owned()).collect();
        let manifests = HashMap::new();
        let subsystems = detect(&paths, |_| None);
        let inputs: Vec<FileInput> = files
            .iter()
            .map(|(path, text)| {
                let language = Language::of(path);
                let parse = language.map(|lang| extract(lang, text)).unwrap_or_default();
                FileInput {
                    path: (*path).to_owned(),
                    hash: "test".to_owned(),
                    language,
                    parse: Arc::new(parse),
                }
            })
            .collect();
        Graph::build(&inputs, subsystems, &manifests)
    }

    // Risk (#542): body-only edits must not produce false signature warnings, whereas
    // public signature changes, IPC commands, and IPC event payload edits must be detected.
    #[test]
    fn detects_public_signature_and_ipc_changes_without_body_noise() {
        let before = r#"
pub fn compute_plan(count: u32) -> bool {
    count > 0
}

fn helper(value: u32) -> u32 {
    value + 1
}

#[tauri::command]
pub async fn dispatch_work(id: u32) -> Result<(), String> {
    Ok(())
}

#[derive(Clone, serde::Serialize, specta::Type, tauri_specta::Event)]
pub struct WorkUpdated {
    pub id: u32,
}
"#;
        let body_only = r#"
pub fn compute_plan(count: u32) -> bool {
    count > 10
}

fn helper(value: u32, extra: u32) -> u32 {
    value + extra
}

#[tauri::command]
pub async fn dispatch_work(id: u32) -> Result<(), String> {
    let _ = id;
    Ok(())
}

#[derive(Clone, serde::Serialize, specta::Type, tauri_specta::Event)]
pub struct WorkUpdated {
    pub id: u32,
}
"#;
        assert!(
            altered_signatures("src-tauri/src/work.rs", Some(before), Some(body_only)).is_empty()
        );

        let altered_src = r#"
pub fn compute_plan(count: u32, strict: bool) -> bool {
    strict && count > 0
}

#[tauri::command]
pub async fn dispatch_work(id: u32, force: bool) -> Result<(), String> {
    let _ = (id, force);
    Ok(())
}

#[derive(Clone, serde::Serialize, specta::Type, tauri_specta::Event)]
pub struct WorkUpdated {
    pub id: u32,
    pub revision: u64,
}
"#;
        let found = altered_signatures("src-tauri/src/work.rs", Some(before), Some(altered_src));
        assert_eq!(found.len(), 3);
        assert!(found.iter().any(|item| {
            item.symbol == "compute_plan" && item.kind == SignatureChangeKind::PublicSignature
        }));
        assert!(found.iter().any(|item| {
            item.symbol == "dispatch_work" && item.kind == SignatureChangeKind::IpcCommand
        }));
        assert!(found.iter().any(|item| {
            item.symbol == "WorkUpdated"
                && item.kind == SignatureChangeKind::IpcEvent
                && item.detail.contains("+revision")
        }));
    }

    // Risk (#542): graph_impact and dispatch matching must trace altered Rust signatures
    // and IPC commands/events to Rust callers and TypeScript bindings consumers.
    #[test]
    fn traces_impact_across_rust_and_typescript_bindings_and_matches_dispatch() {
        let work_rs = r#"
#[tauri::command]
pub async fn dispatch_work(id: u32) -> Result<(), String> {
    Ok(())
}

#[derive(Clone, serde::Serialize, specta::Type, tauri_specta::Event)]
pub struct WorkUpdated {
    pub id: u32,
}
"#;
        let caller_rs = r#"
use crate::work::WorkUpdated;

pub fn emit_work() -> WorkUpdated {
    WorkUpdated { id: 1 }
}
"#;
        let ipc_ts = r#"
import { commands, type WorkUpdated } from './bindings';

export async function runWork(id: number): Promise<WorkUpdated | null> {
    await commands.dispatchWork(id);
    return null;
}
"#;
        let graph = build_graph(&[
            ("src-tauri/src/work.rs", work_rs),
            ("src-tauri/src/caller.rs", caller_rs),
            ("ui/src/lib/ipc/work.ts", ipc_ts),
        ]);

        let cmd_altered = AlteredSignature {
            path: "src-tauri/src/work.rs".into(),
            symbol: "dispatch_work".into(),
            name: "dispatch_work".into(),
            kind: SignatureChangeKind::IpcCommand,
            detail: "signature changed".into(),
            before_signature: None,
            after_signature: None,
        };
        let cmd_impacted = impacted_code_paths(&graph, &cmd_altered);
        assert!(cmd_impacted.contains("ui/src/lib/ipc/work.ts"));

        let event_altered = AlteredSignature {
            path: "src-tauri/src/work.rs".into(),
            symbol: "WorkUpdated".into(),
            name: "WorkUpdated".into(),
            kind: SignatureChangeKind::IpcEvent,
            detail: "fields/variants changed (+revision)".into(),
            before_signature: None,
            after_signature: None,
        };
        let event_impacted = impacted_code_paths(&graph, &event_altered);
        assert!(event_impacted.contains("src-tauri/src/caller.rs"));
        assert!(event_impacted.contains("ui/src/lib/ipc/work.ts"));

        let peer = PeerSummary {
            change: "abcd1234efgh5678".into(),
            owner: "issue-500".into(),
            status: ChangeStatus::Open,
            commit: None,
        };
        let warning = match_altered_against_issue(
            &graph,
            &event_altered,
            &["Update emit_work in src-tauri/src/caller.rs".into()],
            &peer,
        );
        assert!(warning.is_some());
        if let Some(warning) = warning {
            assert_eq!(warning.symbol, "WorkUpdated");
            assert!(
                warning
                    .referencing_paths
                    .contains(&"src-tauri/src/caller.rs".to_owned())
            );
            assert!(format_warning_line(&warning).contains("WorkUpdated"));
        }
    }
}
