//! The warm worktree pool's decisions (SPEC.md F5 Warm pool): which ready worktree a new
//! thread takes, and what the pool does next to stay full at the tip of `master`.
//!
//! The pool's I/O lives in `velocity-git` (checkouts, moves, prebuilds) and its workers in
//! `velocity-threads`; everything here is pure so both workers read one set of rules.

/// Where one pool slot is. A slot is a detached worktree at `<project root>\.ready-<index>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlotState {
    /// Being checked out and set up. Never taken.
    Preparing,
    /// Checked out at `base` with setup done. `built` once the prebuild at `base` finished.
    Ready { base: String, built: bool },
    /// Its prebuild at `base` is running. Never taken: moving the folder would pull it out
    /// from under the compiler.
    Building { base: String },
    /// Being moved to a thread's path. It leaves the pool when the move finishes.
    Taken,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadySlot {
    pub index: u32,
    pub state: SlotState,
}

impl ReadySlot {
    pub fn new(index: u32, state: SlotState) -> Self {
        Self { index, state }
    }

    fn ready_base(&self) -> Option<(&str, bool)> {
        match &self.state {
            SlotState::Ready { base, built } => Some((base.as_str(), *built)),
            _ => None,
        }
    }
}

/// The next job for the pool's checkout worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckoutWork {
    /// Make slot `index` at the tip.
    Create { index: u32 },
    /// Move a ready slot's detached checkout forward to the tip (cheap: a merge landed).
    CatchUp { index: u32, tip: String },
    /// The pool is larger than wanted: remove this ready slot.
    Remove { index: u32 },
}

/// Most slots a pool keeps, whatever a setting or a swarm asks for: the agent count cap of a
/// goal swarm (F16), so a launch never fills a drive with checkouts.
pub const MOST_SLOTS: u32 = 30;

/// How many slots the pool keeps: the setting, raised while a swarm launch is waiting for
/// worktrees (F5 Swarm launches).
pub fn wanted_slots(setting: u32, reserved: u32) -> u32 {
    setting.max(reserved).min(MOST_SLOTS)
}

/// What the checkout worker does next, in order: remove a ready slot while more than `wanted`
/// slots are live (highest index first), add a slot while fewer are, then catch ready slots up
/// to `tip`. A slot being taken is not live: its index stays in use until the take finishes,
/// so a new slot never lands on a folder that is still moving. `None` when the pool is full
/// and current.
pub fn checkout_work(slots: &[ReadySlot], wanted: u32, tip: Option<&str>) -> Option<CheckoutWork> {
    let live = slots
        .iter()
        .filter(|slot| slot.state != SlotState::Taken)
        .count();
    let wanted = usize::try_from(wanted).unwrap_or(usize::MAX);
    if live > wanted {
        return slots
            .iter()
            .filter(|slot| slot.ready_base().is_some())
            .max_by_key(|slot| slot.index)
            .map(|slot| CheckoutWork::Remove { index: slot.index });
    }
    let tip = tip?;
    if live < wanted {
        let index = (0..).find(|index| slots.iter().all(|slot| slot.index != *index))?;
        return Some(CheckoutWork::Create { index });
    }
    slots
        .iter()
        .find(|slot| slot.ready_base().is_some_and(|(base, _)| base != tip))
        .map(|slot| CheckoutWork::CatchUp {
            index: slot.index,
            tip: tip.to_owned(),
        })
}

/// The slot the prebuild worker builds next: the lowest ready, unbuilt slot, while fewer than
/// `standing` (the setting) are built or building. Slots a swarm adds on top are taken
/// straight away, so they skip the build rather than make agents wait behind it.
pub fn prebuild_work(slots: &[ReadySlot], standing: u32) -> Option<u32> {
    let built = slots
        .iter()
        .filter(|slot| match &slot.state {
            SlotState::Building { .. } => true,
            SlotState::Ready { built, .. } => *built,
            _ => false,
        })
        .count();
    if built >= usize::try_from(standing).unwrap_or(usize::MAX) {
        return None;
    }
    slots
        .iter()
        .filter(|slot| slot.ready_base().is_some_and(|(_, built)| !built))
        .map(|slot| slot.index)
        .min()
}

/// The ready slot a new thread takes: one at the tip first (no catch-up), then a built one
/// (only its changes since the build compile), lowest index last. `None` when no slot is
/// ready; the thread then makes its own worktree as before.
pub fn choose_ready(slots: &[ReadySlot], tip: Option<&str>) -> Option<u32> {
    slots
        .iter()
        .filter_map(|slot| slot.ready_base().map(|ready| (slot.index, ready)))
        .min_by_key(|(index, (base, built))| (Some(*base) != tip, !built, *index))
        .map(|(index, _)| index)
}

/// The folder name of slot `index`. Thread slugs never start with a dot, so a slot can't
/// take a thread's name, and the name is unique per project (F5 Guards).
pub fn slot_folder(index: u32) -> String {
    format!(".ready-{index}")
}

/// The slot index a folder name stands for, if it is a pool slot's.
pub fn slot_index(folder: &str) -> Option<u32> {
    let digits = folder.strip_prefix(".ready-")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}
