//! A running thread's transcript as the thread view shows it (SPEC.md §3.2, F4, F5).
//!
//! The ACP client files every `session/update` under a transcript entry and reports where
//! ([`Placement`]). This mirrors those entries and folds them incrementally into the thread
//! view's blocks: messages, tool steps (consecutive calls share one group, as in the
//! reference) and the diffs an edit carries. Restored history stays in an immutable
//! `Arc<[TranscriptBlock]>` and live blocks update in place so streaming chunks never copy
//! the whole transcript (#553).

use std::path::Path;
use std::sync::Arc;

use agent_client_protocol::v1::schema::{
    ContentBlock, ContentChunk, Diff, SessionUpdate, ToolCall, ToolCallContent, ToolCallStatus,
    ToolCallUpdateFields, ToolKind,
};
use similar::TextDiff;
use velocity_acp::Placement;
use velocity_core::skill_menu;
use velocity_core::thread_view_model::{
    DiffBlockView, ErrorCardView, SharedBlocks, ToolStepRow, TranscriptBlock, TranscriptPatch,
    build_diff_block,
};

/// Lines of unchanged context around each change in a diff, as `git diff` shows.
const DIFF_CONTEXT_LINES: usize = 3;

/// Bounds buffered text deltas on unselected threads before collapsing to a tail marker.
const MAX_PENDING_TEXT_DELTA_BYTES: usize = 64 * 1024;

/// Maximum individual block updates tracked before collapsing to a single tail patch.
const MAX_PENDING_INDIVIDUAL_UPDATES: usize = 8;

#[derive(Clone, Debug)]
struct ToolView {
    call_id: Option<String>,
    title: String,
    name: Option<String>,
    kind: ToolKind,
    status: ToolCallStatus,
    location: Option<String>,
    diffs: Vec<DiffBlockView>,
    /// The skill this step loads, shown as "Skill · <name>" (F4, F18).
    skill: Option<String>,
}

#[derive(Clone, Debug)]
enum EntryView {
    Setup(ToolStepRow),
    User(String),
    /// A line Velocity delivered to the agent (#231), not something you typed.
    Notice(String),
    Agent(String),
    Tool(Box<ToolView>),
    Error(ErrorCardView),
    /// Thoughts and plans: kept so ACP's indices still line up, but not shown.
    Hidden,
}

/// Where one [`EntryView`] sits inside [`LiveTranscript::live_blocks`].
#[derive(Clone, Copy, Debug)]
enum EntryLocation {
    None,
    Block(usize),
    Tool {
        group_block: usize,
        step_index: usize,
        diff_start: usize,
        diff_count: usize,
    },
}

/// Compact summary of mutations to `live_blocks` + `queued` since the last delta drain.
#[derive(Clone, Debug, Default)]
enum PendingDelta {
    #[default]
    None,
    AppendAgentText {
        index: usize,
        delta: String,
    },
    Updated(Vec<usize>),
    ReplaceTail {
        start: usize,
    },
}

#[derive(Clone, Debug)]
pub struct LiveTranscript {
    worktree: String,
    base_branch: String,
    /// Restored blocks from earlier sessions; immutable once loaded, never cloned per chunk.
    history: Arc<[TranscriptBlock]>,
    history_lines: (usize, usize),
    entries: Vec<EntryView>,
    /// Incrementally maintained blocks for `entries` (excluding `history` and `queued`).
    live_blocks: Vec<TranscriptBlock>,
    entry_locations: Vec<EntryLocation>,
    entries_lines: (usize, usize),
    running_tools: usize,
    /// `acp_entries[i]` is where ACP transcript entry `i` sits in `entries`.
    acp_entries: Vec<usize>,
    queued: Vec<String>,
    revision: u64,
    cached_full: Option<(u64, SharedBlocks)>,
    pending_delta: PendingDelta,
}

impl LiveTranscript {
    /// A thread whose worktree is being created and set up (F5).
    pub fn new(worktree: &str) -> Self {
        Self::on_base(worktree, "master")
    }

    /// Keep the launch's base branch while the title and setup state change (F5).
    pub fn on_base(worktree: &str, base_branch: &str) -> Self {
        let mut transcript = Self::empty(worktree, base_branch);
        transcript.push_entry(EntryView::Setup(setup_step(worktree, base_branch, true)));
        transcript.pending_delta = PendingDelta::None;
        transcript
    }

    /// A subagent's transcript (§3.2 Subagents): its own ACP entries and no setup row.
    pub fn subagent(worktree: &str) -> Self {
        Self::empty(worktree, "master")
    }

    fn empty(worktree: &str, base_branch: &str) -> Self {
        LiveTranscript {
            worktree: worktree.to_owned(),
            base_branch: base_branch.to_owned(),
            history: Arc::from([]),
            history_lines: (0, 0),
            entries: Vec::new(),
            live_blocks: Vec::new(),
            entry_locations: Vec::new(),
            entries_lines: (0, 0),
            running_tools: 0,
            acp_entries: Vec::new(),
            queued: Vec::new(),
            revision: 0,
            cached_full: None,
            pending_delta: PendingDelta::None,
        }
    }

