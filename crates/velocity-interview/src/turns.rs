//! How each interview step changes the saved project (F3). Pure: the service applies these
//! inside `StateStore::update_project`, so a step is saved whole or not at all.

use velocity_store::{
    AgentActivity, InterviewDecision, InterviewRecord, InterviewStage, PendingInterview,
    ProjectItem, ProofListEntry, ProviderKind, SpeedMode, TaskAttribution, TaskItem, TaskState,
    ThreadItem, ThreadState, WorktreeRole,
};

use crate::envelope::Envelope;

/// The reference's name for the interview thread.
pub const SPEC_THREAD_TITLE: &str = "Project spec";

/// Shown when a reply ends without a question or "ready".
const NO_QUESTION_NOTICE: &str =
    "The agent didn't ask a question this time. Reply in your own words.";

/// How you answered the question on the card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// `1`–`4`, or a click on that option.
    Option(usize),
    Text(String),
}

/// A recorded answer: what the agent is told, and the row the transcript shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedAnswer {
    pub question: String,
    pub key: String,
    pub value: String,
}

/// The provider and model a new interview runs on when Create recorded none (F15, #25).
pub struct InterviewDefaults {
    pub provider: ProviderKind,
    pub model_name: String,
}

/// Turns the interview Create recorded into the project's spec thread and opens it.
/// Returns the new thread's identifier, or `None` when nothing was pending.
pub fn start_from_pending(project: &mut ProjectItem, defaults: &InterviewDefaults) -> Option<u32> {
    let pending = project.pending_interview.take()?;
    let identifier = project
        .threads
        .iter()
        .map(|thread| thread.id)
        .max()
        .map_or(1, |highest| highest.saturating_add(1));
    let thread = spec_thread(identifier, &project.default_branch, pending, defaults);
    project.threads.push(thread.normalize());
    project.active_thread_id = Some(identifier);
    Some(identifier)
}

fn spec_thread(
    identifier: u32,
    branch: &str,
    pending: PendingInterview,
    defaults: &InterviewDefaults,
) -> ThreadItem {
    ThreadItem {
        state: ThreadState::Spec,
        activity: AgentActivity::Thinking,
        // F3: the interview works in the main checkout, so the header shows its branch.
        branch_name: branch.into(),
        worktree_path: None,
        // No worktree at all (F3); the role only matters once a path exists.
        worktree_role: WorktreeRole::default(),
        worktree_path_id: None,
        pull_request_number: None,
        follow_through: None,
        review_stage: None,
        task_ids: Vec::new(),
        additions: 0,
        deletions: 0,
        elapsed_seconds: 0,
        provider: defaults.provider,
        model_name: pending
            .model_name
            .unwrap_or_else(|| defaults.model_name.clone()),
        effort: pending.effort,
        preferred_speed: pending.preferred_speed,
        effective_speed: SpeedMode::Regular,
        model_supports_fast: false,
        interview: Some(InterviewRecord {
            stage: InterviewStage::Scan,
            brief: pending.brief,
            summary: String::new(),
            question: None,
            decisions: Vec::new(),
            proof_list: Vec::new(),
            notice: None,
        }),
        ..ThreadItem::new(identifier, SPEC_THREAD_TITLE.into())
    }
}

pub fn interview_thread(project: &mut ProjectItem, thread: u32) -> Option<&mut ThreadItem> {
    project
        .threads
        .iter_mut()
        .find(|item| item.id == thread && item.interview.is_some())
}

fn record_of(thread: &mut ThreadItem) -> Option<&mut InterviewRecord> {
    thread.interview.as_mut()
}

/// A reply finished: draft its tasks, then show its question or the Ready card.
pub fn apply_reply(project: &mut ProjectItem, thread: u32, envelope: Option<Envelope>) {
    let tasks = envelope
        .as_ref()
        .map(|envelope| envelope.tasks.clone())
        .unwrap_or_default();
    add_draft_tasks(project, tasks);
    let Some(item) = interview_thread(project, thread) else {
        return;
    };
    item.activity = AgentActivity::Idle;
    let Some(record) = record_of(item) else {
        return;
    };
    record.notice = None;
    match envelope {
        Some(envelope) => apply_envelope(record, envelope),
        None => ask_in_own_words(record),
    }
}

fn apply_envelope(record: &mut InterviewRecord, envelope: Envelope) {
    if record.summary.is_empty() && !envelope.summary.trim().is_empty() {
        record.summary = envelope.summary.trim().to_owned();
    }
    if let Some(question) = envelope.question {
        record.question = Some(question.into_question());
        record.stage = InterviewStage::Ask;
    } else if let Some(ready) = envelope.ready {
        record.question = None;
        record.proof_list = ready
            .proof_list
            .into_iter()
            .filter(|feature| !feature.trim().is_empty())
            .map(|feature| ProofListEntry {
                feature: feature.trim().to_owned(),
                included: true,
            })
            .collect();
        record.stage = InterviewStage::Ready;
    } else {
        ask_in_own_words(record);
    }
}

