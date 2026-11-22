use std::collections::HashMap;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    Result, TEXT_LIMIT, bounded,
    cdp::{Cdp, Frame, safe_url},
    error, input, required,
};

#[derive(Clone, PartialEq, Serialize, specta::Type)]
pub struct Tab {
    pub id: String,
    pub title: String,
    pub url: String,
}

#[derive(Default)]
pub struct Page {
    pub active: Option<String>,
    refs: Vec<u64>,
    document: u64,
    watching: bool,
    casting: bool,
    preview_document: Option<u64>,
}

impl Page {
    pub async fn tabs(&self, cdp: &Cdp) -> Result<Vec<Tab>> {
        let result = cdp.browser("Target.getTargets", json!({})).await?;
        Ok(result["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|target| target["type"] == "page")
            .filter_map(|target| {
                Some(Tab {
                    id: target["targetId"].as_str()?.to_owned(),
                    title: bounded(target["title"].as_str()?.to_owned(), 160),
                    url: safe_url(&target["url"]),
                })
            })
            .collect())
    }

    pub async fn ensure_active(&mut self, cdp: &Cdp) -> Result<Vec<Tab>> {
        let tabs = self.tabs(cdp).await?;
        if !tabs.iter().any(|tab| Some(&tab.id) == self.active.as_ref()) {
            self.active = None;
            if let Some(tab) = tabs.first() {
                self.switch(cdp, &tab.id).await?;
            }
        }
        Ok(tabs)
    }

    pub async fn switch(&mut self, cdp: &Cdp, id: &str) -> Result<()> {
        if self.active.as_deref() == Some(id) {
            return self.watch(cdp, self.watching).await;
        }
        // Keep the previous session bound during detach so diagnostics already queued on the
        // DevTools socket for that tab are captured before clearing per-tab session state.
        let previous = cdp.events(|events| events.session.clone());
        if let Some(session) = previous {
            let _ = cdp
                .browser("Target.detachFromTarget", json!({"sessionId":session}))
                .await;
            cdp.events(|events| events.set_session(None));
        }
        self.active = None;
        self.casting = false;
        self.preview_document = None;
        self.refs.clear();
        let result = cdp
            .browser(
                "Target.attachToTarget",
                json!({"targetId":id,"flatten":true}),
            )
            .await?;
        let session = required(&result, "sessionId")?.to_owned();
        cdp.events(|events| events.set_session(Some(session)));
        self.active = Some(id.to_owned());
        cdp.page("Page.enable", json!({})).await?;
        cdp.page("DOM.enable", json!({})).await?;
        cdp.page("Runtime.enable", json!({})).await?;
        cdp.page("Network.enable", json!({})).await?;
        self.watch(cdp, self.watching).await
    }

    pub async fn watch(&mut self, cdp: &Cdp, watching: bool) -> Result<()> {
        self.watching = watching;
        if self.active.is_none() || watching == self.casting {
            return Ok(());
        }
        if watching {
            cdp.page(
                "Page.startScreencast",
                json!({
                    "format":"jpeg","quality":65,"maxWidth":1024,"maxHeight":720,"everyNthFrame":2,
                }),
            )
            .await?;
        } else {
            cdp.page("Page.stopScreencast", json!({})).await?;
            cdp.events(|events| events.frame = None);
            self.preview_document = None;
        }
        self.casting = watching;
        Ok(())
    }

    pub async fn frame(&mut self, cdp: &Cdp) -> Option<Frame> {
        let (frame, document) = cdp.events(|events| (events.frame.take(), events.document));
        if frame.is_some() {
            self.preview_document = Some(document);
            return frame;
        }
        if !self.casting || self.preview_document == Some(document) {
            return None;
        }
        // A static page may not produce a screencast frame until its next paint.
        let frame = capture(cdp, 1024., 720., 65).await.ok()?;
        self.preview_document = Some(document);
        Some(frame)
    }

    pub async fn open(&mut self, cdp: &Cdp, url: &str) -> Result<String> {
        let result = cdp
            .browser("Target.createTarget", json!({"url":url}))
            .await?;
        let id = required(&result, "targetId")?.to_owned();
        self.switch(cdp, &id).await?;
        Ok(id)
    }

    pub async fn navigate(&mut self, cdp: &Cdp, url: &str) -> Result<Value> {
        self.refs.clear();
        let result = cdp.page("Page.navigate", json!({"url":url})).await?;
        if let Some(message) = result["errorText"].as_str() {
            return Err(error(bounded(message.to_owned(), 200)));
        }
        Ok(json!("Navigation started"))
    }

    pub async fn back(&mut self, cdp: &Cdp) -> Result<Value> {
        let history = cdp.page("Page.getNavigationHistory", json!({})).await?;
        let index = history["currentIndex"].as_u64().unwrap_or_default();
        let Some(previous) = index.checked_sub(1) else {
            return Err(error("No previous page in this tab"));
        };
        self.refs.clear();
        cdp.page(
            "Page.navigateToHistoryEntry",
            json!({"entryId":history["entries"][previous as usize]["id"]}),
        )
        .await?;
        Ok(json!("Navigated back"))
    }

    pub async fn reload(&mut self, cdp: &Cdp) -> Result<Value> {
        self.refs.clear();
        cdp.page("Page.reload", json!({})).await?;
        Ok(json!("Reload started"))
    }

    pub fn invalidate(&mut self) {
        self.refs.clear();
    }

    pub async fn snapshot(&mut self, cdp: &Cdp) -> Result<Value> {
        self.refs.clear();
        self.document = cdp.events(|events| events.document);
        let ax = cdp.page("Accessibility.getFullAXTree", json!({})).await?;
        let nodes = ax["nodes"]
            .as_array()
            .ok_or_else(|| error("No accessibility tree"))?;
        let mut depths = HashMap::new();
        let mut lines = String::new();
        for node in nodes {
            let parent = node["parentId"]
                .as_str()
                .and_then(|id| depths.get(id))
                .copied()
                .unwrap_or(0);
            let line = ax_line(node, &mut self.refs);
            if let Some(id) = node["nodeId"].as_str() {
                depths.insert(id, parent + usize::from(line.is_some()));
            }
            if let Some(line) = line {
                lines.push_str(&"  ".repeat(parent.min(8)));
                lines.push_str(&line);
                lines.push('\n');
            }
            if lines.len() >= TEXT_LIMIT {
                break;
            }
        }
        Ok(json!(bounded(lines, TEXT_LIMIT)))
    }

    pub fn resolve(&self, cdp: &Cdp, reference: &str) -> Result<u64> {
        let node = reference
            .strip_prefix('e')
            .and_then(|index| index.parse::<usize>().ok())
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| self.refs.get(index))
            .copied();
        if let Some(node) = node
            && cdp.events(|events| events.document == self.document)
        {
            return Ok(node);
        }
        Err(error("This element ref is stale. Take a new snapshot."))
    }
}

