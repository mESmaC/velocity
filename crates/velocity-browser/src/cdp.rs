use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async_with_config};

use crate::{Result, bounded, error};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
const LOG_LIMIT: usize = 32;

struct Request {
    method: &'static str,
    session: Option<String>,
    params: Value,
    reply: oneshot::Sender<Result<Value>>,
}

#[derive(Clone, Serialize, specta::Type)]
pub struct Frame {
    pub jpeg: String,
    pub width: f64,
    pub height: f64,
}

#[derive(Default)]
pub struct Events {
    pub session: Option<String>,
    pub frame: Option<Frame>,
    pub document: u64,
    pub console: VecDeque<String>,
    pub failures: VecDeque<String>,
    pub network: HashMap<String, String>,
    pub activity: Option<Instant>,
    pub owner: bool,
}

impl Events {
    /// Reset per-tab CDP session state without discarding unread diagnostics or takeover state.
    pub fn set_session(&mut self, session: Option<String>) {
        self.session = session;
        self.frame = None;
        self.document = 0;
        self.network.clear();
        self.activity = None;
    }
}

pub struct Cdp {
    requests: mpsc::Sender<Request>,
    events: Arc<Mutex<Events>>,
    worker: tokio::task::JoinHandle<()>,
}

impl Cdp {
    pub async fn connect(endpoint: String) -> Result<Self> {
        let config = WebSocketConfig::default().max_message_size(Some(8 * 1024 * 1024));
        let (socket, _) = tokio::time::timeout(
            REPLY_TIMEOUT,
            connect_async_with_config(endpoint, Some(config), false),
        )
        .await
        .map_err(|_| error("DevTools connection timed out"))??;
        let (requests, receiver) = mpsc::channel(4);
        let events = Arc::new(Mutex::new(Events::default()));
        let worker = tokio::spawn(run(socket, receiver, events.clone()));
        Ok(Self {
            requests,
            events,
            worker,
        })
    }

    pub fn connected(&self) -> bool {
        !self.requests.is_closed()
    }

