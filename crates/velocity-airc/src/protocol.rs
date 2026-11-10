use crate::{Channel, Error};
use serde_json::{Value, json};
use std::io::{BufRead, Write};

/// Serve one agent connection. Identity is supplied by the launcher, not tool arguments.
pub fn serve(
    channel: &Channel,
    member: &str,
    requested_nick: &str,
    input: impl BufRead,
    mut output: impl Write,
) -> Result<(), Error> {
    let _connection = channel.connect(member)?;
    let mut initialized = false;
    let result = (|| {
        for line in input.lines() {
            let line = line?;
            let response = match serde_json::from_str::<Value>(&line) {
                Ok(request) => respond(channel, member, requested_nick, &request, &mut initialized),
                Err(_) => Some(failure(Value::Null, -32700, "Parse error")),
            };
            if let Some(response) = response {
                serde_json::to_writer(&mut output, &response)?;
                writeln!(output)?;
                output.flush()?;
            }
        }
        Ok(())
    })();
    let cleanup = if initialized {
        channel.leave(member)
    } else {
        Ok(())
    };
    result.and(cleanup)
}

fn respond(
    channel: &Channel,
    member: &str,
    nick: &str,
    request: &Value,
    initialized: &mut bool,
) -> Option<Value> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(Value::as_str);
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || method.is_none() {
        return Some(failure(id, -32600, "Invalid request"));
    }
    request.get("id")?;
    let result = match method.unwrap_or_default() {
        "initialize" => match channel.join(member, nick) {
            Ok(nick) => {
                *initialized = true;
                json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},
                        "serverInfo":{"name":"velocity-airc","version":env!("CARGO_PKG_VERSION")},
                        "instructions":format!("You joined this project's AIRC channel as {nick}. Read airc_claims before choosing work. Call airc_claim with the task, branch and every file BEFORE editing, and with piece set to the work item's identity (task:<id>, issue:<number> or spec:<section>) when it has one; on refusal the holder is named, so re-read airc_claims and claim the first open piece. Extend your existing task claim to add files. Record airc_share or a dependency wait before editing an overlapping file. Messages require named recipients and an intent.")})
            }
            Err(error) => return Some(failure(id, -32603, &error.to_string())),
        },
        "ping" => json!({}),
        _ if !*initialized => return Some(failure(id, -32600, "Initialize first")),
        "tools/list" => json!({"tools":tool_list()}),
        "tools/call" => {
            let parameters = &request["params"];
            match call(channel, member, nick, parameters) {
                Ok(value) => json!({"content":[{"type":"text","text":value.to_string()}]}),
                Err(error) => {
                    json!({"isError":true,"content":[{"type":"text","text":error.to_string()}]})
                }
            }
        }
        _ => return Some(failure(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

fn call(channel: &Channel, member: &str, nick: &str, parameters: &Value) -> Result<Value, Error> {
    let arguments = &parameters["arguments"];
    let after = arguments
        .get("after")
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| Error::Invalid("after must be an unsigned integer".into()))
        })
        .transpose()?
        .unwrap_or(0);
    match text(parameters, "name")? {
        name @ ("airc_claims" | "airc_claim" | "airc_release" | "airc_share") => {
            claim_call(channel, member, name, arguments)
        }
        "join" => Ok(json!({"nick":channel.join(member, nick)?})),
        "leave" => {
            channel.leave(member)?;
            Ok(json!({"left":true}))
        }
        "list_nicks" => Ok(serde_json::to_value(channel.presence()?)?),
        "read_feed" => Ok(serde_json::to_value(channel.read(after)?)?),
        name @ ("read_events" | "read_messages" | "read_ledger") => {
            read_filtered(channel, name, arguments, after)
        }
        "append_ledger" => {
            channel.append_ledger(member, ledger(arguments)?, text(arguments, "text")?)?;
            Ok(json!({"appended":true}))
        }
        "send_message" => {
            let recipients = serde_json::from_value(arguments["to"].clone())?;
            channel.message(
                member,
                recipients,
                text(arguments, "intent")?,
                text(arguments, "text")?,
            )?;
            Ok(json!({"sent":true}))
        }
        _ => Err(Error::Invalid("Unknown AIRC tool".into())),
    }
}

fn read_filtered(
    channel: &Channel,
    name: &str,
    arguments: &Value,
    after: u64,
) -> Result<Value, Error> {
    let entries = match name {
        "read_events" => channel.events(after)?,
        "read_messages" => channel.messages(after)?,
        _ => channel.ledger(ledger(arguments)?, after)?,
    };
    Ok(serde_json::to_value(entries)?)
}

