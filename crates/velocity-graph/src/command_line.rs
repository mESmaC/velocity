//! The standalone indexer (C11): `velocity-graph`, the command Collapse pins and runs, like
//! `git`, to index a commit of a repository it holds and answer the graph tools for it.
//! It reads only git objects, keeps the parse cache in a folder it is given, and prints one
//! JSON object per answer with a versioned schema. The tools' text is the MCP tools' text,
//! made by the same `Tools`, so records, addresses and accuracy rules are the same.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::{Error, Index, Tools};

/// The output schema's version. A field that changes meaning or goes away bumps it; a new
/// field does not.
pub const SCHEMA: u32 = 1;

/// The tools a commit answers. `graph_overlaps`, `graph_grep` and `graph_glob` need a live
/// workspace, so they are not included.
pub const TOOLS: [&str; 5] = [
    "graph_find",
    "graph_read",
    "graph_spec",
    "graph_impact",
    "graph_run",
];

const USAGE: &str = "usage:
  velocity-graph version
  velocity-graph index --repository <path> --commit <revision> --cache <folder>
  velocity-graph query --repository <path> --commit <revision> --cache <folder> <tool> [<json-arguments>]
  velocity-graph serve --repository <path> --cache <folder>
    reads one JSON request per line on stdin: {\"id\"?, \"commit\", \"tool\"?, \"arguments\"?}
    and answers each on one line; a request without a tool indexes the commit";

struct Options {
    repository: PathBuf,
    commit: Option<String>,
    cache: PathBuf,
    rest: Vec<String>,
}

/// Runs the command line `arguments` (without the program name). Exits 0 with an answer, 1
/// with `{"schema", "error"}` when indexing or reading git failed, and 2 on a usage error.
pub fn run(arguments: &[String]) -> ExitCode {
    let Some((command, rest)) = arguments.split_first() else {
        return usage("a command is required");
    };
    match command.as_str() {
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        "version" | "--version" => {
            emit(&version());
            return ExitCode::SUCCESS;
        }
        "index" | "query" | "serve" => {}
        other => return usage(&format!("unknown command `{other}`")),
    }
    let options = match parse(rest, command != "serve") {
        Ok(options) => options,
        Err(message) => return usage(&message),
    };
    let answer = match command.as_str() {
        "index" => index(&options),
        "query" => query(&options),
        _ => return serve(&options),
    };
    match answer {
        Ok(answer) => {
            emit(&answer);
            ExitCode::SUCCESS
        }
        Err(error) => {
            emit(&failure(&error));
            ExitCode::FAILURE
        }
    }
}

fn parse(arguments: &[String], needs_commit: bool) -> Result<Options, String> {
    let (mut repository, mut commit, mut cache) = (None, None, None);
    let mut rest = Vec::new();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let slot = match argument.as_str() {
            "--repository" => &mut repository,
            "--commit" => &mut commit,
            "--cache" => &mut cache,
            _ => {
                rest.push(argument.clone());
                continue;
            }
        };
        let value = arguments
            .next()
            .ok_or_else(|| format!("{argument} needs a value"))?;
        *slot = Some(value.clone());
    }
    let repository = repository.ok_or("--repository is required")?;
    let cache = cache.ok_or("--cache is required")?;
    if needs_commit && commit.is_none() {
        return Err("--commit is required".into());
    }
    Ok(Options {
        repository: PathBuf::from(repository),
        commit,
        cache: PathBuf::from(cache),
        rest,
    })
}

fn version() -> Value {
    json!({
        "schema": SCHEMA,
        // A release build carries its tag's version; a local build, the crate's.
        "indexer": option_env!("VELOCITY_GRAPH_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")),
        "parser": crate::cache::PARSER_VERSION,
        "tools": TOOLS,
    })
}

fn open(options: &Options) -> Result<Index, Error> {
    let commit = options.commit.as_deref().unwrap_or("HEAD");
    Index::open_git(&options.repository, commit, &options.cache)
}

fn index(options: &Options) -> Result<Value, Error> {
    if let Some(extra) = options.rest.first() {
        return Err(Error::Invalid(format!("unexpected argument `{extra}`")));
    }
    Ok(report(&open(options)?))
}