fn ax_line(node: &Value, refs: &mut Vec<u64>) -> Option<String> {
    let role = node["role"]["value"].as_str()?;
    if node["ignored"] == true || matches!(role, "InlineTextBox" | "none") {
        return None;
    }
    let name = node["name"]["value"].as_str().unwrap_or_default();
    if name.is_empty() && matches!(role, "generic" | "paragraph" | "group" | "StaticText") {
        return None;
    }
    let mut line = format!(
        "{role} {}",
        bounded(name.to_owned(), 240).replace(['\n', '\r'], " ")
    );
    if !matches!(role, "StaticText" | "RootWebArea")
        && let Some(node) = node["backendDOMNodeId"].as_u64()
    {
        refs.push(node);
        line.push_str(&format!(" [e{}]", refs.len()));
    }
    for state in ["disabled", "checked", "selected", "expanded"] {
        if let Some(properties) = node["properties"].as_array()
            && properties.iter().any(|property| {
                property["name"] == state
                    && (property["value"]["value"] == true || property["value"]["value"] == "true")
            })
        {
            line.push_str(&format!(" ({state})"));
        }
    }
    Some(line)
}

pub fn checked_url(input: &str) -> Result<&str> {
    match url::Url::parse(input) {
        Ok(url)
            if matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none() =>
        {
            Ok(input)
        }
        _ => Err(error("Provide an HTTP or HTTPS URL without credentials")),
    }
}

pub async fn evaluate(cdp: &Cdp, expression: &str, read_only: bool) -> Result<Value> {
    let result = cdp.page("Runtime.evaluate", json!({
        "expression":expression,"returnByValue":true,"throwOnSideEffect":read_only,"timeout":1500,
    })).await?;
    if !result["exceptionDetails"].is_null() {
        return Err(error("The JavaScript expression threw or had side effects"));
    }
    Ok(result["result"]["value"].clone())
}

pub fn network_idle(cdp: &Cdp) -> bool {
    cdp.events(|events| {
        events.network.is_empty()
            && events
                .activity
                .is_none_or(|activity| activity.elapsed() >= Duration::from_millis(500))
    })
}

