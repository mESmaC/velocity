use std::path::Path;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use crate::{
    Result, TEXT_LIMIT, bounded,
    cdp::Cdp,
    error, input,
    page::{self, Page},
    required,
};

pub enum Output {
    Text(String),
    Image { path: String, jpeg: String },
}

pub async fn call(
    page: &mut Page,
    cdp: &Cdp,
    profile: &Path,
    name: &str,
    args: &Value,
) -> Result<Output> {
    let value = match name {
        "open" => {
            let url = page::checked_url(required(args, "url")?)?;
            let id = page.open(cdp, "about:blank").await?;
            page.navigate(cdp, url).await?;
            json!({ "tab": id })
        }
        "navigate" => {
            page.navigate(cdp, page::checked_url(required(args, "url")?)?)
                .await?
        }
        "back" => page.back(cdp).await?,
        "reload" => page.reload(cdp).await?,
        "tabs" => return tabs(page, cdp, args).await,
        "snapshot" => page.snapshot(cdp).await?,
        "click" | "type" | "press" | "select" | "scroll" => {
            let node = page.resolve(cdp, required(args, "ref")?)?;
            element(cdp, node, name, args).await?;
            json!("Done")
        }
        "evaluate" => page::evaluate(cdp, required(args, "expression")?, true).await?,
        "screenshot" => return screenshot(cdp, profile, args["image"] == true).await,
        "console" => json!(cdp.events(|events| events.console.drain(..).collect::<Vec<_>>())),
        "network_errors" => {
            json!(cdp.events(|events| events.failures.drain(..).collect::<Vec<_>>()))
        }
        _ => return Err(error("Unknown browser tool")),
    };
    Ok(Output::Text(bounded(
        value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string()),
        TEXT_LIMIT,
    )))
}

async fn tabs(page: &mut Page, cdp: &Cdp, args: &Value) -> Result<Output> {
    match args["action"].as_str().unwrap_or("list") {
        "list" => {}
        "switch" => page.switch(cdp, required(args, "tab")?).await?,
        "close" => {
            let id = required(args, "tab")?;
            let tabs = page.tabs(cdp).await?;
            if !tabs.iter().any(|tab| tab.id == id) {
                return Err(error("This browser tab has closed"));
            }
            if tabs.len() == 1 {
                page.open(cdp, "about:blank").await?;
            }
            cdp.browser("Target.closeTarget", json!({"targetId":id}))
                .await?;
        }
        _ => return Err(error("Use list, switch or close")),
    }
    let tabs = page.ensure_active(cdp).await?;
    Ok(Output::Text(bounded(
        json!({"active":page.active,"tabs":tabs}).to_string(),
        TEXT_LIMIT,
    )))
}

async fn element(cdp: &Cdp, node: u64, name: &str, args: &Value) -> Result<()> {
    match name {
        "click" => {
            let (x, y) = input::position(cdp, node).await?;
            input::click(cdp, x, y).await
        }
        "type" => {
            input::reject_secret_field(cdp, node).await?;
            input::focus(cdp, node).await?;
            input::insert(cdp, required(args, "text")?).await
        }
        "press" => {
            input::reject_secret_field(cdp, node).await?;
            input::focus(cdp, node).await?;
            let (key, modifiers) = input::chord(required(args, "key")?)?;
            input::press(cdp, key, modifiers).await
        }
        "select" => {
            input::reject_secret_field(cdp, node).await?;
            input::select(cdp, node, required(args, "option")?).await
        }
        "scroll" => {
            let (x, y) = input::position(cdp, node).await?;
            let horizontal = args["horizontal"].as_f64().unwrap_or_default();
            let vertical = args["vertical"].as_f64().unwrap_or_default();
            if horizontal == 0. && vertical == 0. {
                return Ok(());
            }
            input::scroll(cdp, x, y, horizontal, vertical).await
        }
        _ => Err(error("Unknown element action")),
    }
}

async fn screenshot(cdp: &Cdp, profile: &Path, image: bool) -> Result<Output> {
    let jpeg = page::capture(cdp, 960., 640., 72).await?.jpeg;
    let bytes = STANDARD
        .decode(&jpeg)
        .map_err(|_| error("Invalid browser screenshot"))?;
    let path = profile.join("screenshot.jpg");
    tokio::fs::write(&path, bytes).await?;
    let path = path.display().to_string();
    if image {
        Ok(Output::Image { path, jpeg })
    } else {
        Ok(Output::Text(path))
    }
}

