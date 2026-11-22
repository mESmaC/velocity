//! Native goal-swarm candidate ranking and the optional first-turn choice protocol.

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use agent_client_protocol::v1::schema::{ContentBlock, SessionUpdate};
use serde::Deserialize;
use velocity_acp::client::AgentEvent;
use velocity_airc::claims::{WorkClaims, normalize_file, repository_relative};
use velocity_core::goal_swarm::DEFAULT_PROMPT;
use velocity_core::goal_swarm_choice::{canonical_piece_key, is_piece_unclaimed};
use velocity_core::text_budget::{PROMPT_BYTES, json_text_page, prompt_excerpt};
use velocity_core::work_pool::{WorkKey, WorkPiece, WorkPool};
use velocity_graph::coordination::SubsystemMap;
use velocity_rosen::PathPattern;
use velocity_store::rosen_work::OwnedIssueVersion;

use crate::goal_swarm::GoalSwarmError;

const SHORTLIST_LENGTH: usize = 5;

pub(crate) struct Candidate {
    pub piece: WorkPiece,
    pub files: Vec<String>,
    pub issue_version: Option<OwnedIssueVersion>,
    subsystems: BTreeSet<String>,
}

pub(crate) fn candidates(
    pool: WorkPool,
    checkout: &Path,
    mut issue_files: HashMap<u32, Vec<String>>,
    mut issue_versions: HashMap<u32, OwnedIssueVersion>,
    map: Option<&SubsystemMap>,
) -> Vec<Candidate> {
    let mut candidates: Vec<_> = pool
        .pieces
        .into_iter()
        .map(|piece| {
            let mut files = referenced_files(checkout, &piece.title);
            let mut issue_version = None;
            if let WorkKey::Issue { number, .. } = piece.key {
                files.extend(issue_files.remove(&number).into_iter().flatten());
                issue_version = issue_versions.remove(&number);
            }
            files.sort();
            files.dedup();
            let subsystems = subsystems(map, &files, &piece.title);
            Candidate {
                piece,
                files,
                issue_version,
                subsystems,
            }
        })
        .collect();
    // The spec parser supplies §7 order and the issue reader supplies creation order.
    candidates.sort_by_key(|candidate| match candidate.piece.key {
        WorkKey::Spec { .. } => 0,
        WorkKey::Task { .. } => 1,
        WorkKey::Issue { .. } => 2,
    });
    candidates
}

pub(crate) fn referenced_files(checkout: &Path, text: &str) -> Vec<String> {
    let quoted = text.split('`').skip(1).step_by(2);
    let bare = text
        .split(|character: char| character.is_whitespace() || "()[]<>,;\"'`".contains(character))
        .filter(|word| word.contains(['.', '/', '\\']) || word.chars().all(char::is_uppercase));
    quoted
        .chain(bare)
        .filter_map(|hint| {
            let hint = hint.trim_end_matches('.');
            if !repository_relative(hint) || hint.contains("://") {
                return None;
            }
            let path = hint.split_once(':').map_or(hint, |(path, _)| path);
            let file = normalize_file(path);
            if checkout.join(&file).is_file() {
                return Some(file);
            }
            let (path, _) = file.split_once("#L")?;
            checkout.join(path).is_file().then(|| path.to_owned())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn subsystems(map: Option<&SubsystemMap>, files: &[String], title: &str) -> BTreeSet<String> {
    let Some(map) = map else {
        return BTreeSet::new();
    };
    let mut known: BTreeSet<_> = files
        .iter()
        .filter_map(|file| map.for_file(claim_path(file)).map(str::to_owned))
        .collect();
    if known.is_empty() && !title.is_empty() {
        known.extend(map.classify(title));
    }
    known
}

fn claim_path(file: &str) -> &str {
    file.strip_suffix("::registration").unwrap_or(file)
}

pub(crate) struct HeldWork {
    keys: BTreeSet<String>,
    files: BTreeSet<String>,
    patterns: Vec<PathPattern>,
    subsystems: BTreeSet<String>,
}

impl HeldWork {
    pub fn new(claims: WorkClaims, candidates: &[Candidate], map: Option<&SubsystemMap>) -> Self {
        let mut held = Self {
            keys: BTreeSet::new(),
            files: BTreeSet::new(),
            patterns: Vec::new(),
            subsystems: BTreeSet::new(),
        };
        for pattern in claims.paths.iter().filter(|path| path.contains(['*', '?'])) {
            held.patterns.extend(PathPattern::new(pattern).ok());
            held.subsystems
                .extend(map.and_then(|map| map.classify(pattern)));
        }
        held.subsystems.extend(subsystems(map, &claims.paths, ""));
        held.files.extend(
            claims
                .paths
                .into_iter()
                .map(|file| normalize_file(claim_path(&file))),
        );
        for claim in claims.pieces {
            held.keys.insert(canonical_piece_key(claim.key()));
            held.subsystems
                .extend(subsystems(map, &claim.files, &claim.task));
            held.files.extend(
                claim
                    .files
                    .iter()
                    .map(|file| normalize_file(claim_path(file))),
            );
        }
        for candidate in candidates {
            if held.keys.contains(&candidate.piece.key.claim_piece())
                || candidate.piece.thread_identifier.is_some()
            {
                held.files.extend(candidate.files.iter().cloned());
                held.subsystems.extend(candidate.subsystems.iter().cloned());
            }
        }
        held
    }

    pub fn allows(&self, candidate: &Candidate) -> bool {
        is_piece_unclaimed(&candidate.piece, &self.keys)
            && !candidate.files.iter().any(|file| {
                self.files.contains(file)
                    || self.patterns.iter().any(|pattern| pattern.matches(file))
            })
            && candidate.subsystems.is_disjoint(&self.subsystems)
    }
}

pub(crate) fn shortlist<'a>(
    goal: &str,
    available: impl Iterator<Item = &'a Candidate>,
) -> Vec<&'a Candidate> {
    let words = goal_words(goal);
    let mut ranked: Vec<_> = available.collect();
    ranked.sort_by_cached_key(|candidate| {
        let piece = &candidate.piece;
        let exact_issue = matches!(piece.key, WorkKey::Issue { number, .. }
            if goal.split(|character: char| !character.is_alphanumeric() && character != '#')
                .any(|word| word.strip_prefix('#').and_then(|value| value.parse().ok()) == Some(number)));
        let title = format!("{} {}", piece.title, piece.key.claim_piece());
        Reverse((exact_issue, goal_words(&title).intersection(&words).count()))
    });
    ranked.truncate(SHORTLIST_LENGTH);
    ranked
}

fn goal_words(text: &str) -> BTreeSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.len() > 1)
        .map(str::to_lowercase)
        .collect()
}