    /// Saved blocks precede the next session's independently numbered ACP entries.
    pub fn restore(worktree: &str, mut blocks: Vec<TranscriptBlock>) -> Self {
        let mut transcript = Self::empty(worktree, "master");
        for block in &mut blocks {
            if let TranscriptBlock::ToolGroup { steps } = block {
                for step in steps {
                    step.running = false;
                }
            }
            if let TranscriptBlock::UserMessage { text, queued: true } = block {
                transcript.queued.push(text.clone());
            }
        }
        blocks.retain(|block| !matches!(block, TranscriptBlock::UserMessage { queued: true, .. }));
        transcript.history_lines = count_block_lines(&blocks);
        transcript.history = Arc::from(blocks);
        transcript
    }

    /// A duty agent uses the same view for successive work items with independent ACP indices.
    pub(crate) fn next_work_session(workspace: &Path, previous: Option<&Self>) -> Self {
        let mut blocks = previous.map_or_else(Vec::new, Self::blocks);
        if previous.is_some() {
            blocks.push(velocity_core::thread_view_model::work_session_boundary());
        }
        Self::restore(&workspace.display().to_string(), blocks)
    }

    /// The worktree took the title's name while setup was still running (F4).
    pub fn rename_worktree(&mut self, worktree: &str) {
        worktree.clone_into(&mut self.worktree);
        let step = setup_step(worktree, &self.base_branch, true);
        self.update_setup_entry(step);
    }

    /// The worktree is ready and the agent may start (F5).
    pub fn worktree_ready(&mut self) {
        let step = setup_step(&self.worktree, &self.base_branch, false);
        self.update_setup_entry(step);
    }

    /// Says what a workspace still being made waits on (#514), or clears it with `None`.
    /// A finished setup step is left alone. Returns whether the line changed.
    pub fn setup_waiting(&mut self, reason: Option<&str>) -> bool {
        let creating = setup_step(&self.worktree, &self.base_branch, true).result_text;
        let Some(EntryView::Setup(step)) = self.entries.first_mut() else {
            return false;
        };
        if !step.running {
            return false;
        }
        let text = match reason {
            Some(reason) => format!("{creating} · {reason}"),
            None => creating,
        };
        if step.result_text == text {
            return false;
        }
        step.result_text = text;
        let updated = step.clone();
        self.sync_setup_block(updated);
        true
    }

    /// A Rosen workspace took the worktree's place (F5): the first step names its change.
    pub fn rosen_workspace_ready(&mut self, change: &str) {
        let mut step = setup_step(&self.worktree, &self.base_branch, false);
        step.verb = "Workspace".into();
        let short: String = change.chars().take(8).collect();
        step.result_text = format!("Rosen change {short} from {}", self.base_branch);
        self.update_setup_entry(step);
    }

    fn update_setup_entry(&mut self, updated: ToolStepRow) {
        if let Some(EntryView::Setup(step)) = self.entries.first_mut() {
            *step = updated.clone();
            self.sync_setup_block(updated);
        }
    }

    fn sync_setup_block(&mut self, updated: ToolStepRow) {
        if let Some(EntryLocation::Tool {
            group_block,
            step_index,
            ..
        }) = self.entry_locations.first().copied()
            && let Some(TranscriptBlock::ToolGroup { steps }) =
                self.live_blocks.get_mut(group_block)
            && let Some(slot) = steps.get_mut(step_index)
        {
            *slot = updated;
            self.mark_updated(group_block);
            self.bump_revision();
        }
    }

    pub fn handoff_boundary(&mut self) {
        self.push_entry(EntryView::Notice(crate::parking::HANDOFF_BOUNDARY.into()));
    }

    pub fn record_error(&mut self, headline: &str, detail: &str) {
        self.push_entry(EntryView::Error(ErrorCardView {
            headline: headline.to_owned(),
            detail: detail.to_owned(),
        }));
    }

    /// A parked thread's next session (§2.2): its ACP entries are numbered from zero again,
    /// while everything shown so far stays.
    pub fn new_session(&mut self) {
        self.acp_entries.clear();
        self.worktree_ready();
    }

    /// Messages still waiting for the agent, oldest first.
    pub fn queued(&self) -> &[String] {
        &self.queued
    }

    /// A message waiting for the agent's current turn to end.
    pub fn queue(&mut self, message: &str) {
        self.queued.push(message.to_owned());
        self.mark_replace_tail(self.live_blocks.len());
        self.bump_revision();
    }

    /// Withdraws a message you queued before the agent's turn ended. The first `sent`
    /// messages already went to the agent and can no longer be withdrawn.
    pub fn remove_queued(&mut self, index: usize, sent: usize) -> bool {
        if index < sent || index >= self.queued.len() {
            return false;
        }
        self.queued.remove(index);
        self.mark_replace_tail(self.live_blocks.len());
        self.bump_revision();
        true
    }

    /// A prompt chunk that carried a Velocity delivery rather than your message. It gets
    /// its own row even when ACP files it under the same entry as your message.
    pub fn record_notice(&mut self, placement: Placement, update: &SessionUpdate) {
        let SessionUpdate::UserMessageChunk(chunk) = update else {
            return self.record(placement, update);
        };
        if let Placement::Added(_) = placement {
            self.acp_entries.push(self.entries.len());
        }
        self.push_entry(EntryView::Notice(chunk_text(chunk)));
    }

