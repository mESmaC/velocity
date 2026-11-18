use std::time::Duration;

use tokio::time::Instant;
use velocity_airc::{Delivery, DeliveryPriority, prompt_lines};

// The native channel is polled once a second. Give the next pass time to join the
// first actionable notice, with a fixed deadline even if new notices keep coming.
const COLLECTION_WINDOW: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct Coordination {
    deliveries: Vec<Delivery>,
    wake: Option<Instant>,
}

impl Coordination {
    pub fn push(&mut self, delivery: Delivery) {
        if delivery.priority == DeliveryPriority::NeedsAction {
            self.wake
                .get_or_insert_with(|| Instant::now() + COLLECTION_WINDOW);
        }
        self.deliveries.push(delivery);
    }

    pub async fn wait(&self) {
        if let Some(wake) = self.wake {
            tokio::time::sleep_until(wake).await;
        } else {
            std::future::pending().await
        }
    }

    pub fn turn_ended(&mut self) {
        if let Some(wake) = &mut self.wake {
            // Give normal input queued by the thread runtime time to reach ACP.
            *wake = Instant::now() + COLLECTION_WINDOW;
        }
    }

    pub fn take_prompt(&mut self) -> Option<(String, Vec<u64>, usize)> {
        if self.deliveries.is_empty() {
            return None;
        }
        self.wake = None;
        let deliveries = std::mem::take(&mut self.deliveries);
        let actions = deliveries
            .iter()
            .filter(|item| item.priority == DeliveryPriority::NeedsAction)
            .count();
        let sequences = deliveries.iter().map(|item| item.sequence).collect();
        Some((prompt_lines(&deliveries).join("\n"), sequences, actions))
    }
}