pub(crate) fn needs_choice(goal: &str) -> bool {
    let goal = goal.trim();
    !goal.is_empty() && !goal.eq_ignore_ascii_case(DEFAULT_PROMPT)
}

pub fn selection_prompt(goal: &str, shortlist: &[&WorkPiece]) -> String {
    let goal = prompt_excerpt(goal, 2_500, "\n[Goal excerpt]");
    let mut prompt = format!(
        "{goal}\n\n---\nVelocity goal swarm first turn (F16).\n\
         Read the goal above and use the candidate titles, references and any relevant evidence \
         to rank only the pieces that satisfy it. {} \n\
         This turn is for choosing only. Wait for Velocity to send the claimed piece before \
         claiming files or editing. Velocity can try the next acceptable piece if a claim is refused.\n\
         Return a JSON object with a pieces array of exact piece keys in preference order. \
         Use an empty array if none matches.\nCandidates: ",
        crate::task_spec::instructions(&[&goal]),
    );
    let mut candidates = Vec::new();
    let mut remaining = PROMPT_BYTES - prompt.len() - 100;
    for piece in shortlist {
        let row = serde_json::json!({
            "piece": piece.key.claim_piece(),
            "title": context_excerpt(&piece.title),
            "reference": context_excerpt(&piece.reference),
        });
        let size = row.to_string().len() + 1;
        if size <= remaining {
            remaining -= size;
            candidates.push(row);
        }
    }
    let omitted = candidates.len() < shortlist.len();
    prompt.push_str(&serde_json::Value::Array(candidates).to_string());
    if omitted {
        prompt.push_str("\n[Some candidate details exceeded the prompt limit.]");
    }
    prompt
}

fn context_excerpt(text: &str) -> String {
    let (page, next) = json_text_page(text, 0, 220).unwrap_or_default();
    if next.is_some() {
        format!("{page}…")
    } else {
        page.to_owned()
    }
}

#[derive(Deserialize)]
struct Selection {
    pieces: Vec<String>,
}

pub async fn selection_reply(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    first_turn: &mut Vec<AgentEvent>,
) -> Result<Vec<String>, GoalSwarmError> {
    tokio::time::timeout(
        std::time::Duration::from_secs(300),
        collect_reply(events, first_turn),
    )
    .await
    .map_err(selection_error)?
}

async fn collect_reply(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    first_turn: &mut Vec<AgentEvent>,
) -> Result<Vec<String>, GoalSwarmError> {
    let mut reply = String::new();
    while let Some(event) = events.recv().await {
        match &event {
            AgentEvent::Transcript {
                update: SessionUpdate::AgentMessageChunk(chunk),
                ..
            } => {
                if let ContentBlock::Text(text) = &chunk.content {
                    reply.push_str(&text.text);
                }
            }
            AgentEvent::TurnEnded(result) => {
                result.as_ref().map_err(selection_error)?;
                return parse_selection(&reply);
            }
            _ => {}
        }
        first_turn.push(event);
    }
    Err(selection_error("The session ended before choosing a piece"))
}

// Providers can emit commentary before their final JSON answer.
fn parse_selection(reply: &str) -> Result<Vec<String>, GoalSwarmError> {
    reply
        .char_indices()
        .rev()
        .filter(|(_, character)| *character == '{')
        .find_map(|(index, _)| {
            serde_json::Deserializer::from_str(&reply[index..])
                .into_iter::<Selection>()
                .next()
                .and_then(Result::ok)
        })
        .map(|selection| selection.pieces)
        .ok_or_else(|| selection_error("The agent did not return a goal work choice"))
}

fn selection_error(error: impl std::fmt::Display) -> GoalSwarmError {
    GoalSwarmError::Selection(error.to_string())
}