fn query(options: &Options) -> Result<Value, Error> {
    let Some((tool, rest)) = options.rest.split_first() else {
        return Err(Error::Invalid(format!(
            "query needs a tool: {}",
            TOOLS.join(", ")
        )));
    };
    let arguments = match rest {
        [] => json!({}),
        [arguments] => serde_json::from_str(arguments)?,
        [_, extra, ..] => return Err(Error::Invalid(format!("unexpected argument `{extra}`"))),
    };
    call(&mut Tools::new(open(options)?), tool, &arguments)
}

/// What indexing the commit found. `parsed` and `cached` count this update's parses made and
/// reused, and `skipped` the files it read but left unparsed, by reason.
fn report(index: &Index) -> Value {
    let records = &index.graph.records;
    let plane = |plane: u8| {
        records
            .iter()
            .filter(|record| record.kind.plane() == plane)
            .count()
    };
    json!({
        "schema": SCHEMA,
        "commit": index.commit(),
        "files": plane(2),
        "symbols": plane(1),
        "subsystems": plane(3),
        "parsed": index.report.parsed,
        "cached": index.report.cached,
        "milliseconds": index.report.milliseconds,
        "skipped": index.skipped,
    })
}

/// One tool answer. `ok: false` is a classified failure the caller should read (ambiguous,
/// missing, a bad argument), with the same text the MCP tool gives.
fn call(tools: &mut Tools, tool: &str, arguments: &Value) -> Result<Value, Error> {
    if !TOOLS.contains(&tool) {
        return Err(Error::Invalid(format!(
            "unknown tool `{tool}`; expected one of {}",
            TOOLS.join(", ")
        )));
    }
    let (ok, text) = match tools.call(tool, arguments) {
        Ok(text) => (true, text),
        Err(text) => (false, text),
    };
    Ok(json!({
        "schema": SCHEMA,
        "commit": tools.index().commit(),
        "tool": tool,
        "ok": ok,
        "text": text,
    }))
}

/// Answers requests on stdin with one warm index, moving it between commits so only files
/// whose blobs changed are read and only cache misses are parsed.
fn serve(options: &Options) -> ExitCode {
    let mut tools = None;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else {
            return ExitCode::FAILURE;
        };
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(request) => respond(&mut tools, options, &request),
            Err(error) => failure(&Error::Json(error)),
        };
        if !emit(&answer) {
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

fn respond(tools: &mut Option<Tools>, options: &Options, request: &Value) -> Value {
    let mut answer =
        answer_request(tools, options, request).unwrap_or_else(|error| failure(&error));
    if let Some(id) = request.get("id") {
        answer["id"] = id.clone();
    }
    answer
}

fn answer_request(
    tools: &mut Option<Tools>,
    options: &Options,
    request: &Value,
) -> Result<Value, Error> {
    let commit = request["commit"]
        .as_str()
        .ok_or_else(|| Error::Invalid("`commit` is required".into()))?;
    if let Some(warm) = tools.as_mut() {
        warm.select_commit(&options.repository, commit)?;
    } else {
        let index = Index::open_git(&options.repository, commit, &options.cache)?;
        *tools = Some(Tools::new(index));
    }
    let Some(tools) = tools.as_mut() else {
        return Err(Error::Invalid("the index did not open".into()));
    };
    match request["tool"].as_str() {
        None => Ok(report(tools.index())),
        Some(tool) => {
            let arguments = match &request["arguments"] {
                Value::Null => json!({}),
                arguments => arguments.clone(),
            };
            call(tools, tool, &arguments)
        }
    }
}

fn failure(error: &Error) -> Value {
    json!({"schema": SCHEMA, "error": error.to_string()})
}

fn usage(message: &str) -> ExitCode {
    emit(&json!({"schema": SCHEMA, "error": message}));
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

/// Prints one answer on its own line. Returns false when stdout is closed.
fn emit(answer: &Value) -> bool {
    let mut output = std::io::stdout().lock();
    writeln!(output, "{answer}")
        .and_then(|()| output.flush())
        .is_ok()
}