fn tool_list() -> Vec<Value> {
    let empty = json!({});
    let cursor = json!({"after":{"type":"integer","minimum":0}});
    let mut tools = vec![
        tool(
            "join",
            "Join this project channel using the launcher's identity",
            empty.clone(),
            &[],
        ),
        tool("leave", "Leave the project channel", empty.clone(), &[]),
        tool(
            "list_nicks",
            "Read active and departed project members",
            empty,
            &[],
        ),
        tool(
            "read_feed",
            "Read the project log after a sequence number",
            cursor.clone(),
            &[],
        ),
        tool(
            "read_events",
            "Read project events after a sequence number",
            cursor.clone(),
            &[],
        ),
        tool(
            "read_messages",
            "Read project coordination messages",
            cursor,
            &[],
        ),
    ];
    tools.extend(writing_tools());
    tools.extend(claim_tools());
    tools
}

fn writing_tools() -> Vec<Value> {
    let ledger = json!({"type":"string","enum":["discovery","past_work"]});
    let string = json!({"type":"string","minLength":1});
    vec![
        tool(
            "read_ledger",
            "Read Discoveries or Past work",
            json!({"ledger":ledger.clone(),
            "after":{"type":"integer","minimum":0}}),
            &["ledger"],
        ),
        tool(
            "append_ledger",
            "Record a discovery or past work",
            json!({"ledger":ledger,"text":string.clone()}),
            &["ledger", "text"],
        ),
        tool(
            "send_message",
            "Send coordination words to named recipients; never broadcast",
            json!({"to":{"type":"array","minItems":1,"items":string.clone()},
                "intent":{"type":"string","enum":["inform","request","question"]},"text":string}),
            &["to", "intent", "text"],
        ),
    ]
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"name":name,"description":description,"inputSchema":{
        "type":"object","properties":properties,"required":required,"additionalProperties":false}})
}

fn ledger(arguments: &Value) -> Result<&str, Error> {
    match text(arguments, "ledger")? {
        name @ ("discovery" | "past_work") => Ok(name),
        _ => Err(Error::Invalid(
            "Ledger must be discovery or past_work".into(),
        )),
    }
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Invalid(format!("Missing string: {key}")))
}

fn failure(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

fn claim_tools() -> Vec<Value> {
    let string = json!({"type":"string","minLength":1});
    vec![
        tool(
            "airc_claims",
            "Read held piece and file claims",
            json!({}),
            &[],
        ),
        tool(
            "airc_claim",
            "Claim or extend your piece BEFORE editing. First claimant wins; a refusal names the holder. piece locks a work item by identity: task:<id>, issue:<number> or spec:<section>; without it the task text is the lock. Files are repository-relative; use ::registration for shared registration lists.",
            json!({"piece":string,"task":string,"branch":string,"files":{"type":"array","items":string}}),
            &["task", "branch", "files"],
        ),
        tool(
            "airc_release",
            "Release your own piece, named by its piece or task, after it merges or is deleted",
            json!({"task":string}),
            &["task"],
        ),
        tool(
            "airc_share",
            "Record a share before editing an overlapping file",
            json!({"holder":string,"file":string}),
            &["holder", "file"],
        ),
    ]
}

fn claim_call(
    channel: &Channel,
    member: &str,
    name: &str,
    arguments: &Value,
) -> Result<Value, Error> {
    match name {
        "airc_claims" => Ok(serde_json::to_value(channel.claims()?)?),
        "airc_claim" => {
            let files = serde_json::from_value(arguments["files"].clone())?;
            let piece = arguments
                .get("piece")
                .map(|value| {
                    value
                        .as_str()
                        .ok_or_else(|| Error::Invalid("piece must be a string".into()))
                })
                .transpose()?
                .unwrap_or_default();
            Ok(serde_json::to_value(channel.claim_piece(
                member,
                piece,
                text(arguments, "task")?,
                text(arguments, "branch")?,
                files,
            )?)?)
        }
        "airc_release" => {
            channel.release(member, text(arguments, "task")?)?;
            Ok(json!({"released":true}))
        }
        "airc_share" => {
            channel.share(member, text(arguments, "holder")?, text(arguments, "file")?)?;
            Ok(json!({"shared":true}))
        }
        _ => Err(Error::Invalid("Unknown claim tool".into())),
    }
}