    /// Files one update where the ACP client placed it.
    pub fn record(&mut self, placement: Placement, update: &SessionUpdate) {
        if velocity_acp::client::is_queued_run_notice(update) {
            return self.record_notice(placement, update);
        }
        match placement {
            Placement::Added(_) => {
                // The client adds entries in order, so the next ACP index is the next one here.
                let entry = new_entry(update, &self.worktree);
                self.acp_entries.push(self.entries.len());
                self.push_entry(entry);
            }
            Placement::Appended(index) => self.record_appended(index, update),
        }
    }

    fn record_appended(&mut self, acp_index: usize, update: &SessionUpdate) {
        let Some(position) = self.acp_entries.get(acp_index).copied() else {
            return;
        };
        // Your message after a notice in one prompt starts its own row.
        if let (Some(EntryView::Notice(_)), SessionUpdate::UserMessageChunk(chunk)) =
            (self.entries.get(position), update)
        {
            self.acp_entries[acp_index] = self.entries.len();
            self.push_entry(EntryView::User(chunk_text(chunk)));
            return;
        }
        match (self.entries.get_mut(position), update) {
            (Some(EntryView::Agent(_)), SessionUpdate::AgentMessageChunk(chunk)) => {
                self.append_agent_chunk(position, &chunk_text(chunk));
            }
            (Some(EntryView::User(_)), SessionUpdate::UserMessageChunk(chunk)) => {
                self.append_user_chunk(position, &chunk_text(chunk));
            }
            (
                Some(EntryView::Tool(_)),
                SessionUpdate::ToolCall(_) | SessionUpdate::ToolCallUpdate(_),
            ) => {
                self.update_tool_entry(position, update);
            }
            _ => {}
        }
    }

    fn append_agent_chunk(&mut self, position: usize, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        let Some(EntryView::Agent(text)) = self.entries.get_mut(position) else {
            return;
        };
        text.push_str(chunk);
        match self.entry_locations.get(position).copied() {
            Some(EntryLocation::Block(block_index)) => {
                if let Some(TranscriptBlock::AgentMessage { paragraphs }) =
                    self.live_blocks.get_mut(block_index)
                {
                    if let Some(last) = paragraphs.last_mut() {
                        last.push_str(chunk);
                    } else {
                        paragraphs.push(chunk.to_owned());
                    }
                    self.mark_append_agent_text(block_index, chunk);
                    self.bump_revision();
                }
            }
            Some(EntryLocation::None) if !text.trim().is_empty() => {
                let block = TranscriptBlock::AgentMessage {
                    paragraphs: vec![text.clone()],
                };
                if position + 1 == self.entries.len() {
                    let block_index = self.live_blocks.len();
                    self.live_blocks.push(block);
                    self.entry_locations[position] = EntryLocation::Block(block_index);
                    self.mark_replace_tail(block_index);
                    self.bump_revision();
                } else {
                    self.rebuild_live_blocks_from(position);
                }
            }
            _ => {}
        }
    }

    fn append_user_chunk(&mut self, position: usize, chunk: &str) {
        let Some(EntryView::User(text)) = self.entries.get_mut(position) else {
            return;
        };
        text.push_str("\n\n");
        text.push_str(chunk);
        if let Some(EntryLocation::Block(block_index)) = self.entry_locations.get(position).copied()
            && let Some(TranscriptBlock::UserMessage {
                text: block_text, ..
            }) = self.live_blocks.get_mut(block_index)
        {
            block_text.push_str("\n\n");
            block_text.push_str(chunk);
            self.mark_updated(block_index);
            self.bump_revision();
        }
    }

    fn update_tool_entry(&mut self, position: usize, update: &SessionUpdate) {
        let worktree = self.worktree.clone();
        let Some(entry) = self.entries.get_mut(position) else {
            return;
        };
        let EntryView::Tool(tool) = entry else {
            return;
        };
        let old_running = is_running(tool.status);
        let old_lines = tool_lines(tool);
        let old_diffs = tool.diffs.len();
        merge(entry, update, &worktree);
        let EntryView::Tool(tool) = entry else {
            return;
        };
        let new_running = is_running(tool.status);
        let new_lines = tool_lines(tool);
        let new_diffs = tool.diffs.len();
        let step = tool_step(tool);
        let updated_diffs = (new_diffs == old_diffs).then(|| tool.diffs.clone());

        self.adjust_running_tools(old_running, new_running);
        self.entries_lines = (
            self.entries_lines.0.saturating_sub(old_lines.0) + new_lines.0,
            self.entries_lines.1.saturating_sub(old_lines.1) + new_lines.1,
        );

        if let (
            Some(EntryLocation::Tool {
                group_block,
                step_index,
                diff_start,
                diff_count,
            }),
            Some(diffs),
        ) = (self.entry_locations.get(position).copied(), updated_diffs)
            && diff_count == diffs.len()
        {
            if let Some(TranscriptBlock::ToolGroup { steps }) =
                self.live_blocks.get_mut(group_block)
                && let Some(slot) = steps.get_mut(step_index)
            {
                *slot = step;
                self.mark_updated(group_block);
            }
            for (offset, diff) in diffs.into_iter().enumerate() {
                let target = diff_start + offset;
                if let Some(slot) = self.live_blocks.get_mut(target) {
                    *slot = TranscriptBlock::Diff { block: diff };
                    self.mark_updated(target);
                }
            }
            self.bump_revision();
        } else {
            self.rebuild_live_blocks_from(position);
        }
    }

