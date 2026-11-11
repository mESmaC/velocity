//! The interview's live transcript: ACP `session/update` notifications become the thread
//! view's blocks (§3.2 Transcript, F4). It lives in memory only; `state.json` keeps the
//! interview's decisions, not its stream.

use agent_client_protocol::v1::schema::{
    ContentBlock, SessionUpdate, ToolCallStatus, ToolCallUpdateFields, ToolKind,
};
use velocity_acp::Placement;
use velocity_core::thread_view_model::{ToolStepRow, TranscriptBlock};

use crate::envelope::without_envelopes;

/// What one ACP transcript entry shows.
enum EntryContent {
    /// User prompts carry Velocity's instructions; the view shows the brief and decisions
    /// instead. Thoughts and plans are not shown either.
    Hidden,
    AgentText(String),
    Tool(ToolStep),
}

struct ToolStep {
    verb: &'static str,
    title: String,
    status: ToolCallStatus,
}

/// Where a block sits: an ACP entry by index, or one Velocity added (brief, decisions).
enum Slot {
    Entry(usize),
    Added(TranscriptBlock),
}

#[derive(Default)]
pub struct LiveTranscript {
    slots: Vec<Slot>,
    entries: Vec<EntryContent>,
    turn_text: String,
    busy: bool,
}

impl LiveTranscript {
    pub fn push(&mut self, block: TranscriptBlock) {
        self.slots.push(Slot::Added(block));
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    /// A prompt went out: what the agent writes from here is this turn's reply.
    pub fn begin_turn(&mut self) {
        self.turn_text.clear();
        self.busy = true;
    }

    /// The turn ended: the full text of its reply, where the interview block is.
    pub fn end_turn(&mut self) -> String {
        self.busy = false;
        std::mem::take(&mut self.turn_text)
    }

    /// Records one update where the ACP client's transcript put it.
    pub fn record(&mut self, placement: Placement, update: &SessionUpdate) {
        if let SessionUpdate::AgentMessageChunk(chunk) = update
            && let ContentBlock::Text(text) = &chunk.content
        {
            self.turn_text.push_str(&text.text);
        }
        match placement {
            Placement::Added(index) => {
                // Entries arrive in order, so index == entries.len(); pad defensively.
                while self.entries.len() < index {
                    self.entries.push(EntryContent::Hidden);
                }
                self.entries.push(new_entry(update));
                self.slots.push(Slot::Entry(index));
            }
            Placement::Appended(index) => {
                if let Some(entry) = self.entries.get_mut(index) {
                    merge_entry(entry, update);
                }
            }
        }
    }

    /// The blocks to show, with neighbouring tool steps grouped as in the reference.
    pub fn blocks(&self) -> Vec<TranscriptBlock> {
        let mut blocks: Vec<TranscriptBlock> = Vec::new();
        for slot in &self.slots {
            let block = match slot {
                Slot::Added(block) => Some(block.clone()),
                Slot::Entry(index) => self.entries.get(*index).and_then(entry_block),
            };
            match (block, blocks.last_mut()) {
                (
                    Some(TranscriptBlock::ToolGroup { steps }),
                    Some(TranscriptBlock::ToolGroup { steps: previous }),
                ) => previous.extend(steps),
                (Some(block), _) => blocks.push(block),
                (None, _) => {}
            }
        }
        blocks
    }
}

fn new_entry(update: &SessionUpdate) -> EntryContent {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            ContentBlock::Text(text) => EntryContent::AgentText(text.text.clone()),
            _ => EntryContent::AgentText(String::new()),
        },
        SessionUpdate::ToolCall(call) => EntryContent::Tool(ToolStep {
            verb: verb(call.kind),
            title: call.title.clone(),
            status: call.status,
        }),
        SessionUpdate::ToolCallUpdate(update) => {
            let mut step = ToolStep {
                verb: verb(ToolKind::Other),
                title: String::new(),
                status: ToolCallStatus::InProgress,
            };
            apply_fields(&mut step, &update.fields);
            EntryContent::Tool(step)
        }
        _ => EntryContent::Hidden,
    }
}

fn merge_entry(entry: &mut EntryContent, update: &SessionUpdate) {
    match (entry, update) {
        (EntryContent::AgentText(text), SessionUpdate::AgentMessageChunk(chunk)) => {
            if let ContentBlock::Text(more) = &chunk.content {
                text.push_str(&more.text);
            }
        }
        (EntryContent::Tool(step), SessionUpdate::ToolCallUpdate(update)) => {
            apply_fields(step, &update.fields);
        }
        _ => {}
    }
}

fn apply_fields(step: &mut ToolStep, fields: &ToolCallUpdateFields) {
    if let Some(kind) = fields.kind {
        step.verb = verb(kind);
    }
    if let Some(title) = &fields.title {
        step.title.clone_from(title);
    }
    if let Some(status) = fields.status {
        step.status = status;
    }
}

fn entry_block(entry: &EntryContent) -> Option<TranscriptBlock> {
    match entry {
        EntryContent::Hidden => None,
        EntryContent::AgentText(text) => {
            let shown = without_envelopes(text);
            let paragraphs: Vec<String> = shown
                .split("\n\n")
                .map(str::trim)
                .filter(|paragraph| !paragraph.is_empty())
                .map(str::to_owned)
                .collect();
            (!paragraphs.is_empty()).then_some(TranscriptBlock::AgentMessage { paragraphs })
        }
        EntryContent::Tool(step) => Some(TranscriptBlock::ToolGroup {
            steps: vec![tool_row(step)],
        }),
    }
}

/// One tool step; a skill load reads "Skill · <name>", as in a thread (F4, F18).
fn tool_row(step: &ToolStep) -> ToolStepRow {
    let skill = velocity_core::skill_menu::loaded_skill(&step.title, None, None);
    ToolStepRow {
        running: matches!(
            step.status,
            ToolCallStatus::Pending | ToolCallStatus::InProgress
        ),
        verb: if skill.is_some() {
            "Skill".into()
        } else {
            step.verb.into()
        },
        argument: skill.unwrap_or_else(|| step.title.clone()),
        result_text: match step.status {
            ToolCallStatus::Failed => "failed".into(),
            _ => String::new(),
        },
    }
}

fn verb(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "Read",
        ToolKind::Edit => "Edit",
        ToolKind::Delete => "Delete",
        ToolKind::Move => "Move",
        ToolKind::Search => "Search",
        ToolKind::Execute => "Run",
        ToolKind::Think => "Think",
        ToolKind::Fetch => "Fetch",
        ToolKind::SwitchMode => "Mode",
        _ => "Tool",
    }
}
