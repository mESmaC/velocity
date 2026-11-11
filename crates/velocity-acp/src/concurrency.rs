//! Process-wide provider slots, shared by every role and runtime (F15).
use crate::Provider;
use std::collections::VecDeque;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use tokio::sync::Notify;

static SLOTS: Mutex<Vec<Slot>> = Mutex::new(Vec::new());
static CHANGED: LazyLock<Notify> = LazyLock::new(Notify::new);

struct Slot {
    provider: Provider,
    maximum: Option<u32>,
    active: usize,
    waiting: VecDeque<Arc<()>>,
}

fn slot(slots: &mut Vec<Slot>, provider: Provider) -> &mut Slot {
    let index = slots.iter().position(|slot| slot.provider == provider);
    let index = index.unwrap_or_else(|| {
        slots.push(Slot {
            provider,
            maximum: None,
            active: 0,
            waiting: VecDeque::new(),
        });
        slots.len() - 1
    });
    &mut slots[index]
}

/// Replaces caps without interrupting active agents. A lower cap drains naturally.
pub fn configure(limits: &[(Provider, u32)]) {
    let mut slots = SLOTS.lock().unwrap_or_else(PoisonError::into_inner);
    for entry in slots.iter_mut() {
        entry.maximum = None;
    }
    for (provider, maximum) in limits {
        slot(&mut slots, *provider).maximum = Some((*maximum).max(1));
    }
    drop(slots);
    CHANGED.notify_waiters();
}

/// Dropping a waiter cancels its queue entry; dropping a permit releases its slot.
pub struct Permit {
    provider: Provider,
    ticket: Arc<()>,
    active: bool,
}

impl Permit {
    fn try_acquire(&mut self) -> bool {
        let mut slots = SLOTS.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = slot(&mut slots, self.provider);
        if entry
            .maximum
            .is_some_and(|maximum| entry.active >= maximum as usize)
            || entry
                .waiting
                .front()
                .is_none_or(|first| !Arc::ptr_eq(first, &self.ticket))
        {
            return false;
        }
        entry.waiting.pop_front();
        entry.active += 1;
        self.active = true;
        CHANGED.notify_waiters();
        true
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut slots = SLOTS.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = slot(&mut slots, self.provider);
        if self.active {
            entry.active = entry.active.saturating_sub(1);
        } else {
            entry
                .waiting
                .retain(|ticket| !Arc::ptr_eq(ticket, &self.ticket));
        }
        drop(slots);
        CHANGED.notify_waiters();
    }
}

/// FIFO within each provider; notifications work across the roles' Tokio runtimes.
pub async fn acquire(provider: Provider, queued: impl FnOnce()) -> Permit {
    let ticket = Arc::new(());
    slot(
        &mut SLOTS.lock().unwrap_or_else(PoisonError::into_inner),
        provider,
    )
    .waiting
    .push_back(ticket.clone());
    let mut permit = Permit {
        provider,
        ticket,
        active: false,
    };
    let mut queued = Some(queued);
    loop {
        let changed = CHANGED.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if permit.try_acquire() {
            return permit;
        }
        if let Some(queued) = queued.take() {
            queued();
        }
        changed.await;
    }
}