    pub fn events<T>(&self, read: impl FnOnce(&mut Events) -> T) -> T {
        read(
            &mut self
                .events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    pub async fn browser(&self, method: &'static str, params: Value) -> Result<Value> {
        self.call(method, None, params).await
    }

    pub async fn page(&self, method: &'static str, params: Value) -> Result<Value> {
        let session = self.events(|events| events.session.clone());
        self.call(
            method,
            Some(session.ok_or_else(|| error("No active browser tab"))?),
            params,
        )
        .await
    }

    async fn call(
        &self,
        method: &'static str,
        session: Option<String>,
        params: Value,
    ) -> Result<Value> {
        let disconnected = || error("The browser connection closed");
        let (reply, response) = oneshot::channel();
        self.requests
            .send(Request {
                method,
                session,
                params,
                reply,
            })
            .await
            .map_err(|_| disconnected())?;
        response.await.map_err(|_| disconnected())?
    }
}

impl Drop for Cdp {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

async fn run(
    mut socket: Socket,
    mut requests: mpsc::Receiver<Request>,
    events: Arc<Mutex<Events>>,
) {
    let mut identifier = 0_u64;
    loop {
        tokio::select! {
            request = requests.recv() => {
                let Some(request) = request else { break };
                identifier += 1;
                let result = tokio::time::timeout(
                    REPLY_TIMEOUT, exchange(&mut socket, &events, identifier, &request),
                ).await.unwrap_or_else(|_| Err(error(format!("{} timed out", request.method))));
                let _ = request.reply.send(result);
            }
            message = socket.next() => {
                if consume(&mut socket, &events, message).await.is_err() {
                    break;
                }
            }
        }
    }
}

async fn exchange(
    socket: &mut Socket,
    events: &Mutex<Events>,
    id: u64,
    request: &Request,
) -> Result<Value> {
    let mut message = json!({"id":id,"method":request.method,"params":request.params});
    if let Some(session) = &request.session {
        message["sessionId"] = json!(session);
    }
    socket.send(Message::text(message.to_string())).await?;
    loop {
        let message = socket.next().await;
        if let Some(response) = consume(socket, events, message).await?
            && response["id"].as_u64() == Some(id)
        {
            if let Some(message) = response["error"]["message"].as_str() {
                return Err(error(bounded(
                    format!("{}: {message}", request.method),
                    400,
                )));
            }
            return Ok(response["result"].clone());
        }
    }
}

async fn consume(
    socket: &mut Socket,
    events: &Mutex<Events>,
    message: Option<std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>,
) -> Result<Option<Value>> {
    let message = message.ok_or_else(|| error("The browser connection closed"))??;
    let value: Value = match message {
        Message::Text(text) => serde_json::from_str(&text)?,
        Message::Close(_) => return Err(error("The browser connection closed")),
        _ => return Ok(None),
    };
    if value["id"].is_number() {
        return Ok(Some(value));
    }
    if value["method"] == "Page.screencastFrame" {
        let ack = json!({
            "id": 0, "method":"Page.screencastFrameAck", "sessionId":value["sessionId"],
            "params":{"sessionId": value["params"]["sessionId"]},
        });
        socket.send(Message::text(ack.to_string())).await?;
    }
    update(
        &mut events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        &value,
    );
    Ok(None)
}

fn update(events: &mut Events, event: &Value) {
    if events.session.as_deref() != event["sessionId"].as_str() {
        return;
    }
    let params = &event["params"];
    match event["method"].as_str().unwrap_or_default() {
        "Page.screencastFrame" => save_frame(events, params),
        "Page.frameNavigated" if params["frame"]["parentId"].is_null() => document_changed(events),
        "DOM.documentUpdated" => document_changed(events),
        "Network.requestWillBeSent" => {
            events.activity = Some(Instant::now());
            if let Some(id) = params["requestId"].as_str()
                && events.network.len() < 256
            {
                events
                    .network
                    .insert(id.to_owned(), safe_url(&params["request"]["url"]));
            }
        }
        "Network.loadingFinished" | "Network.loadingFailed" => finish_request(events, event),
        "Network.responseReceived" if !events.owner => {
            let status = params["response"]["status"].as_u64().unwrap_or_default();
            if status >= 400 {
                append(
                    &mut events.failures,
                    format!("HTTP {status} {}", safe_url(&params["response"]["url"])),
                );
            }
        }
        "Runtime.consoleAPICalled" if !events.owner => save_console(events, params),
        "Runtime.exceptionThrown" if !events.owner => {
            append(&mut events.console, "Uncaught JavaScript exception".into());
        }
        _ => {}
    }
}

fn document_changed(events: &mut Events) {
    events.document += 1;
    events.frame = None;
}

fn save_frame(events: &mut Events, params: &Value) {
    if let Some(image) = params["data"]
        .as_str()
        .filter(|data| data.len() <= 512 * 1024)
    {
        let metadata = &params["metadata"];
        events.frame = Some(Frame {
            jpeg: image.to_owned(),
            width: metadata["deviceWidth"]
                .as_f64()
                .unwrap_or(f64::from(crate::engine::WIDTH)),
            height: metadata["deviceHeight"]
                .as_f64()
                .unwrap_or(f64::from(crate::engine::HEIGHT)),
        });
    }
}

fn finish_request(events: &mut Events, event: &Value) {
    events.activity = Some(Instant::now());
    let params = &event["params"];
    let url = params["requestId"]
        .as_str()
        .and_then(|id| events.network.remove(id));
    if event["method"] == "Network.loadingFailed" && !events.owner && params["canceled"] != true {
        let detail = params["errorText"].as_str().unwrap_or("Request failed");
        append(
            &mut events.failures,
            format!("{detail} {}", url.unwrap_or_default()),
        );
    }
}

fn save_console(events: &mut Events, params: &Value) {
    let kind = params["type"].as_str().unwrap_or("log");
    let Some(args) = params["args"].as_array() else {
        return;
    };
    let words: Vec<_> = args
        .iter()
        .take(8)
        .map(|arg| match &arg["value"] {
            Value::String(text) => bounded(text.clone(), 160),
            Value::Null => arg["type"].as_str().unwrap_or("undefined").to_owned(),
            value => bounded(value.to_string(), 160),
        })
        .collect();
    append(&mut events.console, format!("{kind}: {}", words.join(" ")));
}

fn append(log: &mut VecDeque<String>, line: String) {
    if log.len() == LOG_LIMIT {
        log.pop_front();
    }
    log.push_back(bounded(line, 500));
}

pub fn safe_url(value: &Value) -> String {
    let Some(mut url) = value.as_str().and_then(|text| url::Url::parse(text).ok()) else {
        return String::new();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    bounded(url.to_string(), 500)
}