pub enum Wait<'a> {
    Text(&'a str),
    Selector(&'a str),
    NetworkIdle,
}

impl<'a> Wait<'a> {
    pub fn parse(args: &'a Value) -> Result<(Self, Duration)> {
        let criterion = match (
            args["text"].as_str(),
            args["selector"].as_str(),
            args["network_idle"] == true,
        ) {
            (Some(text), None, false) => Self::Text(text),
            (None, Some(selector), false) => Self::Selector(selector),
            (None, None, true) => Self::NetworkIdle,
            _ => return Err(error("Provide one of text, selector or network_idle")),
        };
        let timeout = args["timeout_ms"]
            .as_u64()
            .unwrap_or(5_000)
            .clamp(100, 30_000);
        Ok((criterion, Duration::from_millis(timeout)))
    }

    pub async fn matches(&self, cdp: &Cdp) -> Result<bool> {
        let expression = match self {
            Self::NetworkIdle => return Ok(page::network_idle(cdp)),
            Self::Text(text) => format!(
                "document.body?.innerText.includes({}) ?? false",
                json!(text)
            ),
            Self::Selector(selector) => {
                format!("document.querySelector({}) !== null", json!(selector))
            }
        };
        Ok(page::evaluate(cdp, &expression, false).await? == true)
    }
}

fn tool(name: &str, description: &str, properties: &[(&str, &str)], required: &[&str]) -> Value {
    let properties = properties
        .iter()
        .map(|(name, kind)| (name.to_string(), json!({"type":kind})))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "name":name,"description":description,
        "inputSchema":{"type":"object","properties":properties,"required":required},
    })
}

pub fn definitions() -> Vec<Value> {
    navigation_tools()
        .into_iter()
        .chain(element_tools())
        .chain(observation_tools())
        .collect()
}

fn navigation_tools() -> [Value; 5] {
    [
        tool(
            "open",
            "Open an HTTP(S) URL in a new tab. Supports localhost.",
            &[("url", "string")],
            &["url"],
        ),
        tool(
            "navigate",
            "Navigate the active tab to an HTTP(S) URL.",
            &[("url", "string")],
            &["url"],
        ),
        tool("back", "Go back in the active tab.", &[], &[]),
        tool("reload", "Reload the active tab.", &[], &[]),
        tool(
            "tabs",
            "List tabs, or switch or close by tab ID (action: list, switch, close).",
            &[("action", "string"), ("tab", "string")],
            &[],
        ),
    ]
}

fn element_tools() -> [Value; 6] {
    [
        tool(
            "snapshot",
            "Read the active page as an accessibility tree. Refs are valid until navigation or the next snapshot.",
            &[],
            &[],
        ),
        tool(
            "click",
            "Click an element from the current snapshot.",
            &[("ref", "string")],
            &["ref"],
        ),
        tool(
            "type",
            "Insert text into an element. Use the owner's Take over for sign-in and secrets.",
            &[("ref", "string"), ("text", "string")],
            &["ref", "text"],
        ),
        tool(
            "press",
            "Focus an element and press a key, for example Enter or Ctrl+a.",
            &[("ref", "string"), ("key", "string")],
            &["ref", "key"],
        ),
        tool(
            "select",
            "Choose a native select option by label or value.",
            &[("ref", "string"), ("option", "string")],
            &["ref", "option"],
        ),
        tool(
            "scroll",
            "Scroll an element into view, optionally scrolling by the given pixel deltas.",
            &[
                ("ref", "string"),
                ("horizontal", "number"),
                ("vertical", "number"),
            ],
            &["ref"],
        ),
    ]
}

fn observation_tools() -> [Value; 5] {
    [
        tool(
            "wait_for",
            "Wait for visible text, a CSS selector, or network_idle=true. Timeout defaults to 5000 ms and is at most 30000 ms.",
            &[
                ("text", "string"),
                ("selector", "string"),
                ("network_idle", "boolean"),
                ("timeout_ms", "integer"),
            ],
            &[],
        ),
        tool(
            "screenshot",
            "Save a downscaled JPEG to a file. Set image=true to include the image.",
            &[("image", "boolean")],
            &[],
        ),
        tool(
            "console",
            "Read console messages since the previous console call.",
            &[],
            &[],
        ),
        tool(
            "network_errors",
            "Read failed network requests since the previous network_errors call.",
            &[],
            &[],
        ),
        tool(
            "evaluate",
            "Evaluate a read-only JavaScript expression and return a bounded result.",
            &[("expression", "string")],
            &["expression"],
        ),
    ]
}