    fn push_entry(&mut self, entry: EntryView) {
        if let EntryView::Tool(tool) = &entry {
            if is_running(tool.status) {
                self.running_tools += 1;
            }
            let (added, deleted) = tool_lines(tool);
            self.entries_lines.0 += added;
            self.entries_lines.1 += deleted;
        }
        let location = self.append_entry_block(&entry);
        self.entries.push(entry);
        self.entry_locations.push(location);
        self.bump_revision();
    }

    fn append_entry_block(&mut self, entry: &EntryView) -> EntryLocation {
        match entry {
            EntryView::Setup(step) => self.push_tool_blocks(step.clone(), &[]),
            EntryView::Tool(tool) => self.push_tool_blocks(tool_step(tool), &tool.diffs),
            EntryView::Hidden => EntryLocation::None,
            other => {
                let Some(block) = message_block(other) else {
                    return EntryLocation::None;
                };
                let block_index = self.live_blocks.len();
                self.live_blocks.push(block);
                self.mark_replace_tail(block_index);
                EntryLocation::Block(block_index)
            }
        }
    }

    fn push_tool_blocks(&mut self, step: ToolStepRow, diffs: &[DiffBlockView]) -> EntryLocation {
        let (group_block, step_index, pushed_group) = match self.live_blocks.last_mut() {
            Some(TranscriptBlock::ToolGroup { steps }) => {
                let step_index = steps.len();
                steps.push(step);
                (self.live_blocks.len() - 1, step_index, false)
            }
            _ => {
                let group_block = self.live_blocks.len();
                self.live_blocks
                    .push(TranscriptBlock::ToolGroup { steps: vec![step] });
                (group_block, 0, true)
            }
        };
        let diff_start = self.live_blocks.len();
        for diff in diffs {
            self.live_blocks.push(TranscriptBlock::Diff {
                block: diff.clone(),
            });
        }
        if pushed_group || !diffs.is_empty() || !self.queued.is_empty() {
            self.mark_replace_tail(group_block);
        } else {
            self.mark_updated(group_block);
        }
        EntryLocation::Tool {
            group_block,
            step_index,
            diff_start,
            diff_count: diffs.len(),
        }
    }

    /// Rebuilds only the suffix of `live_blocks` starting from the block containing `position`.
    fn rebuild_live_blocks_from(&mut self, position: usize) {
        let start_block = self.block_start_for_entry(position);
        let first_entry = self
            .entry_locations
            .iter()
            .position(|location| location_block_index(*location) >= Some(start_block))
            .unwrap_or(position)
            .min(position);
        self.live_blocks.truncate(start_block);
        for index in first_entry..self.entries.len() {
            let location = match &self.entries[index] {
                EntryView::Setup(step) => {
                    push_tool_blocks_raw(&mut self.live_blocks, step.clone(), &[])
                }
                EntryView::Tool(tool) => {
                    push_tool_blocks_raw(&mut self.live_blocks, tool_step(tool), &tool.diffs)
                }
                EntryView::Hidden => EntryLocation::None,
                other => match message_block(other) {
                    Some(block) => {
                        let block_index = self.live_blocks.len();
                        self.live_blocks.push(block);
                        EntryLocation::Block(block_index)
                    }
                    None => EntryLocation::None,
                },
            };
            self.entry_locations[index] = location;
        }
        self.mark_replace_tail(start_block);
        self.bump_revision();
    }

    fn block_start_for_entry(&self, position: usize) -> usize {
        if let Some(index) = self
            .entry_locations
            .get(position)
            .copied()
            .and_then(location_block_index)
        {
            return index;
        }
        self.entry_locations[position..]
            .iter()
            .copied()
            .find_map(location_block_index)
            .unwrap_or(self.live_blocks.len())
    }

    fn adjust_running_tools(&mut self, was_running: bool, now_running: bool) {
        match (was_running, now_running) {
            (false, true) => self.running_tools += 1,
            (true, false) => self.running_tools = self.running_tools.saturating_sub(1),
            _ => {}
        }
    }

    /// The oldest `count` queued messages went out as the agent's next prompt. Only those
    /// leave the queue: a message queued after them still waits for the following turn.
    pub fn sent_queued(&mut self, count: usize) {
        let removed = count.min(self.queued.len());
        if removed == 0 {
            return;
        }
        self.queued.drain(..removed);
        self.mark_replace_tail(self.live_blocks.len());
        self.bump_revision();
    }

    /// Whether a tool call is pending or running: activity `executing` (§2.2).
    pub fn has_running_tool(&self) -> bool {
        self.running_tools > 0
    }

