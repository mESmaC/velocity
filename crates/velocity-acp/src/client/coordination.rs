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

    pub fn take_prompt(&mut self) -> Option<(String, Vec<String>, usize)> {
        if self.deliveries.is_empty() {
            return None;
        }
        self.wake = None;
        let mut deliveries = std::mem::take(&mut self.deliveries);
        let actions = deliveries
            .iter()
            .filter(|item| item.priority == DeliveryPriority::NeedsAction)
            .count();
        let identifiers = deliveries.iter().map(Delivery::identifier).collect();
        let mut prompt = prompt_lines(&deliveries).join("\n");
        if prompt.len() > velocity_core::text_budget::PROMPT_BYTES {
            deliveries.sort_by_key(|delivery| delivery.priority != DeliveryPriority::NeedsAction);
            let first = deliveries
                .iter()
                .map(|delivery| delivery.sequence)
                .min()
                .unwrap_or_default();
            let last = deliveries
                .iter()
                .map(|delivery| delivery.sequence)
                .max()
                .unwrap_or_default();
            let hint = format!(
                "\n[Notices clipped. Use read_feed with after {} and before {} for more details.]",
                first.saturating_sub(1),
                last.saturating_add(1)
            );
            prompt = velocity_core::text_budget::prompt_excerpt(
                &prompt_lines(&deliveries).join("\n"),
                velocity_core::text_budget::PROMPT_BYTES,
                &hint,
            );
        }
        Some((prompt, identifiers, actions))
    }
}
