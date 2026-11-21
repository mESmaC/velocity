//! Typed payloads for structured Rosen project events so AIRC entries never put
//! raw JSON inside display strings (#540).
use std::collections::BTreeMap;

use rosen_client::protocol::{Event, EventKind};
use serde::{Deserialize, Serialize};

/// Symbol edits in an overlapping file, split by requester and current holder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct OverlapSymbols {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requester: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holder: Vec<String>,
}

/// One concurrent change whose claimed paths overlap a path claim.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct OverlapDetail {
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub symbols: BTreeMap<String, OverlapSymbols>,
}

/// One waiting change woken when another change released a path claim.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct WokenDetail {
    pub change: String,
    pub owner: String,
    pub path: String,
}

/// Typed event detail carried beside an entry's human-readable `text`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    Rebased {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        onto: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tree: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        regenerate: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        conflicts: Vec<String>,
    },
    Landed {
        commit: String,
    },
    Conflict {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        onto: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        paths: Vec<String>,
    },
    PathClaim {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exclusive: Option<bool>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        overlaps: Vec<OverlapDetail>,
    },
    Released {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        released_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wake: Option<bool>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        woken: Vec<WokenDetail>,
    },
    Reverted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        culprit: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
}

impl EventPayload {
    pub(crate) fn from_rosen(event: &Event, kind: &str, owner: &str) -> Option<(String, Self)> {
        let detail = &event.detail;
        match event.kind {
            EventKind::Rebased => Some(rebased(detail)),
            EventKind::Landed => Some(landed(detail)),
            EventKind::Conflict => Some(conflict(detail)),
            EventKind::Claim => Some(path_claim(detail, kind, owner)),
            EventKind::Released => Some(released(detail)),
            EventKind::Reverted => Some(reverted(detail)),
            _ => None,
        }
    }

    /// True when any typed field in this payload contains the lowercased `query`.
    pub(crate) fn matches_query(&self, query: &str) -> bool {
        let hit = |value: &str| value.to_lowercase().contains(query);
        let opt = |value: &Option<String>| value.as_deref().is_some_and(hit);
        match self {
            Self::Rebased {
                onto,
                tree,
                regenerate,
                conflicts,
            } => {
                opt(onto)
                    || opt(tree)
                    || regenerate.iter().any(|path| hit(path))
                    || conflicts.iter().any(|path| hit(path))
            }
            Self::Landed { commit } => hit(commit),
            Self::Conflict { onto, paths } => opt(onto) || paths.iter().any(|path| hit(path)),
            Self::PathClaim { overlaps, .. } => overlaps.iter().any(|overlap| {
                hit(&overlap.owner)
                    || opt(&overlap.change)
                    || overlap.paths.iter().any(|path| hit(path))
                    || overlap.symbols.values().any(|symbols| {
                        symbols
                            .requester
                            .iter()
                            .chain(&symbols.holder)
                            .any(|symbol| hit(symbol))
                    })
            }),
            Self::Released {
                reason,
                commit,
                path,
                released_by,
                woken,
                ..
            } => {
                opt(reason)
                    || opt(commit)
                    || opt(path)
                    || opt(released_by)
                    || woken
                        .iter()
                        .any(|item| hit(&item.change) || hit(&item.owner) || hit(&item.path))
            }
            Self::Reverted {
                culprit,
                commit,
                output,
            } => opt(culprit) || opt(commit) || opt(output),
        }
    }
}

/// Human-readable summary for a Rosen overlap event after owner nicks or symbols resolve.
pub(crate) fn overlap_text(nick: &str, overlaps: &[OverlapDetail]) -> String {
    let parts: Vec<String> = overlaps
        .iter()
        .map(|overlap| {
            let paths: Vec<String> = overlap
                .paths
                .iter()
                .map(|path| match overlap.symbols.get(path) {
                    Some(symbols) => format!(
                        "{path}{}",
                        crate::claims::symbol_overlap(&symbols.requester, &symbols.holder)
                    ),
                    None => path.clone(),
                })
                .collect();
            format!("{} on {}", overlap.owner, paths.join(", "))
        })
        .collect();
    format!(
        "{nick} overlaps {}; {}",
        parts.join("; "),
        crate::claims::OVERLAP_ADVICE
    )
}