pub async fn capture(cdp: &Cdp, max_width: f64, max_height: f64, quality: u32) -> Result<Frame> {
    let (width, height) = input::viewport(cdp).await?;
    let scale = (max_width / width).min(max_height / height).min(1.);
    let capture = cdp
        .page(
            "Page.captureScreenshot",
            json!({
                "format":"jpeg","quality":quality,"captureBeyondViewport":false,
                "clip":{"x":0,"y":0,"width":width,"height":height,"scale":scale},
            }),
        )
        .await?;
    let jpeg = required(&capture, "data")?;
    if jpeg.len() > 512 * 1024 {
        return Err(error("The browser frame exceeded its size limit"));
    }
    Ok(Frame {
        jpeg: jpeg.to_owned(),
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;

    use super::*;
    use crate::tools::{self, Output};

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    struct MockCdp {
        cdp: Cdp,
        push: mpsc::UnboundedSender<Value>,
        server: tokio::task::JoinHandle<()>,
    }

    impl Drop for MockCdp {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    impl MockCdp {
        fn send(&self, event: Value) -> TestResult {
            self.push.send(event)?;
            Ok(())
        }

        async fn flush(&self) -> TestResult {
            self.cdp.browser("Browser.getVersion", json!({})).await?;
            Ok(())
        }
    }

    async fn start_mock_cdp() -> std::result::Result<MockCdp, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (push, mut outbound) = mpsc::unbounded_channel::<Value>();
        let server = tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(mut socket) = accept_async(stream).await else {
                return;
            };
            let mut next_tab = 2_u32;
            loop {
                tokio::select! {
                    biased;
                    Some(event) = outbound.recv() => {
                        if socket.send(Message::text(event.to_string())).await.is_err() {
                            break;
                        }
                    }
                    incoming = socket.next() => {
                        let Some(Ok(Message::Text(text))) = incoming else { break };
                        let Ok(request) = serde_json::from_str::<Value>(&text) else { break };
                        let reply = mock_reply(&request, &mut next_tab);
                        if socket.send(Message::text(reply.to_string())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let cdp = Cdp::connect(format!("ws://{address}")).await?;
        Ok(MockCdp { cdp, push, server })
    }

    fn mock_reply(request: &Value, next_tab: &mut u32) -> Value {
        let id = request["id"].as_u64().unwrap_or_default();
        let result = match request["method"].as_str().unwrap_or_default() {
            "Target.attachToTarget" => {
                let target = request["params"]["targetId"].as_str().unwrap_or_default();
                json!({ "sessionId": format!("session-{target}") })
            }
            "Target.createTarget" => {
                let target = format!("tab-{}", *next_tab);
                *next_tab += 1;
                json!({ "targetId": target })
            }
            "Target.getTargets" => json!({
                "targetInfos": [
                    { "targetId": "tab-a", "type": "page", "title": "Tab A", "url": "http://localhost:3000/a" },
                    { "targetId": "tab-b", "type": "page", "title": "Tab B", "url": "http://localhost:3000/b" },
                ]
            }),
            _ => json!({}),
        };
        json!({ "id": id, "result": result })
    }

    fn text_output(output: Output) -> std::result::Result<String, Box<dyn std::error::Error>> {
        match output {
            Output::Text(text) => Ok(text),
            Output::Image { .. } => Err("expected text output".into()),
        }
    }

    async fn drain(
        page: &mut Page,
        cdp: &Cdp,
        tool: &str,
    ) -> std::result::Result<Vec<String>, Box<dyn std::error::Error>> {
        let output = tools::call(page, cdp, Path::new("."), tool, &json!({})).await?;
        Ok(serde_json::from_str(&text_output(output)?)?)
    }

    fn console_event(session: &str, kind: &str, message: &str) -> Value {
        json!({
            "method": "Runtime.consoleAPICalled",
            "sessionId": session,
            "params": { "type": kind, "args": [{ "type": "string", "value": message }] },
        })
    }

    fn http_failure_event(session: &str, status: u64, url: &str) -> Value {
        json!({
            "method": "Network.responseReceived",
            "sessionId": session,
            "params": { "response": { "status": status, "url": url } },
        })
    }

    // Risk (#635): switching tabs must not discard unread console or network failure diagnostics
    // before their MCP drains read them once, while still clearing detached per-tab network state.
    #[tokio::test]
    async fn switch_preserves_unread_diagnostics_until_drained() -> TestResult {
        let mock = start_mock_cdp().await?;
        let mut page = Page::default();
        page.switch(&mock.cdp, "tab-a").await?;

        mock.send(json!({
            "method": "Network.requestWillBeSent",
            "sessionId": "session-tab-a",
            "params": { "requestId": "req-1", "request": { "url": "http://localhost:3000/slow" } },
        }))?;
        mock.send(console_event("session-tab-a", "warn", "from tab a"))?;
        mock.send(http_failure_event(
            "session-tab-a",
            500,
            "http://localhost:3000/fail?token=secret",
        ))?;
        mock.flush().await?;
        assert!(!network_idle(&mock.cdp));

        tools::call(
            &mut page,
            &mock.cdp,
            Path::new("."),
            "tabs",
            &json!({ "action": "switch", "tab": "tab-b" }),
        )
        .await?;
        assert!(network_idle(&mock.cdp));

        assert_eq!(
            drain(&mut page, &mock.cdp, "console").await?,
            vec!["warn: from tab a"]
        );
        assert_eq!(
            drain(&mut page, &mock.cdp, "network_errors").await?,
            vec!["HTTP 500 http://localhost:3000/fail"]
        );
        assert!(drain(&mut page, &mock.cdp, "console").await?.is_empty());
        assert!(
            drain(&mut page, &mock.cdp, "network_errors")
                .await?
                .is_empty()
        );
        Ok(())
    }

    // Risk (#635): opening a new tab switches CDP sessions and must preserve unread diagnostics
    // from the previous tab until drained once.
    #[tokio::test]
    async fn open_preserves_unread_diagnostics_until_drained() -> TestResult {
        let mock = start_mock_cdp().await?;
        let mut page = Page::default();
        page.switch(&mock.cdp, "tab-b").await?;

        mock.send(json!({
            "method": "Runtime.exceptionThrown",
            "sessionId": "session-tab-b",
            "params": {},
        }))?;
        mock.send(json!({
            "method": "Network.requestWillBeSent",
            "sessionId": "session-tab-b",
            "params": { "requestId": "req-2", "request": { "url": "http://localhost:3000/down" } },
        }))?;
        mock.send(json!({
            "method": "Network.loadingFailed",
            "sessionId": "session-tab-b",
            "params": { "requestId": "req-2", "errorText": "net::ERR_FAILED", "canceled": false },
        }))?;
        tools::call(
            &mut page,
            &mock.cdp,
            Path::new("."),
            "open",
            &json!({ "url": "http://localhost:3000/c" }),
        )
        .await?;

        assert_eq!(
            drain(&mut page, &mock.cdp, "console").await?,
            vec!["Uncaught JavaScript exception"]
        );
        assert_eq!(
            drain(&mut page, &mock.cdp, "network_errors").await?,
            vec!["net::ERR_FAILED http://localhost:3000/down"]
        );
        assert!(drain(&mut page, &mock.cdp, "console").await?.is_empty());
        assert!(
            drain(&mut page, &mock.cdp, "network_errors")
                .await?
                .is_empty()
        );
        Ok(())
    }

    // Risk (#635): preserving diagnostics across tab switches must still suppress owner
    // takeover diagnostics and enforce bounded log and output sizes.
    #[tokio::test]
    async fn switch_preserves_takeover_suppression_and_bounds() -> TestResult {
        let mock = start_mock_cdp().await?;
        let mut page = Page::default();
        page.switch(&mock.cdp, "tab-a").await?;

        mock.cdp.events(|events| events.owner = true);
        page.switch(&mock.cdp, "tab-b").await?;
        assert!(mock.cdp.events(|events| events.owner));

        mock.send(console_event("session-tab-b", "log", "owner secret"))?;
        mock.send(http_failure_event(
            "session-tab-b",
            502,
            "http://localhost:3000/owner",
        ))?;
        mock.flush().await?;
        assert!(drain(&mut page, &mock.cdp, "console").await?.is_empty());
        assert!(
            drain(&mut page, &mock.cdp, "network_errors")
                .await?
                .is_empty()
        );

        mock.cdp.events(|events| events.owner = false);
        for index in 0..40 {
            mock.send(console_event(
                "session-tab-b",
                "log",
                &format!("entry-{index}"),
            ))?;
        }
        mock.flush().await?;
        page.switch(&mock.cdp, "tab-a").await?;

        let lines = drain(&mut page, &mock.cdp, "console").await?;
        assert_eq!(lines.len(), 32);
        assert_eq!(lines.first().map(String::as_str), Some("log: entry-8"));
        assert_eq!(lines.last().map(String::as_str), Some("log: entry-39"));

        let long_url = format!("http://localhost:3000/{}", "x".repeat(400));
        for _ in 0..32 {
            mock.send(http_failure_event("session-tab-a", 500, &long_url))?;
        }
        mock.flush().await?;
        page.switch(&mock.cdp, "tab-b").await?;
        let text = text_output(
            tools::call(
                &mut page,
                &mock.cdp,
                Path::new("."),
                "network_errors",
                &json!({}),
            )
            .await?,
        )?;
        assert!(text.len() <= TEXT_LIMIT + "\n[truncated]".len());
        assert!(text.ends_with("\n[truncated]"));
        Ok(())
    }
}
