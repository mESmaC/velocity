//! The `preflight` tool (#537), served by the same stdio subprocess as `queue_run`.
//! One call inspects the change's diff and runs only the checks that apply before naming.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::queued_run_tool::{REQUEST_TIMEOUT, Request, Work, submit};

pub const PREFLIGHT_TOOL: &str = "preflight";

const DESCRIPTION: &str = "Run the checks this change needs before naming it with `rosen commit -m`. \
    Velocity reads the change's diff and runs only what applies: `rustfmt --check` on changed \
    Rust files only, UI type-check and lint when `ui/` changed, queued \
    `cargo clippy -p <crate> --all-targets -- -D warnings` for every touched Rust crate, active \
    overlaps with symbol analysis (`graph_overlaps`), and generated-file freshness checks \
    (Cargo.lock, ui/pnpm-lock.yaml, ui/src/lib/bindings.ts and Rosen base regeneration). \
    Velocity records a passing preflight (and a Rosen receipt on after-land projects) for the \
    checked snapshot and refuses to name or land a change whose latest snapshot has no passing \
    preflight. After preflight queues, end your turn. Velocity sends one pass/fail report with \
    excerpts from any failing step and the full log path in your next prompt.";

#[derive(Default, Deserialize, Serialize)]
pub(super) struct PreflightRequest {}

pub fn preflight_tool() -> Value {
    json!({
        "name": PREFLIGHT_TOOL,
        "description": DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {}
        }
    })
}

pub fn request_preflight(port: u16, token: &str, arguments: Value) -> Result<Value, String> {
    if !arguments.is_null() {
        let _: PreflightRequest =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
    }
    let request = Request {
        token: token.to_owned(),
        work: Work::Preflight(PreflightRequest {}),
    };
    submit::<PathBuf>(port, &request, REQUEST_TIMEOUT)
        .map_err(|error| format!("Could not queue preflight: {error}"))?
        .map(|path| {
            json!({
                "queued": true,
                "log": path,
                "next": "End your turn. Velocity will send the preflight report when it finishes."
            })
        })
}
