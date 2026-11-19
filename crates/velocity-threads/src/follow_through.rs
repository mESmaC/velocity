//! Delivery is acknowledged by the actual ACP prompt and its completed turn.
//! A channel accepting a message is not enough: its queued input can be lost on exit.
use std::collections::BTreeMap;

use agent_client_protocol::v1::schema::{ContentBlock, SessionUpdate, StopReason};
use velocity_acp::client::AgentEvent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryStatus {
    Pending,
    Delivered,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeliveryPhase {
    Queued,
    Prompted,
    Completed,
    Failed,
}

struct Delivery {
    message: String,
    phase: DeliveryPhase,
    wakes_agent: bool,
}

#[derive(Default)]
pub(crate) struct Deliveries {
    pending: BTreeMap<String, Delivery>,
}

impl Deliveries {
    pub fn stage(&mut self, identifier: &str, message: &str) {
        self.pending.insert(
            identifier.to_owned(),
            Delivery {
                message: message.to_owned(),
                phase: DeliveryPhase::Queued,
                wakes_agent: false,
            },
        );
    }

    pub fn status(&self, identifier: &str, message: &str) -> Option<DeliveryStatus> {
        let delivery = self.pending.get(identifier)?;
        if delivery.message != message {
            return None;
        }
        Self::phase_status(delivery)
    }

    pub fn airc_status(&self, identifier: &str) -> Option<DeliveryStatus> {
        self.pending.get(identifier).and_then(Self::phase_status)
    }

    fn phase_status(delivery: &Delivery) -> Option<DeliveryStatus> {
        match delivery.phase {
            DeliveryPhase::Queued | DeliveryPhase::Prompted => Some(DeliveryStatus::Pending),
            DeliveryPhase::Completed => Some(DeliveryStatus::Delivered),
            DeliveryPhase::Failed => None,
        }
    }

    pub fn stage_airc(&mut self, identifier: String, wakes_agent: bool) {
        self.pending.insert(
            identifier,
            Delivery {
                message: String::new(),
                phase: DeliveryPhase::Queued,
                wakes_agent,
            },
        );
    }

    pub fn airc_wake_pending(&self) -> bool {
        self.pending
            .values()
            .any(|delivery| delivery.wakes_agent && delivery.phase == DeliveryPhase::Queued)
    }

    pub fn reset_airc(&mut self) {
        self.pending.retain(|identifier, delivery| {
            !identifier.starts_with("airc:") || delivery.phase == DeliveryPhase::Completed
        });
    }

    pub fn apply(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::CoordinationPrompt { identifiers, .. } => {
                for identifier in identifiers {
                    if let Some(delivery) = self.pending.get_mut(identifier) {
                        delivery.phase = DeliveryPhase::Prompted;
                    }
                }
            }
            AgentEvent::Transcript {
                update: SessionUpdate::UserMessageChunk(chunk),
                ..
            } => {
                if let ContentBlock::Text(text) = &chunk.content {
                    for (identifier, delivery) in &mut self.pending {
                        if !identifier.starts_with("airc:")
                            && delivery.phase == DeliveryPhase::Queued
                            && delivery.message == text.text
                        {
                            delivery.phase = DeliveryPhase::Prompted;
                        }
                    }
                }
            }
            AgentEvent::TurnEnded(outcome) => {
                for delivery in self.pending.values_mut() {
                    if delivery.phase == DeliveryPhase::Prompted {
                        delivery.phase = if matches!(outcome, Ok(StopReason::EndTurn)) {
                            DeliveryPhase::Completed
                        } else {
                            DeliveryPhase::Failed
                        };
                    }
                }
            }
            _ => {}
        }
    }
}
