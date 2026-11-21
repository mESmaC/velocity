use crate::{
    Channel, Error,
    issue_tools::{self, IssueHandler},
    next_work_tools::{self, NextWorkHandler},
    read_tools::{self, MessageReader},
    remote,
};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::Path;
use std::time::Instant;
use velocity_core::issue_dependencies::{
    IssueReference, blocker_list, parse_blockers, with_declared_blockers,
};
use velocity_core::text_budget::tool_answer;
use velocity_rosen::mcp_latency::LatencyRecorder;

#[derive(Default)]
pub(crate) struct ToolOptions<'a> {
    continuous_dispatch: bool,
    issue_handler: Option<&'a IssueHandler<'a>>,
    app_data: Option<&'a Path>,
    next_work_handler: Option<&'a NextWorkHandler<'a>>,
}

impl<'a> ToolOptions<'a> {
    pub(crate) fn new(
        continuous_dispatch: bool,
        issue_handler: Option<&'a IssueHandler<'a>>,
        app_data: Option<&'a Path>,
    ) -> Self {
        Self {
            continuous_dispatch,
            issue_handler,
            app_data,
            next_work_handler: None,
        }
    }

    pub(crate) fn with_next_work(mut self, handler: &'a NextWorkHandler<'a>) -> Self {
        self.next_work_handler = Some(handler);
        self
    }
}

/// Serve one agent connection. Identity is supplied by the launcher, not tool arguments.
pub fn serve(
    channel: &Channel,
    member: &str,
    requested_nick: &str,
    input: impl BufRead + Send + 'static,
    output: impl Write + Send + 'static,
) -> Result<(), Error> {
    serve_with_options(
        channel,
        member,
        requested_nick,
        input,
        output,
        ToolOptions::default(),
    )
}

pub(crate) fn serve_with_options(
    channel: &Channel,
    member: &str,
    requested_nick: &str,
    input: impl BufRead + Send + 'static,
    output: impl Write + Send + 'static,
    options: ToolOptions<'_>,
) -> Result<(), Error> {
    let _connection = channel.connect(member)?;
    let connected = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let output = std::sync::Arc::new(std::sync::Mutex::new(output));
    let receiver = read_requests(input, connected.clone(), output.clone());
    let latency = LatencyRecorder::open(crate::SERVER_NAME, options.app_data);
    let mut session = ToolSession::default();
    let result = (|| {
        for line in receiver {
            let line = line?;
            let response = match serde_json::from_str::<Value>(&line) {
                Ok(request) => respond(
                    channel,
                    member,
                    requested_nick,
                    &request,
                    &mut session,
                    &connected,
                    &options,
                    &latency,
                ),
                Err(_) => Some(failure(Value::Null, -32700, "Parse error")),
            };
            if let Some(response) = response {
                write_response(&output, &response)?;
            }
        }
        Ok(())
    })();
    let cleanup = if session.nick.is_some() {
        channel.leave(member)
    } else {
        Ok(())
    };
    result.and(cleanup)
}

fn instructions(nick: &str, continuous_dispatch: bool, has_issue_tools: bool) -> String {
    let task_rule = if continuous_dispatch {
        "Continuous dispatch started this thread. Use task_add, task_update and task_delete without waiting for a user request. Record concrete follow-up work and split work into actionable tasks. Velocity marks your assigned task done when your change lands, so leave it assigned while you work."
    } else {
        "Use task_add, task_update and task_delete only when the user explicitly asks you to add, change or remove Task List items. Otherwise file issues."
    };
    let issue_rule = if has_issue_tools {
        " Use issue_read to inspect any issue in this project's repository and issue_create to file spec questions, contradictions or findings on this project's repository."
    } else {
        ""
    };
    format!(
        "You joined this project's AIRC channel as {nick}. Check ownership with airc_claims using the candidate task or piece as the task filter. Call airc_claim with the task, branch and every file BEFORE editing, and with piece set to the work item's identity (task:<id>, issue:<number> or spec:<section>) when it has one; on refusal the holder is named, so check the next candidate. Extend your existing task claim to add files. Record airc_share or a dependency wait before editing an overlapping file. Messages require named recipients and an intent. Log readers (read_feed, read_events, read_ledger) return newest first (up to 50), filter by query or after/before sequence, return next_before for older matches, and read oversized entries by sequence with offset. task_list reads the project Task List at any time. {task_rule} Task List changes never edit SPEC.md or start an interview.{issue_rule} Use append_ledger with velocity_issue only to report a problem in the Velocity IDE/tooling running your session (its worktrees, AIRC, duty tools, agent shells, build queue, code graph or desktop app), which Velocity files on mESmaC/velocity regardless of which project you are in; for issues on this project's repository (including when this project is mESmaC/velocity), use issue_create instead. A discovery about Rosen itself (its server, workspaces, durability, claims, rebases or landing) uses rosen_issue instead and is filed on mESmaC/rosen. Use at most one ledger issue field. Search read_ledger with ledger discovery and the problem's terms before reporting it. An issue that waits on other issues names them in blocked_by, which records GitHub issue dependencies; never hold it with a label."
    )
}