fn ask_in_own_words(record: &mut InterviewRecord) {
    if record.stage == InterviewStage::Scan {
        record.stage = InterviewStage::Ask;
    }
    if record.stage == InterviewStage::Ask && record.question.is_none() {
        record.notice = Some(NO_QUESTION_NOTICE.into());
    }
}

/// Spec tasks are drafts until the spec locks (F3, F8).
pub fn add_draft_tasks(project: &mut ProjectItem, tasks: Vec<String>) {
    for text in tasks {
        let text = text.trim().to_owned();
        if text.is_empty() || project.tasks.iter().any(|task| task.text == text) {
            continue;
        }
        let Some(identifier) = project.next_task_identifier() else {
            return;
        };
        project.tasks.push(TaskItem::new(
            identifier,
            text,
            TaskState::Draft,
            TaskAttribution::Spec,
        ));
    }
}

/// Records your answer to the card's question as a decision. `None` when there is no
/// question waiting, or the option number has no option.
pub fn record_answer(
    project: &mut ProjectItem,
    thread: u32,
    answer: Answer,
) -> Option<RecordedAnswer> {
    let item = interview_thread(project, thread)?;
    let record = item.interview.as_mut()?;
    if record.stage != InterviewStage::Ask {
        return None;
    }
    let question = record.question.as_ref()?;
    let value = match answer {
        Answer::Option(number) => question.options.get(number.checked_sub(1)?)?.label.clone(),
        Answer::Text(text) => text.trim().to_owned(),
    };
    let recorded = RecordedAnswer {
        question: question.text.clone(),
        key: question.key.clone(),
        value,
    };
    record.decisions.push(InterviewDecision {
        question: recorded.question.clone(),
        key: recorded.key.clone(),
        value: recorded.value.clone(),
    });
    record.question = None;
    record.notice = None;
    item.activity = AgentActivity::Thinking;
    Some(recorded)
}

/// A message went to the agent: it is working again, and a failed interview resumes.
pub fn mark_working(project: &mut ProjectItem, thread: u32) {
    if let Some(item) = interview_thread(project, thread) {
        item.activity = AgentActivity::Thinking;
        if item.state == ThreadState::Error {
            item.state = ThreadState::Spec;
        }
        if let Some(record) = item.interview.as_mut() {
            record.notice = None;
        }
    }
}

/// Keep grilling (`G`): back to asking, with the next question to come.
pub fn begin_grill(project: &mut ProjectItem, thread: u32) -> bool {
    let Some(item) = interview_thread(project, thread) else {
        return false;
    };
    let Some(record) = item.interview.as_mut() else {
        return false;
    };
    if record.stage != InterviewStage::Ready {
        return false;
    }
    record.stage = InterviewStage::Ask;
    record.question = None;
    record.notice = None;
    item.activity = AgentActivity::Thinking;
    true
}

pub fn toggle_proof_entry(project: &mut ProjectItem, thread: u32, index: usize) {
    if let Some(entry) = interview_thread(project, thread)
        .and_then(|item| item.interview.as_mut())
        .filter(|record| record.stage == InterviewStage::Ready)
        .and_then(|record| record.proof_list.get_mut(index))
    {
        entry.included = !entry.included;
    }
}

/// The features left on in the Ready card's proof list.
pub fn included_proof_list(record: &InterviewRecord) -> Vec<String> {
    record
        .proof_list
        .iter()
        .filter(|entry| entry.included)
        .map(|entry| entry.feature.clone())
        .collect()
}

/// Lock spec & publish tasks (`L`), after SPEC.md is committed: the project is unlocked
/// for build work (Rule 2), drafts become tasks and the thread is done.
pub fn lock(project: &mut ProjectItem, thread: u32) {
    if let Some(item) = interview_thread(project, thread) {
        item.state = ThreadState::Done;
        item.activity = AgentActivity::Idle;
        if let Some(record) = item.interview.as_mut() {
            record.stage = InterviewStage::Locked;
            record.question = None;
            record.notice = None;
        }
    }
    project.spec_locked = true;
    for task in &mut project.tasks {
        if task.state == TaskState::Draft {
            task.state = TaskState::Todo;
        }
    }
}

/// The agent stopped on an error. The thread keeps its decisions; your next message
/// restarts the agent (§2.1 `error`).
pub fn fail(project: &mut ProjectItem, thread: u32, message: String) {
    if let Some(item) = interview_thread(project, thread) {
        item.state = ThreadState::Error;
        item.activity = AgentActivity::Idle;
        if let Some(record) = item.interview.as_mut() {
            record.notice = Some(message);
        }
    }
}

/// Shows why an action did not go through, without changing anything else.
pub fn set_notice(project: &mut ProjectItem, thread: u32, message: String) {
    if let Some(record) = interview_thread(project, thread).and_then(|item| item.interview.as_mut())
    {
        record.notice = Some(message);
    }
}