    /// Marks tool calls left running by the agent as finished when its turn or session
    /// ends (#544). Real Velocity runs (`queue_run`, `preflight`, `workflow_run`) keep the
    /// logical turn open until they complete, so active Velocity waits are unaffected.
    pub fn finish_running_tools(&mut self) {
        if self.running_tools == 0 {
            return;
        }
        let mut updated_groups = Vec::new();
        for (position, entry) in self.entries.iter_mut().enumerate() {
            if let EntryView::Tool(tool) = entry
                && is_running(tool.status)
            {
                tool.status = ToolCallStatus::Completed;
                if let Some(EntryLocation::Tool {
                    group_block,
                    step_index,
                    ..
                }) = self.entry_locations.get(position).copied()
                    && let Some(TranscriptBlock::ToolGroup { steps }) =
                        self.live_blocks.get_mut(group_block)
                    && let Some(step) = steps.get_mut(step_index)
                {
                    step.running = false;
                    updated_groups.push(group_block);
                }
            }
        }
        self.running_tools = 0;
        if !updated_groups.is_empty() {
            for group_block in updated_groups {
                self.mark_updated(group_block);
            }
            self.bump_revision();
        }
    }

    /// `+added −deleted` across every edit the agent made, for the thread header (F4).
    pub fn line_counts(&self) -> (usize, usize) {
        (
            self.history_lines.0 + self.entries_lines.0,
            self.history_lines.1 + self.entries_lines.1,
        )
    }

    /// The newest block without copying earlier history or live blocks (#553).
    pub fn last_block(&self) -> Option<TranscriptBlock> {
        if let Some(text) = self.queued.last() {
            return Some(TranscriptBlock::UserMessage {
                text: text.clone(),
                queued: true,
            });
        }
        if let Some(block) = self.live_blocks.last() {
            return Some(block.clone());
        }
        self.history.last().cloned()
    }

    /// Whether there are un-drained incremental transcript changes.
    pub fn has_pending_delta(&self) -> bool {
        !matches!(self.pending_delta, PendingDelta::None)
    }

    /// Clears pending delta tracking after a full snapshot has been materialized.
    pub fn clear_pending_delta(&mut self) {
        self.pending_delta = PendingDelta::None;
    }

    /// Drains pending incremental changes as ordered [`TranscriptPatch`] entries (#553).
    pub fn take_patches(&mut self) -> Vec<TranscriptPatch> {
        let base = self.history.len();
        match std::mem::take(&mut self.pending_delta) {
            PendingDelta::None => Vec::new(),
            PendingDelta::AppendAgentText { index, delta } => {
                vec![TranscriptPatch::AppendAgentText {
                    index: base + index,
                    delta,
                }]
            }
            PendingDelta::Updated(mut indices) => {
                indices.sort_unstable();
                indices.dedup();
                indices
                    .into_iter()
                    .filter_map(|index| {
                        self.live_blocks.get(index).cloned().map(|block| {
                            TranscriptPatch::UpdateBlock {
                                index: base + index,
                                block,
                            }
                        })
                    })
                    .collect()
            }
            PendingDelta::ReplaceTail { start } => {
                let start = start.min(self.live_blocks.len());
                let mut blocks =
                    Vec::with_capacity(self.live_blocks.len() - start + self.queued.len());
                blocks.extend(self.live_blocks[start..].iter().cloned());
                blocks.extend(self.queued.iter().map(|text| TranscriptBlock::UserMessage {
                    text: text.clone(),
                    queued: true,
                }));
                vec![TranscriptPatch::ReplaceTail {
                    start: base + start,
                    blocks,
                }]
            }
        }
    }

    /// Full transcript as a cached `SharedBlocks`, materialized only when needed (save or open).
    pub fn shared_blocks(&mut self) -> SharedBlocks {
        if let Some((version, cached)) = &self.cached_full
            && *version == self.revision
        {
            return Arc::clone(cached);
        }
        let shared: SharedBlocks = Arc::new(self.blocks());
        self.cached_full = Some((self.revision, Arc::clone(&shared)));
        shared
    }

    pub fn blocks(&self) -> Vec<TranscriptBlock> {
        let mut blocks =
            Vec::with_capacity(self.history.len() + self.live_blocks.len() + self.queued.len());
        blocks.extend(self.history.iter().cloned());
        blocks.extend(self.live_blocks.iter().cloned());
        blocks.extend(self.queued.iter().map(|text| TranscriptBlock::UserMessage {
            text: text.clone(),
            queued: true,
        }));
        blocks
    }

