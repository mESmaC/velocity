//! The block the spec agent ends every reply with (F3). It carries what Velocity shows as
//! structure rather than prose: the next question, the draft tasks an answer adds, the scan
//! summary, and "ready" with the proposed proof list.

use serde::Deserialize;
use velocity_store::{InterviewOption, InterviewQuestion};

/// The fence that opens the block: ```` ```velocity-interview ````.
pub const FENCE_LABEL: &str = "velocity-interview";

/// F3: "up to 4 options".
const MAX_OPTIONS: usize = 4;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Envelope {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub tasks: Vec<String>,
    #[serde(default)]
    pub question: Option<EnvelopeQuestion>,
    #[serde(default)]
    pub ready: Option<EnvelopeReady>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct EnvelopeQuestion {
    #[serde(default)]
    pub key: String,
    pub text: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub options: Vec<EnvelopeOption>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct EnvelopeOption {
    pub label: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub recommended: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct EnvelopeReady {
    #[serde(default)]
    pub proof_list: Vec<String>,
}

/// The last well-formed block in `text`, or `None` when the reply has none.
pub fn parse_envelope(text: &str) -> Option<Envelope> {
    fenced_blocks(text)
        .into_iter()
        .rev()
        .find_map(|(_, body)| serde_json::from_str::<Envelope>(body.trim()).ok())
}

/// `text` without its interview blocks: what the transcript shows as the agent's message.
pub fn without_envelopes(text: &str) -> String {
    let mut shown = String::new();
    let mut rest = 0;
    for (range, _) in fenced_blocks(text) {
        shown.push_str(&text[rest..range.start]);
        rest = range.end;
    }
    shown.push_str(&text[rest..]);
    shown.trim().to_owned()
}

/// Every ```` ```velocity-interview ```` block: its whole span and its body. An unclosed
/// block runs to the end, because a reply still streaming has not closed it yet.
fn fenced_blocks(text: &str) -> Vec<(std::ops::Range<usize>, &str)> {
    let opening = format!("```{FENCE_LABEL}");
    let mut blocks = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find(&opening) {
        let start = from + found;
        let body_start = start + opening.len();
        let (body_end, end) = match text[body_start..].find("```") {
            Some(close) => (body_start + close, body_start + close + 3),
            None => (text.len(), text.len()),
        };
        blocks.push((start..end, &text[body_start..body_end]));
        from = end;
    }
    blocks
}

impl EnvelopeQuestion {
    /// The stored question: at most four options and at most one marked recommended.
    pub fn into_question(self) -> InterviewQuestion {
        let mut recommended_seen = false;
        let options = self
            .options
            .into_iter()
            .filter(|option| !option.label.trim().is_empty())
            .take(MAX_OPTIONS)
            .map(|option| {
                let recommended = option.recommended && !recommended_seen;
                recommended_seen |= recommended;
                InterviewOption {
                    label: option.label.trim().to_owned(),
                    detail: option.detail.trim().to_owned(),
                    recommended,
                }
            })
            .collect();
        let key = if self.key.trim().is_empty() {
            String::from("Decision")
        } else {
            self.key.trim().to_owned()
        };
        InterviewQuestion {
            key,
            text: self.text.trim().to_owned(),
            why: self.why.trim().to_owned(),
            options,
        }
    }
}
