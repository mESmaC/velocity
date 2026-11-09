//! Pure display policies for SPEC.md F1. Text and native effects stay at callers.

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadGroup {
    Draft,
    Duty,
    Swarm,
    Spec,
    Working,
    Review,
    Error,
    Closed,
    Done,
    Merged,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy)]
pub struct LayoutRow {
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub identifier: usize,
    pub group: ThreadGroup,
    pub hidden: bool,
    pub matches_search: bool,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number::optional"))]
    #[cfg_attr(feature = "bindings", specta(type = Option<String>))]
    pub member_of: Option<usize>,
    pub expanded: bool,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub agent_count: usize,
    pub paused: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PulseKind {
    Duty,
    Spec,
    Running,
    Watching,
    Failed,
    MergedToday,
}

pub fn group_index(group: ThreadGroup) -> usize {
    match group {
        ThreadGroup::Draft => 0,
        ThreadGroup::Duty => 1,
        ThreadGroup::Swarm => 2,
        ThreadGroup::Spec => 3,
        ThreadGroup::Working => 4,
        ThreadGroup::Review => 5,
        ThreadGroup::Error => 6,
        ThreadGroup::Closed => 7,
        ThreadGroup::Done => 8,
        ThreadGroup::Merged => 9,
    }
}

pub fn is_root_visible(group: ThreadGroup, row: LayoutRow) -> bool {
    row.group == group && !row.hidden && row.matches_search && row.member_of.is_none()
}

pub fn is_member_visible(parent: usize, row: LayoutRow) -> bool {
    match row.member_of {
        Some(identifier) => identifier == parent && !row.hidden,
        None => false,
    }
}

/// Member visibility follows navigation, including restored and palette selections.
pub fn expand_members(row: LayoutRow, selected: Option<LayoutRow>) -> bool {
    !row.hidden
        && matches!(row.group, ThreadGroup::Swarm)
        && selected.is_some_and(|selected| {
            !selected.hidden
                && (selected.identifier == row.identifier
                    || selected.member_of == Some(row.identifier))
        })
}

pub fn show_group(group: ThreadGroup, has_rows: bool, search_empty: bool) -> bool {
    has_rows || (matches!(group, ThreadGroup::Duty) && search_empty)
}

pub fn pulse_contribution(kind: PulseKind, row: LayoutRow) -> usize {
    match kind {
        PulseKind::Duty => {
            if matches!(row.group, ThreadGroup::Duty) && !row.paused {
                row.agent_count
            } else {
                0
            }
        }
        PulseKind::Spec => usize::from(matches!(row.group, ThreadGroup::Spec)),
        PulseKind::Running => usize::from(matches!(row.group, ThreadGroup::Working)),
        PulseKind::Watching => usize::from(matches!(row.group, ThreadGroup::Review)),
        PulseKind::Failed => usize::from(matches!(row.group, ThreadGroup::Error)),
        PulseKind::MergedToday => 0,
    }
}

/// Clamp before incrementing so even a stale selection cannot overflow.
pub fn bounded_selection(count: usize, selected: usize) -> usize {
    if count == 0 {
        0
    } else if selected < count {
        selected
    } else {
        count - 1
    }
}

pub fn next_selection(count: usize, selected: usize) -> usize {
    let selected = bounded_selection(count, selected);
    if count == 0 || selected == count - 1 {
        0
    } else {
        selected + 1
    }
}

pub fn previous_selection(count: usize, selected: usize) -> usize {
    let selected = bounded_selection(count, selected);
    if count == 0 {
        0
    } else if selected == 0 {
        count - 1
    } else {
        selected - 1
    }
}

pub fn alt_target(number: usize, visible: &[usize]) -> Option<usize> {
    match number {
        1..=9 if number <= visible.len() => Some(visible[number - 1]),
        _ => None,
    }
}

/// Literal substring matching, used by the sidebar thread search. The reference's
/// `renderThreads` filters with `title.includes(query)`; only the palette is fuzzy.
pub fn substring_match(query: &[char], label: &[char]) -> bool {
    query.is_empty() || label.windows(query.len()).any(|window| window == query)
}

/// Greedy literal subsequence matching, with no regex interpretation or recursion.
pub fn fuzzy_match(query: &[char], label: &[char]) -> bool {
    let mut query_index = 0;
    let mut label_index = 0;
    while label_index < label.len() {
        if query_index == query.len() {
            return true;
        }
        if query[query_index] == label[label_index] {
            query_index += 1;
        }
        label_index += 1;
    }
    query_index == query.len()
}
