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
            },
        );
    }

    pub fn status(&self, identifier: &str, message: &str) -> Option<DeliveryStatus> {
        let delivery = self.pending.get(identifier)?;
        if delivery.message != message {
            return None;
        }
        match delivery.phase {
            DeliveryPhase::Queued | DeliveryPhase::Prompted => Some(DeliveryStatus::Pending),
            DeliveryPhase::Completed => Some(DeliveryStatus::Delivered),
            DeliveryPhase::Failed => None,
        }
    }

    pub fn forget(&mut self, identifier: &str) {
        self.pending.remove(identifier);
    }

    pub fn apply(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Transcript {
                update: SessionUpdate::UserMessageChunk(chunk),
                ..
            } => {
                if let ContentBlock::Text(text) = &chunk.content {
                    for delivery in self.pending.values_mut() {
                        if delivery.phase == DeliveryPhase::Queued && delivery.message == text.text
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