fn rebased(detail: &serde_json::Value) -> (String, EventPayload) {
    let onto = string(detail, "onto");
    let tree = string(detail, "tree");
    let regenerate = strings(detail, "regenerate");
    let conflicts = strings(detail, "conflicts");
    let mut text = match &onto {
        Some(commit) => format!("rebased onto {}", short(commit)),
        None => "rebased".into(),
    };
    if !conflicts.is_empty() {
        text.push_str(&format!(" · conflicts: {}", conflicts.join(", ")));
    }
    if !regenerate.is_empty() {
        text.push_str(&format!(" · regenerate: {}", regenerate.join(", ")));
    }
    let payload = EventPayload::Rebased {
        onto,
        tree,
        regenerate,
        conflicts,
    };
    (text, payload)
}

fn landed(detail: &serde_json::Value) -> (String, EventPayload) {
    let commit = string(detail, "commit").unwrap_or_default();
    let text = if commit.is_empty() {
        "landed".into()
    } else {
        format!("landed {}", short(&commit))
    };
    (text, EventPayload::Landed { commit })
}

fn conflict(detail: &serde_json::Value) -> (String, EventPayload) {
    let onto = string(detail, "onto");
    let paths = strings(detail, "paths");
    let mut text = if paths.is_empty() {
        "merge conflict".into()
    } else {
        format!("conflict in {}", paths.join(", "))
    };
    if let Some(commit) = &onto {
        text.push_str(&format!(" onto {}", short(commit)));
    }
    (text, EventPayload::Conflict { onto, paths })
}

fn path_claim(detail: &serde_json::Value, kind: &str, owner: &str) -> (String, EventPayload) {
    let paths = strings(detail, "paths");
    let exclusive = detail.get("exclusive").and_then(serde_json::Value::as_bool);
    let overlaps = detail
        .get("overlaps")
        .and_then(serde_json::Value::as_array)
        .map(|rows| rows.iter().filter_map(parse_overlap).collect::<Vec<_>>())
        .unwrap_or_default();
    let text = if !overlaps.is_empty() {
        overlap_text(owner, &overlaps)
    } else {
        let joined = paths.join(", ");
        match kind {
            "shares" => format!("shares {joined}"),
            "waits" => format!("waiting on {joined}"),
            _ if joined.is_empty() => "claimed paths".into(),
            _ => format!("claimed {joined}"),
        }
    };
    let payload = EventPayload::PathClaim {
        exclusive,
        overlaps,
    };
    (text, payload)
}

fn released(detail: &serde_json::Value) -> (String, EventPayload) {
    let reason = string(detail, "reason");
    let commit = string(detail, "commit");
    let path = string(detail, "path");
    let released_by = string(detail, "released_by");
    let wake = detail.get("wake").and_then(serde_json::Value::as_bool);
    let paths = strings(detail, "paths");
    let woken = detail
        .get("woken")
        .and_then(serde_json::Value::as_array)
        .map(|rows| rows.iter().filter_map(parse_woken).collect::<Vec<_>>())
        .unwrap_or_default();
    let text = if wake == Some(true) {
        match &path {
            Some(path) => format!("unblocked on {path}"),
            None => "unblocked".into(),
        }
    } else if let Some(reason) = &reason {
        match &commit {
            Some(commit) => format!("released claims · {reason} {}", short(commit)),
            None => format!("released claims · {reason}"),
        }
    } else if !paths.is_empty() {
        format!("released {}", paths.join(", "))
    } else {
        "released claims".into()
    };
    let payload = EventPayload::Released {
        reason,
        commit,
        path,
        released_by,
        wake,
        woken,
    };
    (text, payload)
}

fn reverted(detail: &serde_json::Value) -> (String, EventPayload) {
    let culprit = string(detail, "culprit");
    let commit = string(detail, "commit");
    let output = string(detail, "output");
    let text = match (&culprit, &commit) {
        (Some(culprit), Some(commit)) => {
            format!("reverted {} (revert {})", short(culprit), short(commit))
        }
        (Some(culprit), None) => format!("reverted {}", short(culprit)),
        _ => "reverted".into(),
    };
    let payload = EventPayload::Reverted {
        culprit,
        commit,
        output,
    };
    (text, payload)
}

fn parse_overlap(value: &serde_json::Value) -> Option<OverlapDetail> {
    let owner = string(value, "owner")?;
    let change = string(value, "change");
    let paths = strings(value, "paths");
    let symbols = value
        .get("symbols")
        .cloned()
        .and_then(|symbols| serde_json::from_value(symbols).ok())
        .unwrap_or_default();
    Some(OverlapDetail {
        owner,
        change,
        paths,
        symbols,
    })
}

fn parse_woken(value: &serde_json::Value) -> Option<WokenDetail> {
    Some(WokenDetail {
        change: string(value, "change")?,
        owner: string(value, "owner")?,
        path: string(value, "path")?,
    })
}

fn string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn strings(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn short(id: &str) -> &str {
    id.get(..7).unwrap_or(id)
}
