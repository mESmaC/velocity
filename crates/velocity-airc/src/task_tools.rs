//! The F8 Task List tools every agent gets: create, read, update and delete.
use crate::protocol::{text, tool};
use crate::{Channel, Error, TaskChange, TaskTarget};
use serde_json::{Value, json};

/// How long a write waits for the store owner's answer. The app drains requests about once
/// a second; past this the agent gets the queued request and finds the answer in read_feed.
const RESULT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

pub(crate) const TASK_TOOLS: &[&str] = &["task_add", "task_list", "task_update", "task_delete"];

pub(crate) fn call(
    channel: &Channel,
    member: &str,
    name: &str,
    arguments: &Value,
) -> Result<Value, Error> {
    let request = match name {
        "task_list" => return Ok(json!({"tasks":channel.task_list()?})),
        "task_add" => channel.request_task(member, text(arguments, "title")?)?,
        "task_update" => channel.request_task_update(member, change(arguments)?)?,
        _ => channel.request_task_delete(member, task(arguments)?)?,
    };
    answer(channel, request)
}

fn change(arguments: &Value) -> Result<TaskChange, Error> {
    let mut change = TaskChange::new(task(arguments)?);
    change.title = arguments
        .get("title")
        .map(|title| {
            title
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::Invalid("title must be a string".into()))
        })
        .transpose()?;
    change.state = arguments
        .get("state")
        .map(|state| match state.as_str() {
            Some("todo") => Ok(TaskTarget::Todo),
            Some("done") => Ok(TaskTarget::Done),
            _ => Err(Error::Invalid("state must be todo or done".into())),
        })
        .transpose()?;
    Ok(change)
}

fn task(arguments: &Value) -> Result<u32, Error> {
    arguments
        .get("task")
        .and_then(Value::as_u64)
        .and_then(|task| u32::try_from(task).ok())
        .ok_or_else(|| Error::Invalid("task must be a task number from task_list".into()))
}

/// Wait briefly for the store owner so the agent learns at once whether its change saved.
fn answer(channel: &Channel, request: u64) -> Result<Value, Error> {
    let started = std::time::Instant::now();
    loop {
        if let Some(result) = channel.task_result(request)? {
            return Ok(json!({"request":request,"result":result}));
        }
        if started.elapsed() >= RESULT_WAIT {
            return Ok(json!({"queued":true,"request":request,
                "result":"Velocity has not answered yet; read_feed shows the task_result for this request"}));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

pub(crate) fn tools() -> Vec<Value> {
    let task =
        json!({"type":"integer","minimum":1,"description":"The task number task_list shows"});
    vec![
        tool(
            "task_list",
            "Read the project Task List: each task's number, title, state (draft, todo, now or done), the agent on it and its live state, and who added it.",
            json!({}),
            &[],
        ),
        tool(
            "task_add",
            "Add a project Task List item ONLY when the user explicitly asks you to add it. Otherwise file an issue. Velocity saves it with your attribution, as a draft before the spec is locked and todo after. Never edits SPEC.md or starts an interview.",
            json!({"title":{"type":"string","minLength":1}}),
            &["title"],
        ),
        tool(
            "task_update",
            "Change a Task List item's title, its state, or both, ONLY when the user explicitly asks you to. state todo reopens a done task; done marks it finished. A task stays a draft until the spec is locked, and a task an agent is on can be marked done but not reopened.",
            json!({"task":task.clone(),"title":{"type":"string","minLength":1},
                "state":{"type":"string","enum":["todo","done"]}}),
            &["task"],
        ),
        tool(
            "task_delete",
            "Remove a Task List item ONLY when the user explicitly asks you to. An agent working on it keeps working; its thread just no longer lists the task.",
            json!({"task":task}),
            &["task"],
        ),
    ]
}
