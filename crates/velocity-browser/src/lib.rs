//! A private Chromium profile, CDP connection and tool route for each build thread.

mod cdp;
mod engine;
mod hub;
mod input;
mod page;
mod tools;
mod transport;

pub use cdp::Frame;
pub use engine::MEMORY_BUDGET;
pub use hub::{BrowserHub, BrowserKey, BrowserView, Route};
pub use input::BrowserInput;
pub use transport::{PORT_ENV, SERVER, SWITCH, TOKEN_ENV, serve_mcp};

use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    WebSocket(Box<tokio_tungstenite::tungstenite::Error>),
}

impl From<tokio_tungstenite::tungstenite::Error> for BrowserError {
    fn from(error: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::WebSocket(Box::new(error))
    }
}

pub type Result<T> = std::result::Result<T, BrowserError>;

fn error(message: impl Into<String>) -> BrowserError {
    BrowserError::Message(message.into())
}

fn required<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name]
        .as_str()
        .ok_or_else(|| error(format!("Provide {name}")))
}

const TEXT_LIMIT: usize = 8_000;

fn bounded(mut text: String, limit: usize) -> String {
    if text.len() > limit {
        text.truncate(text.floor_char_boundary(limit));
        text.push_str("\n[truncated]");
    }
    text
}