    /// Agent calls only: the worktree setup row is not a provider tool call.
    pub fn tool_steps(&self) -> Vec<ToolStepRow> {
        let saved = self
            .history
            .iter()
            .filter_map(|block| match block {
                TranscriptBlock::ToolGroup { steps } => Some(
                    steps
                        .iter()
                        // The setup row, a worktree's or a Rosen workspace's, is no tool call.
                        .filter(|step| step.verb != "Worktree" && step.verb != "Workspace")
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .flatten();
        saved
            .chain(self.entries.iter().filter_map(|entry| match entry {
                EntryView::Tool(tool) => Some(tool_step(tool)),
                _ => None,
            }))
            .collect()
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.cached_full = None;
    }

    fn mark_append_agent_text(&mut self, index: usize, chunk: &str) {
        match &mut self.pending_delta {
            PendingDelta::None => {
                if chunk.len() > MAX_PENDING_TEXT_DELTA_BYTES {
                    self.pending_delta = PendingDelta::ReplaceTail { start: index };
                } else {
                    self.pending_delta = PendingDelta::AppendAgentText {
                        index,
                        delta: chunk.to_owned(),
                    };
                }
            }
            PendingDelta::AppendAgentText {
                index: existing,
                delta,
            } if *existing == index => {
                if delta.len().saturating_add(chunk.len()) > MAX_PENDING_TEXT_DELTA_BYTES {
                    self.pending_delta = PendingDelta::ReplaceTail { start: index };
                } else {
                    delta.push_str(chunk);
                }
            }
            PendingDelta::AppendAgentText {
                index: existing, ..
            } => {
                let start = (*existing).min(index);
                self.pending_delta = PendingDelta::ReplaceTail { start };
            }
            PendingDelta::Updated(indices) => {
                let current = std::mem::take(indices);
                self.pending_delta = record_updated_index(current, index);
            }
            PendingDelta::ReplaceTail { start } => *start = (*start).min(index),
        }
    }

    fn mark_updated(&mut self, index: usize) {
        match &mut self.pending_delta {
            PendingDelta::None => {
                self.pending_delta = PendingDelta::Updated(vec![index]);
            }
            PendingDelta::AppendAgentText {
                index: existing, ..
            } => {
                let indices = if *existing == index {
                    vec![index]
                } else {
                    vec![*existing, index]
                };
                self.pending_delta = PendingDelta::Updated(indices);
            }
            PendingDelta::Updated(indices) => {
                let current = std::mem::take(indices);
                self.pending_delta = record_updated_index(current, index);
            }
            PendingDelta::ReplaceTail { start } => *start = (*start).min(index),
        }
    }

    fn mark_replace_tail(&mut self, start: usize) {
        let earliest = match &self.pending_delta {
            PendingDelta::None => start,
            PendingDelta::AppendAgentText { index, .. } => start.min(*index),
            PendingDelta::Updated(indices) => indices
                .iter()
                .copied()
                .min()
                .map_or(start, |min| start.min(min)),
            PendingDelta::ReplaceTail { start: existing } => start.min(*existing),
        };
        self.pending_delta = PendingDelta::ReplaceTail { start: earliest };
    }
}

fn push_tool_blocks_raw(
    live_blocks: &mut Vec<TranscriptBlock>,
    step: ToolStepRow,
    diffs: &[DiffBlockView],
) -> EntryLocation {
    let (group_block, step_index) = match live_blocks.last_mut() {
        Some(TranscriptBlock::ToolGroup { steps }) => {
            let step_index = steps.len();
            steps.push(step);
            (live_blocks.len() - 1, step_index)
        }
        _ => {
            let group_block = live_blocks.len();
            live_blocks.push(TranscriptBlock::ToolGroup { steps: vec![step] });
            (group_block, 0)
        }
    };
    let diff_start = live_blocks.len();
    for diff in diffs {
        live_blocks.push(TranscriptBlock::Diff {
            block: diff.clone(),
        });
    }
    EntryLocation::Tool {
        group_block,
        step_index,
        diff_start,
        diff_count: diffs.len(),
    }
}

fn record_updated_index(mut indices: Vec<usize>, index: usize) -> PendingDelta {
    if !indices.contains(&index) {
        indices.push(index);
    }
    if indices.len() > MAX_PENDING_INDIVIDUAL_UPDATES {
        let start = indices.into_iter().min().unwrap_or(index);
        PendingDelta::ReplaceTail { start }
    } else {
        PendingDelta::Updated(indices)
    }
}

fn location_block_index(location: EntryLocation) -> Option<usize> {
    match location {
        EntryLocation::None => None,
        EntryLocation::Block(index) => Some(index),
        EntryLocation::Tool { group_block, .. } => Some(group_block),
    }
}

fn count_block_lines(blocks: &[TranscriptBlock]) -> (usize, usize) {
    blocks
        .iter()
        .fold((0, 0), |(added, deleted), block| match block {
            TranscriptBlock::Diff { block } => (added + block.added, deleted + block.deleted),
            _ => (added, deleted),
        })
}

fn tool_lines(tool: &ToolView) -> (usize, usize) {
    tool.diffs.iter().fold((0, 0), |(added, deleted), diff| {
        (added + diff.added, deleted + diff.deleted)
    })
}

fn setup_step(worktree: &str, base_branch: &str, running: bool) -> ToolStepRow {
    ToolStepRow {
        running,
        verb: "Worktree".into(),
        argument: worktree.to_owned(),
        result_text: if running {
            format!("creating from {base_branch}")
        } else {
            format!("created from {base_branch}")
        },
        tool_call_id: None,
    }
}

fn message_block(entry: &EntryView) -> Option<TranscriptBlock> {
    match entry {
        EntryView::User(text) => Some(TranscriptBlock::UserMessage {
            text: text.clone(),
            queued: false,
        }),
        EntryView::Notice(text) => Some(TranscriptBlock::Notice { text: text.clone() }),
        EntryView::Agent(text) => Some(TranscriptBlock::AgentMessage {
            paragraphs: paragraphs(text),
        }),
        EntryView::Error(card) => Some(TranscriptBlock::ErrorCard { card: card.clone() }),
        EntryView::Setup(_) | EntryView::Tool(_) | EntryView::Hidden => None,
    }
}

/// Preserve markdown verbatim: trimming split paragraphs corrupts indented code
/// after blank lines inside fences and nested lists.
fn paragraphs(text: &str) -> Vec<String> {
    if text.trim().is_empty() {
        Vec::new()
    } else {
        vec![text.to_owned()]
    }
}

fn new_entry(update: &SessionUpdate, worktree: &str) -> EntryView {
    match update {
        SessionUpdate::UserMessageChunk(chunk) => EntryView::User(chunk_text(chunk)),
        SessionUpdate::AgentMessageChunk(chunk) => EntryView::Agent(chunk_text(chunk)),
        SessionUpdate::ToolCall(call) => EntryView::Tool(Box::new(tool_view(call, worktree))),
        SessionUpdate::ToolCallUpdate(update) => {
            let mut tool = ToolView {
                call_id: Some(update.tool_call_id.0.to_string()),
                title: String::new(),
                name: None,
                kind: ToolKind::Other,
                status: ToolCallStatus::Pending,
                location: None,
                diffs: Vec::new(),
                skill: None,
            };
            apply_fields(&mut tool, &update.fields, worktree);
            EntryView::Tool(Box::new(tool))
        }
        _ => EntryView::Hidden,
    }
}

fn merge(entry: &mut EntryView, update: &SessionUpdate, worktree: &str) {
    match (entry, update) {
        (EntryView::User(text), SessionUpdate::UserMessageChunk(chunk)) => {
            // Each queued message is its own chunk of one prompt.
            text.push_str("\n\n");
            text.push_str(&chunk_text(chunk));
        }
        (EntryView::Agent(text), SessionUpdate::AgentMessageChunk(chunk)) => {
            text.push_str(&chunk_text(chunk));
        }
        (EntryView::Tool(tool), SessionUpdate::ToolCall(call)) => {
            **tool = tool_view(call, worktree);
        }
        (EntryView::Tool(tool), SessionUpdate::ToolCallUpdate(update)) => {
            if tool.call_id.is_none() {
                tool.call_id = Some(update.tool_call_id.0.to_string());
            }
            apply_fields(tool, &update.fields, worktree);
        }
        _ => {}
    }
}

fn chunk_text(chunk: &ContentChunk) -> String {
    match &chunk.content {
        ContentBlock::Text(text) => text.text.clone(),
        ContentBlock::Image(_) => "[image]".into(),
        ContentBlock::ResourceLink(link) => link.uri.clone(),
        _ => String::new(),
    }
}

fn tool_view(call: &ToolCall, worktree: &str) -> ToolView {
    let first_path = call.locations.first().map(|location| &location.path);
    ToolView {
        call_id: Some(call.tool_call_id.0.to_string()),
        skill: loaded_skill(&call.title, call.raw_input.as_ref(), first_path),
        title: call.title.clone(),
        name: call.name.clone(),
        kind: call.kind,
        status: call.status,
        location: call
            .locations
            .first()
            .map(|location| display_path(&location.path, worktree)),
        diffs: diffs(&call.content, worktree),
    }
}

fn apply_fields(tool: &mut ToolView, fields: &ToolCallUpdateFields, worktree: &str) {
    if let Some(title) = &fields.title {
        tool.title.clone_from(title);
    }
    if let Some(name) = &fields.name {
        tool.name = Some(name.clone());
    }
    if let Some(kind) = fields.kind {
        tool.kind = kind;
    }
    if let Some(status) = fields.status {
        tool.status = status;
    }
    if let Some(locations) = &fields.locations {
        tool.location = locations
            .first()
            .map(|location| display_path(&location.path, worktree));
    }
    if let Some(content) = &fields.content {
        tool.diffs = diffs(content, worktree);
    }
    let first_path = fields
        .locations
        .as_ref()
        .and_then(|locations| locations.first())
        .map(|location| &location.path);
    if let Some(skill) = loaded_skill(&tool.title, fields.raw_input.as_ref(), first_path) {
        tool.skill = Some(skill);
    }
}

/// The skill a tool call loads, from its title, its raw `skill` input or the `SKILL.md` it
/// reads (F4, F18).
fn loaded_skill(
    title: &str,
    raw_input: Option<&serde_json::Value>,
    path: Option<&std::path::PathBuf>,
) -> Option<String> {
    let raw_skill = raw_input
        .and_then(|input| input.get("skill"))
        .and_then(serde_json::Value::as_str);
    let path = path.map(|path| path.to_string_lossy());
    skill_menu::loaded_skill(title, raw_skill, path.as_deref())
}

fn diffs(content: &[ToolCallContent], worktree: &str) -> Vec<DiffBlockView> {
    content
        .iter()
        .filter_map(|item| match item {
            ToolCallContent::Diff(diff) => Some(diff_block(diff, worktree)),
            _ => None,
        })
        .collect()
}

fn diff_block(diff: &Diff, worktree: &str) -> DiffBlockView {
    let old_text = diff.old_text.as_deref().unwrap_or_default();
    let unified = TextDiff::from_lines(old_text, &diff.new_text)
        .unified_diff()
        .context_radius(DIFF_CONTEXT_LINES)
        .to_string();
    let lines = unified
        .lines()
        .map(|line| line.trim_end_matches('\r').to_owned())
        .collect();
    build_diff_block(display_path(&diff.path, worktree), lines)
}

/// Paths inside the worktree show relative to it, as the reference does.
fn display_path(path: &Path, worktree: &str) -> String {
    path.strip_prefix(worktree)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn is_running(status: ToolCallStatus) -> bool {
    matches!(status, ToolCallStatus::Pending | ToolCallStatus::InProgress)
}

fn tool_step(tool: &ToolView) -> ToolStepRow {
    let (added, deleted) = tool_lines(tool);
    let result_text = if matches!(tool.status, ToolCallStatus::Failed) {
        "failed".into()
    } else if added + deleted > 0 {
        format!("+{added} −{deleted}")
    } else {
        String::new()
    };
    if let Some(skill) = &tool.skill {
        return ToolStepRow {
            running: is_running(tool.status),
            verb: "Skill".into(),
            argument: skill.clone(),
            result_text,
            tool_call_id: tool.call_id.clone(),
        };
    }
    ToolStepRow {
        running: is_running(tool.status),
        verb: tool_verb(tool),
        argument: tool.location.clone().unwrap_or_else(|| tool.title.clone()),
        result_text,
        tool_call_id: tool.call_id.clone(),
    }
}

fn tool_verb(tool: &ToolView) -> String {
    let verb = match tool.kind {
        ToolKind::Read => "Read",
        ToolKind::Edit => "Edit",
        ToolKind::Delete => "Delete",
        ToolKind::Move => "Move",
        ToolKind::Search => "Search",
        ToolKind::Execute => "Bash",
        ToolKind::Think => "Think",
        ToolKind::Fetch => "Fetch",
        ToolKind::SwitchMode => "Mode",
        _ => return tool.name.clone().unwrap_or_else(|| "Tool".into()),
    };
    verb.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::v1::schema::{ToolCallId, ToolCallUpdate};
    use velocity_core::thread_view_model::apply_transcript_patches;

    // Risk (#553): incremental transcript patches must stay identical to the full block
    // projection across restored history, streamed chunks, tool updates, diffs and queued edits.
    #[test]
    fn incremental_patches_match_full_projection() {
        let history = vec![TranscriptBlock::UserMessage {
            text: "earlier prompt".into(),
            queued: false,
        }];
        let mut transcript = LiveTranscript::restore("work", history);
        let mut synced = transcript.blocks();

        transcript.record(
            Placement::Added(0),
            &SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::from(
                "hello ".to_owned(),
            ))),
        );
        let patches = transcript.take_patches();
        assert!(apply_transcript_patches(&mut synced, &patches));
        assert_eq!(synced, transcript.blocks());

        transcript.record(
            Placement::Appended(0),
            &SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::from(
                "world".to_owned(),
            ))),
        );
        let patches = transcript.take_patches();
        assert_eq!(
            patches,
            vec![TranscriptPatch::AppendAgentText {
                index: 1,
                delta: "world".into(),
            }]
        );
        assert!(apply_transcript_patches(&mut synced, &patches));
        assert_eq!(synced, transcript.blocks());

        let call = ToolCall::new(ToolCallId::new("c1"), "edit.rs")
            .kind(ToolKind::Edit)
            .status(ToolCallStatus::InProgress);
        transcript.record(Placement::Added(1), &SessionUpdate::ToolCall(call));
        assert!(transcript.has_running_tool());
        let patches = transcript.take_patches();
        assert!(apply_transcript_patches(&mut synced, &patches));
        assert_eq!(synced, transcript.blocks());

        let mut fields = ToolCallUpdateFields::new();
        fields.status = Some(ToolCallStatus::Completed);
        fields.content = Some(vec![ToolCallContent::Diff(Diff::new(
            "work/a.rs",
            "line1\nline2\n",
        ))]);
        let update = ToolCallUpdate::new(ToolCallId::new("c1"), fields);
        transcript.record(
            Placement::Appended(1),
            &SessionUpdate::ToolCallUpdate(update),
        );
        assert!(!transcript.has_running_tool());
        assert_eq!(transcript.line_counts(), (2, 0));
        let patches = transcript.take_patches();
        assert!(apply_transcript_patches(&mut synced, &patches));
        assert_eq!(synced, transcript.blocks());

        transcript.queue("follow up");
        let patches = transcript.take_patches();
        assert!(apply_transcript_patches(&mut synced, &patches));
        assert_eq!(synced, transcript.blocks());
        assert_eq!(transcript.last_block(), synced.last().cloned());

        assert!(transcript.remove_queued(0, 0));
        let patches = transcript.take_patches();
        assert!(apply_transcript_patches(&mut synced, &patches));
        assert_eq!(synced, transcript.blocks());
    }
}
