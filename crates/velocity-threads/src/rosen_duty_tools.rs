//! The tools PR duty's Rosen MCP server lists (F5, F7), as the reviewing agent reads them.

use serde_json::{Value, json};

/// A review by reading (`reading`) gets only the tools that read the change and record a
/// decision: Velocity checks and lands the head it approves.
pub(crate) fn tool_list(change: &str, reading: bool) -> Vec<Value> {
    let string = json!({"type": "string"});
    let head = json!({"type": "string", "description": "The change's full head id you checked. Velocity refuses it if the change has moved."});
    let none = json!({"type": "object", "properties": {}, "additionalProperties": false});
    let read = json!({
        "name": "rosen_duty_change",
        "description": format!("Rosen change {change} as Rosen has it now: its head, its author, its named snapshots (the author's description of the work), its review threads and its diff against its base."),
        "inputSchema": none,
    });
    if reading {
        let approves = "approve records Rosen's approval at that head; Velocity then runs the pre-land checks on it and lands it";
        return vec![read, review_tool(change, approves, &string, &head)];
    }
    let approves = "approve records Rosen's approval at that head (after rosen_duty_checked on it)";
    vec![
        read,
        json!({
            "name": "rosen_duty_prepare_landing",
            "description": "Write the change's current merge result onto its base into your working folder, replacing what was there, and give it the folder setup. Returns head, base and conflicts. Run the local checks there afterwards. On conflicts, request changes naming the files.",
            "inputSchema": none,
        }),
        json!({
            "name": "rosen_duty_checked",
            "description": "Record that the local checks passed on the merge result rosen_duty_prepare_landing returned: submits the receipt Rosen needs to land it. Pass that exact head and base.",
            "inputSchema": {"type": "object", "properties": {"head": head, "base": string}, "required": ["head", "base"], "additionalProperties": false},
        }),
        review_tool(change, approves, &string, &head),
        json!({
            "name": "rosen_duty_land",
            "description": format!("Land change {change} at the head you approved. Rosen refuses unless it holds your approval at exactly that head and a receipt for the current merge result."),
            "inputSchema": {"type": "object", "properties": {"head": head}, "required": ["head"], "additionalProperties": false},
        }),
    ]
}

fn review_tool(change: &str, approves: &str, string: &Value, head: &Value) -> Value {
    json!({
        "name": "rosen_duty_review",
        "description": format!("Record your decision on change {change}. {approves}. request_changes sends your reason and comments to the author's agent. comment only posts line comments. hold leaves the change undecided because a comment says to wait: PR duty skips this head until it moves. A reason is required."),
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["approve", "request_changes", "comment", "hold"]},
                "head": head,
                "reason": string,
                "proof_review": {
                    "type": "object",
                    "description": "Required for approve. Explain scope against the project proof list (missing required design proofs or off-list Lean block approval); statement correctness, hypotheses, cost model and alternatives; confidence level and tier requirements. Explain inapplicability for ordinary changes.",
                    "properties": {"scope": string, "statements": string, "confidence": string},
                    "required": ["scope", "statements", "confidence"],
                    "additionalProperties": false
                },
                "comments": {
                    "type": "array",
                    "description": "Line comments on the change's head, each opening a Rosen review thread.",
                    "items": {
                        "type": "object",
                        "properties": {"path": string, "line": {"type": "integer"}, "body": string},
                        "required": ["path", "line", "body"],
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["action", "head", "reason"],
            "additionalProperties": false,
        },
    })
}
