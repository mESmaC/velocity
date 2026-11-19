use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Result, cdp::Cdp, error, required};

#[derive(Deserialize, specta::Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserInput {
    Click {
        x: f64,
        y: f64,
    },
    Key {
        key: String,
        modifiers: u8,
    },
    Text {
        text: String,
    },
    Scroll {
        x: f64,
        y: f64,
        horizontal: f64,
        vertical: f64,
    },
}

impl BrowserInput {
    pub async fn dispatch(self, cdp: &Cdp) -> Result<()> {
        match self {
            Self::Click { x, y } => click(cdp, x, y).await,
            Self::Key { key, modifiers } => press(cdp, &key, modifiers).await,
            Self::Text { text } if text.len() <= 8192 => insert(cdp, &text).await,
            Self::Text { .. } => Err(error("Send text in chunks of at most 8 KiB")),
            Self::Scroll {
                x,
                y,
                horizontal,
                vertical,
            } => scroll(cdp, x, y, horizontal, vertical).await,
        }
    }
}

pub async fn click(cdp: &Cdp, x: f64, y: f64) -> Result<()> {
    for event in ["mousePressed", "mouseReleased"] {
        cdp.page(
            "Input.dispatchMouseEvent",
            json!({
                "type":event,"x":x,"y":y,"button":"left","clickCount":1,
            }),
        )
        .await?;
    }
    Ok(())
}

pub async fn scroll(cdp: &Cdp, x: f64, y: f64, horizontal: f64, vertical: f64) -> Result<()> {
    cdp.page(
        "Input.dispatchMouseEvent",
        json!({
            "type":"mouseWheel","x":x,"y":y,"deltaX":horizontal,"deltaY":vertical,
        }),
    )
    .await?;
    Ok(())
}

pub async fn position(cdp: &Cdp, node: u64) -> Result<(f64, f64)> {
    cdp.page("DOM.scrollIntoViewIfNeeded", json!({"backendNodeId":node}))
        .await?;
    let model = cdp
        .page("DOM.getContentQuads", json!({"backendNodeId":node}))
        .await?;
    let quad = model["quads"][0]
        .as_array()
        .filter(|quad| quad.len() == 8)
        .ok_or_else(|| error("The element is outside the visible page"))?;
    let (width, height) = viewport(cdp).await?;
    let x = quad
        .iter()
        .step_by(2)
        .map(|value| value.as_f64().unwrap_or_default().clamp(0., width))
        .sum::<f64>()
        / 4.;
    let y = quad
        .iter()
        .skip(1)
        .step_by(2)
        .map(|value| value.as_f64().unwrap_or_default().clamp(0., height))
        .sum::<f64>()
        / 4.;
    Ok((x, y))
}

pub async fn viewport(cdp: &Cdp) -> Result<(f64, f64)> {
    let metrics = cdp.page("Page.getLayoutMetrics", json!({})).await?;
    let viewport = &metrics["cssVisualViewport"];
    Ok((
        viewport["clientWidth"]
            .as_f64()
            .unwrap_or(f64::from(crate::engine::WIDTH)),
        viewport["clientHeight"]
            .as_f64()
            .unwrap_or(f64::from(crate::engine::HEIGHT)),
    ))
}

pub async fn focus(cdp: &Cdp, node: u64) -> Result<()> {
    cdp.page("DOM.focus", json!({"backendNodeId":node})).await?;
    Ok(())
}

pub async fn insert(cdp: &Cdp, text: &str) -> Result<()> {
    cdp.page("Input.insertText", json!({"text":text})).await?;
    Ok(())
}

pub fn chord(key: &str) -> Result<(&str, u8)> {
    let mut parts = key.split('+').peekable();
    let mut modifiers = 0;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() && !part.is_empty() {
            return Ok((part, modifiers));
        }
        modifiers |= match part {
            "Alt" => 1,
            "Ctrl" | "Control" => 2,
            "Meta" => 4,
            "Shift" => 8,
            _ => return Err(error("Use modifiers Alt, Ctrl, Meta or Shift")),
        };
    }
    Err(error("Provide a key such as Enter or Ctrl+a"))
}