#[derive(Default)]
struct ToolSession {
    nick: Option<String>,
    messages: MessageReader,
}

#[allow(clippy::too_many_arguments)]
fn respond(
    channel: &Channel,
    member: &str,
    nick: &str,
    request: &Value,
    session: &mut ToolSession,
    connected: &std::sync::atomic::AtomicBool,
    options: &ToolOptions<'_>,
    latency: &LatencyRecorder,
) -> Option<Value> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(Value::as_str);
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || method.is_none() {
        return Some(failure(id, -32600, "Invalid request"));
    }
    request.get("id")?;
    let result = match method.unwrap_or_default() {
        "initialize" => {
            match initialize_session(channel, member, nick, session, options, latency) {
                Ok(value) => value,
                Err(error) => return Some(failure(id, -32603, &error.to_string())),
            }
        }
        "ping" => json!({}),
        _ if session.nick.is_none() => return Some(failure(id, -32600, "Initialize first")),
        "tools/list" => match channel.is_rosen_project(member) {
            Ok(rosen) => json!({"tools":tool_list(rosen, options)}),
            Err(error) => return Some(failure(id, -32603, &error.to_string())),
        },
        "tools/call" => call_tool(
            channel,
            member,
            nick,
            &request["params"],
            connected,
            options,
            session,
            latency,
        ),
        _ => return Some(failure(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

fn initialize_session(
    channel: &Channel,
    member: &str,
    nick: &str,
    session: &mut ToolSession,
    options: &ToolOptions<'_>,
    latency: &LatencyRecorder,
) -> Result<Value, Error> {
    remote::reset_connect_duration();
    let started = Instant::now();
    let joined = channel.join_with_session(member, nick, true);
    latency.record(
        "initialize",
        remote::take_connect_duration(),
        started.elapsed(),
        joined.is_ok(),
    );
    let nick = joined?;
    session.nick = Some(nick.clone());
    // Pre-warm the incremental remote event cache before the first tool call (#541).
    if let Some(remote) = channel.remote.clone() {
        let _ = std::thread::Builder::new()
            .name("airc-warm".into())
            .spawn(move || {
                let _ = remote.warm();
            });
    }
    Ok(json!({
        "protocolVersion":"2025-11-25",
        "capabilities":{"tools":{}},
        "serverInfo":{"name":"velocity-airc","version":env!("CARGO_PKG_VERSION")},
        "instructions":instructions(
            &nick,
            options.continuous_dispatch,
            options.issue_handler.is_some(),
        ),
    }))
}

#[allow(clippy::too_many_arguments)]
fn call_tool(
    channel: &Channel,
    member: &str,
    nick: &str,
    parameters: &Value,
    connected: &std::sync::atomic::AtomicBool,
    options: &ToolOptions<'_>,
    session: &mut ToolSession,
    latency: &LatencyRecorder,
) -> Value {
    let tool_name = parameters["name"].as_str().unwrap_or("unknown");
    remote::reset_connect_duration();
    let started = Instant::now();
    let (text, failed) = match call(
        channel, member, nick, parameters, connected, options, session,
    ) {
        Ok(value) => (value.to_string(), false),
        Err(error) => (error.to_string(), true),
    };
    latency.record(
        tool_name,
        remote::take_connect_duration(),
        started.elapsed(),
        !failed,
    );
    let text = tool_answer(
        text,
        "Use a query, a smaller limit, or one sequence or task when the tool supports it.",
    );
    json!({"isError":failed,"content":[{"type":"text","text":text}]})
}

fn call(
    channel: &Channel,
    member: &str,
    nick: &str,
    parameters: &Value,
    connected: &std::sync::atomic::AtomicBool,
    options: &ToolOptions<'_>,
    session: &mut ToolSession,
) -> Result<Value, Error> {
    let arguments = &parameters["arguments"];
    match text(parameters, "name")? {
        name @ ("airc_claims" | "airc_claim" | "airc_release" | "airc_share") => {
            claim_call(channel, member, name, arguments)
        }
        "airc_rebase" => {
            channel.rebase(member)?;
            Ok(json!({"rebased":true}))
        }
        "airc_wait" => channel.wait_until(
            member,
            text(arguments, "holder")?,
            text(arguments, "file")?,
            || !connected.load(std::sync::atomic::Ordering::Acquire),
        ),
        name if crate::task_tools::TASK_TOOLS.contains(&name) => {
            crate::task_tools::call(channel, member, name, arguments)
        }
        name if issue_tools::ISSUE_TOOLS.contains(&name) => issue_tools::call(
            channel,
            member,
            session.nick.as_deref().unwrap_or(nick),
            name,
            arguments,
            options.issue_handler,
        ),
        next_work_tools::NEXT_WORK_TOOL => next_work_tools::call(
            channel,
            member,
            session.nick.as_deref().unwrap_or(nick),
            arguments,
            options.next_work_handler,
        ),
        "join" => Ok(json!({"nick":channel.join_with_session(member, nick, true)?})),
        "leave" => {
            channel.leave(member)?;
            Ok(json!({"left":true}))
        }
        "list_nicks" => Ok(serde_json::to_value(channel.presence()?)?),
        "read_messages" => session.messages.read(
            channel,
            member,
            session.nick.as_deref().unwrap_or(nick),
            arguments,
        ),
        name @ ("read_feed" | "read_events" | "read_ledger") => {
            read_tools::log(channel, name, arguments)
        }
        "append_ledger" => append_ledger(channel, member, arguments),
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

fn append_ledger(channel: &Channel, member: &str, arguments: &Value) -> Result<Value, Error> {
    let title = |name: &str| {
        arguments
            .get(name)
            .and_then(Value::as_str)
            .filter(|title| !title.trim().is_empty())
            .map(str::to_owned)
    };
    let velocity_issue = title("velocity_issue");
    let rosen_issue = title("rosen_issue");
    let reported_velocity = velocity_issue.is_some();
    let reported_rosen = rosen_issue.is_some();
    let blockers = report_blockers(arguments, reported_velocity || reported_rosen)?;
    // The blockers ride on the text's closing line, which the filing edge links (#511).
    let entry = with_declared_blockers(text(arguments, "text")?, &blockers);
    channel.append_ledger_report(
        member,
        ledger(arguments)?,
        &entry,
        velocity_issue,
        rosen_issue,
    )?;
    Ok(
        json!({"appended":true,"velocity_issue":reported_velocity,"rosen_issue":reported_rosen,
        "blocked_by":blocker_list(&blockers)}),
    )
}

/// The issues a reported discovery waits on. Each names its repository: the report is
/// filed on Velocity's or Rosen's repository, not the project the agent works in.
fn report_blockers(arguments: &Value, reported: bool) -> Result<Vec<IssueReference>, Error> {
    let names = issue_tools::blocked_by(arguments)?;
    if !names.is_empty() && !reported {
        return Err(Error::Invalid(
            "blocked_by goes with velocity_issue or rosen_issue".into(),
        ));
    }
    let blockers = parse_blockers(&names).map_err(Error::Invalid)?;
    if blockers.iter().any(|blocker| !blocker.names_repository()) {
        return Err(Error::Invalid(
            "Name each blocker's repository, as owner/name#12".into(),
        ));
    }
    Ok(blockers)
}

fn tool_list(rosen: bool, options: &ToolOptions<'_>) -> Vec<Value> {
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
            "Read the project feed (newest first, filterable by query, sequence or kind; pass kind=\"events\" for event-log entries only).",
            read_tools::feed_fields(),
            &[],
        ),
        tool(
            "read_messages",
            "Read up to 10 unread messages addressed to you, with 512-byte previews. Call again while more_unread is true. For a longer message, pass its sequence and next_offset as offset to read the next part.",
            json!({"after":cursor["after"], "sequence":{"type":"integer","minimum":1},
                "offset":{"type":"integer","minimum":0}}),
            &[],
        ),
    ];
    tools.extend(crate::task_tools::tools(options.continuous_dispatch));
    if options.issue_handler.is_some() {
        tools.extend(issue_tools::tools());
    }
    if options.next_work_handler.is_some() {
        tools.push(next_work_tools::tool());
    }
    tools.extend(writing_tools());
    tools.extend(claim_tools());
    if !rosen {
        tools.push(tool("airc_rebase", "Bring your registered branch up to date without rewriting history. Commit current edits first, then call this on a targeted merge notice or at landing. Velocity posts rebased on success or conflict on failure; resolve conflicts in your own worktree, regenerate artifacts and check before pushing.", json!({}), &[]));
    }
    tools.push(tool("airc_wait", "Wait for a file holder. This call stays pending until its dependency ends; do not edit meanwhile. Velocity may park you while you wait: it ends your session to free a provider slot and resumes it when the dependency ends, with the outcome as your next message. On merge Velocity updates your branch before you continue; resolve any reported conflict before continuing.", json!({"holder":{"type":"string"},"file":{"type":"string"}}), &["holder", "file"]));
    tools
}

fn writing_tools() -> Vec<Value> {
    let ledger = json!({"type":"string","enum":["discovery","past_work"]});
    let mut fields = read_tools::log_fields();
    fields["ledger"] = ledger.clone();
    let string = json!({"type":"string","minLength":1});
    vec![
        tool(
            "read_ledger",
            "Read Discoveries or Past work (newest first, filterable by query or sequence).",
            fields,
            &["ledger"],
        ),
        tool(
            "append_ledger",
            "Record a discovery or past work in the project ledger. To file an issue on this project's own repository (including spec questions, contradictions or code findings), use issue_create instead. Pass velocity_issue (a one-line title) only when a discovery reports a problem in the Velocity IDE/tooling running your session (its worktrees and paths, AIRC, the review swarm or Scout tools, agent shells and sandboxes, the build queue, the code graph, or the desktop app); Velocity files it on mESmaC/velocity (or adds it to a matching open issue) regardless of which project you are in. Pass rosen_issue instead for a problem in Rosen itself (its server, workspaces, durability, claims, rebases or landing) to file on mESmaC/rosen. Use at most one issue field, and leave both out when another agent already reported the same problem or when you already filed it with issue_create. When velocity_issue or rosen_issue waits on other issues, name them in blocked_by as owner/name#12 and Velocity links them as GitHub issue dependencies.",
            json!({"ledger":ledger,"text":string.clone(),
                "blocked_by":{"type":"array","items":{"type":"string"},
                    "description":"Only with velocity_issue or rosen_issue: issues that must close first, each as owner/name#12"},
                "velocity_issue":{"type":"string","minLength":1,"maxLength":200,
                    "description":"One-line title to file this tooling discovery on mESmaC/velocity (for problems in the Velocity IDE/tooling running your session; use issue_create for this project's own issues, including when this project is mESmaC/velocity)"},
                "rosen_issue":{"type":"string","minLength":1,"maxLength":200,
                    "description":"One-line title to file this discovery on mESmaC/rosen (for problems in Rosen itself; cannot be combined with velocity_issue)"}}),
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

pub(crate) fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"name":name,"description":description,"inputSchema":{
        "type":"object","properties":properties,"required":required,"additionalProperties":false}})
}

pub(crate) fn ledger(arguments: &Value) -> Result<&str, Error> {
    match text(arguments, "ledger")? {
        name @ ("discovery" | "past_work") => Ok(name),
        _ => Err(Error::Invalid(
            "Ledger must be discovery or past_work".into(),
        )),
    }
}

pub(crate) fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
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
            "Read matching claims by repository-relative path or exact task text or piece key (both filters must match when supplied). Without filters, get counts by agent, your own claims and claims overlapping your files. Pass next_offset as offset for more summary rows.",
            json!({"path":string, "task":string, "offset":{"type":"integer","minimum":0}}),
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
        "airc_claims" => read_tools::claims(channel, member, arguments),
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

// Keep answering transport heartbeats while a dependency tool is pending.
// EOF cancels that tool before the server releases its connection lock.
fn read_requests(
    input: impl BufRead + Send + 'static,
    connected: std::sync::Arc<std::sync::atomic::AtomicBool>,
    output: std::sync::Arc<std::sync::Mutex<impl Write + Send + 'static>>,
) -> std::sync::mpsc::Receiver<std::io::Result<String>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in input.lines() {
            if let Ok(text) = &line
                && let Ok(request) = serde_json::from_str::<Value>(text)
                && request["jsonrpc"] == "2.0"
                && request["method"] == "ping"
                && request.get("id").is_some()
            {
                let reply = json!({"jsonrpc":"2.0","id":request["id"],"result":{}});
                if write_response(&output, &reply).is_err() {
                    break;
                }
                continue;
            }
            if sender.send(line).is_err() {
                break;
            }
        }
        connected.store(false, std::sync::atomic::Ordering::Release);
    });
    receiver
}

fn write_response(output: &std::sync::Mutex<impl Write>, response: &Value) -> Result<(), Error> {
    let mut output = output
        .lock()
        .map_err(|error| Error::Invalid(error.to_string()))?;
    serde_json::to_writer(&mut *output, response)?;
    writeln!(output)?;
    output.flush()?;
    Ok(())
}
