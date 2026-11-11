//! A count of every `gh` process Velocity starts for GitHub, so the shared GraphQL and REST
//! budgets can be traced to the feature that spends them (#217). Counting never fails a call.
use std::collections::BTreeMap;
use std::panic::Location;
use std::sync::Mutex;

static TOTALS: Mutex<BTreeMap<(String, String), u64>> = Mutex::new(BTreeMap::new());

/// How many `gh` calls one caller made for one operation since Velocity started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestTotal {
    /// Where the GitHub client was created: `file:line` of its `discover` call.
    pub caller: String,
    /// The `gh` subcommand, and for GraphQL the part of the response it reads.
    pub operation: String,
    pub count: u64,
}

pub(crate) fn record(caller: &Location<'static>, operation: String) {
    // A poisoned lock only means another counting thread panicked; the totals are still usable.
    let mut totals = TOTALS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *totals
        .entry((caller_label(caller), operation))
        .or_insert(0) += 1;
}

/// Every caller's totals, largest first.
pub fn request_totals() -> Vec<RequestTotal> {
    let totals = TOTALS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut rows: Vec<_> = totals
        .iter()
        .map(|((caller, operation), count)| RequestTotal {
            caller: caller.clone(),
            operation: operation.clone(),
            count: *count,
        })
        .collect();
    rows.sort_by(|left, right| right.count.cmp(&left.count));
    rows
}

/// The operation a `gh` argument list performs, without numbers, SHAs or repository names, so
/// one feature's calls add up under one key.
pub(crate) fn operation(arguments: &[&str], graphql_path: Option<&str>) -> String {
    if let Some(path) = graphql_path {
        return format!("graphql {path}");
    }
    match arguments {
        ["api", "user", ..] => "api user".into(),
        ["api", "--method", method, ..] => format!("api {method}"),
        [command, subcommand, ..] => format!("{command} {subcommand}"),
        [command] => (*command).into(),
        [] => String::new(),
    }
}

fn caller_label(caller: &Location<'static>) -> String {
    // `track_caller` reports the path as compiled; keep the last two parts, which name the
    // feature's file, on every platform.
    let parts: Vec<&str> = caller.file().split(['/', '\\']).collect();
    let file = parts[parts.len().saturating_sub(2)..].join("/");
    format!("{file}:{}", caller.line())
}
