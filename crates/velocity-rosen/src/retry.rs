//! Retry transient workspace launch failures without changing version control (F5, #407).

use std::future::Future;
use std::time::Duration;

/// Retries returned transport failures for up to a minute, with exponential backoff.
/// An in-flight workspace creation is never cancelled: it may already own a change.
pub async fn retry_workspace<Value, Attempt, Pending>(mut attempt: Attempt) -> Result<Value, String>
where
    Attempt: FnMut() -> Pending,
    Pending: Future<Output = Result<Value, String>>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut delay = Duration::from_secs(1);
    loop {
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() || !transient(&error) {
                    return Err(error);
                }
                tokio::time::sleep(delay.min(remaining)).await;
                if tokio::time::Instant::now() >= deadline {
                    return Err(error);
                }
                delay = (delay * 2).min(Duration::from_secs(8));
            }
        }
    }
}

// The Tauri edges also return errors from starting the server and recording its source,
// so classify their displayed causes as well as Rosen client's transport messages.
fn transient(error: &str) -> bool {
    let error = error.to_lowercase();
    [
        "all pipe instances are busy",
        "pipe busy",
        "os error 231",
        "connection refused",
        "actively refused",
        "os error 10061",
        "timed out",
        "timeout",
        "time out",
        "no rosen server is running",
        "the connection to the server closed",
    ]
    .iter()
    .any(|cause| error.contains(cause))
}