pub async fn press(cdp: &Cdp, key: &str, modifiers: u8) -> Result<()> {
    let (code, virtual_key) = key_code(key)?;
    let text = if modifiers & 7 == 0 {
        match key {
            "Enter" => "\r",
            "Space" => " ",
            value if value.chars().count() == 1 => value,
            _ => "",
        }
    } else {
        ""
    };
    let key = if key == "Space" { " " } else { key };
    let mut down = json!({
        "type": if text.is_empty() { "rawKeyDown" } else { "keyDown" },
        "key":key,"code":code,"windowsVirtualKeyCode":virtual_key,"modifiers":modifiers,"text":text,
    });
    cdp.page("Input.dispatchKeyEvent", down.clone()).await?;
    down["type"] = json!("keyUp");
    down["text"] = json!("");
    cdp.page("Input.dispatchKeyEvent", down).await?;
    Ok(())
}

fn key_code(key: &str) -> Result<(String, u32)> {
    let value = match key {
        "Backspace" => 8,
        "Tab" => 9,
        "Enter" => 13,
        "Escape" => 27,
        " " | "Space" => return Ok(("Space".into(), 32)),
        "PageUp" => 33,
        "PageDown" => 34,
        "End" => 35,
        "Home" => 36,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "Delete" => 46,
        _ if key.len() == 1 && key.as_bytes()[0].is_ascii_alphabetic() => {
            let letter = key.as_bytes()[0].to_ascii_uppercase();
            return Ok((format!("Key{}", char::from(letter)), u32::from(letter)));
        }
        _ if key.len() == 1 && key.as_bytes()[0].is_ascii_digit() => {
            return Ok((format!("Digit{key}"), u32::from(key.as_bytes()[0])));
        }
        _ if key.chars().count() == 1 => return Ok((String::new(), 0)),
        _ => return Err(error("Unsupported browser key")),
    };
    Ok((key.to_owned(), value))
}

// This check reads only element metadata. Never include the form values in tool output.
const SECRET_FIELD: &str = r#"function () {
  const field = [this.type, this.name, this.id, this.autocomplete, this.placeholder,
    this.getAttribute('aria-label'), ...Array.from(this.labels || [], l => l.textContent)].join(' ');
  return /password|passcode|passwd|token|secret|api[-_ ]?key|one[-_ ]?time[-_ ]?code|otp/i.test(field);
}"#;

pub async fn reject_secret_field(cdp: &Cdp, node: u64) -> Result<()> {
    if call_on_node(cdp, node, SECRET_FIELD, Vec::new()).await? == true {
        return Err(error(
            "This field needs the owner. Ask them to use Take over for sign-in.",
        ));
    }
    Ok(())
}

pub async fn select(cdp: &Cdp, node: u64, option: &str) -> Result<()> {
    let result = call_on_node(
        cdp,
        node,
        r#"function (choice) {
        if (this.tagName !== 'SELECT' || this.disabled) return false;
        const index = Array.from(this.options).findIndex(option =>
          !option.disabled && (option.value === choice || option.label === choice));
        if (index < 0) return false;
        this.selectedIndex = index;
        this.dispatchEvent(new Event('input', {bubbles: true}));
        this.dispatchEvent(new Event('change', {bubbles: true}));
        return true;
    }"#,
        vec![json!({"value":option})],
    )
    .await?;
    if result != true {
        return Err(error(
            "No enabled select option matches that label or value",
        ));
    }
    Ok(())
}

async fn call_on_node(
    cdp: &Cdp,
    node: u64,
    function: &str,
    arguments: Vec<Value>,
) -> Result<Value> {
    let result = cdp
        .page("DOM.resolveNode", json!({"backendNodeId":node}))
        .await?;
    let object = required(&result["object"], "objectId")?;
    let result = cdp.page("Runtime.callFunctionOn", json!({
        "objectId":object,"functionDeclaration":function,"arguments":arguments,"returnByValue":true,
    })).await;
    let _ = cdp
        .page("Runtime.releaseObject", json!({"objectId":object}))
        .await;
    let result = result?;
    if !result["exceptionDetails"].is_null() {
        return Err(error(
            "The element is no longer available. Take a new snapshot.",
        ));
    }
    Ok(result["result"]["value"].clone())
}
