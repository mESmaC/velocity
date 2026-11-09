//! The spec interview's read model (SPEC.md F3): the question card, the decisions, the
//! proposed proof list and the phase the thread is in.

use crate::navigation::NavigationView;

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterviewPhase {
    Scan,
    Ask,
    Ready,
    Locked,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterviewOptionView {
    pub label: String,
    pub detail: String,
    pub recommended: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterviewQuestionView {
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub number: usize,
    pub key: String,
    pub text: String,
    pub why: String,
    pub options: Vec<InterviewOptionView>,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterviewDecisionView {
    pub key: String,
    pub value: String,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProofListEntryView {
    pub feature: String,
    pub included: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterviewView {
    pub phase: InterviewPhase,
    pub brief: String,
    pub summary: String,
    /// Shown only while the agent waits for an answer, never while it is still working.
    pub question: Option<InterviewQuestionView>,
    pub decisions: Vec<InterviewDecisionView>,
    pub proof_list: Vec<ProofListEntryView>,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub task_count: usize,
    /// "scanning…", "Q3 · 2 decided", "ready to lock", "locked · 9 tasks".
    pub status_label: String,
    /// Why the last action did not go through, such as locking before SPEC.md exists.
    pub notice: Option<String>,
}

/// One interview, keyed by its thread.
#[derive(Clone, Debug)]
pub struct InterviewEntry {
    pub thread_identifier: usize,
    pub view: InterviewView,
}

/// The view a spec thread opens in: its phase decides which keys work (§5).
pub fn interview_navigation_view(phase: InterviewPhase) -> NavigationView {
    match phase {
        InterviewPhase::Scan => NavigationView::SpecScan,
        InterviewPhase::Ask => NavigationView::SpecAsk,
        InterviewPhase::Ready => NavigationView::SpecReady,
        InterviewPhase::Locked => NavigationView::SpecLocked,
    }
}

/// The sidebar and header label for an interview, as in the reference.
pub fn interview_status_label(
    phase: InterviewPhase,
    question_number: usize,
    decided: usize,
    task_count: usize,
) -> String {
    match phase {
        InterviewPhase::Scan => "scanning…".into(),
        InterviewPhase::Ask => format!("Q{question_number} · {decided} decided"),
        InterviewPhase::Ready => "ready to lock".into(),
        InterviewPhase::Locked => format!("locked · {task_count} tasks"),
    }
}

/// The composer hint for each phase.
pub fn interview_placeholder(phase: InterviewPhase) -> &'static str {
    match phase {
        InterviewPhase::Ask => "Answer in your own words, or press 1–4 to pick",
        InterviewPhase::Scan | InterviewPhase::Ready => "Ask the spec agent anything",
        InterviewPhase::Locked => "Spec locked · the Task List is now the source of truth",
    }
}
