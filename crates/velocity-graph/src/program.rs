//! `graph_run`: pointer programs over one register (F19, after A.TXY's N6 programs). Several
//! hops run in one tool call, with a one-line audit per step.

use std::collections::{HashSet, VecDeque};
use std::fmt::Write;

use crate::graph::{Graph, Provenance, Relation};
use crate::query::{Lookup, failure, line, lookup, short};
use crate::search::Search;

const STEP_LIMIT: usize = 16;
/// Records a set may hold before later steps refuse to fan out further.
const SET_LIMIT: usize = 2_000;

enum Register {
    Empty,
    Set(Vec<usize>),
    Count(usize),
}

/// What a program produced: its audit and result, and the files its records came from (for
/// the staleness check).
pub struct Outcome {
    pub text: String,
    pub files: Vec<String>,
    /// `SOURCE` asked for these records' lines; the caller reads them after the check.
    pub source: Vec<usize>,
}

pub fn run(graph: &Graph, search: &Search, program: &str) -> Result<Outcome, String> {
    let steps = split_steps(program);
    if steps.is_empty() || steps.len() > STEP_LIMIT {
        return Err(format!(
            "a program has 1 to {STEP_LIMIT} steps separated by `;` or new lines"
        ));
    }
    let mut register = Register::Empty;
    let mut audit = String::new();
    let mut source = Vec::new();
    for (number, step) in steps.iter().enumerate() {
        let words = tokens(step);
        let Some(operation) = words.first().map(|word| word.to_ascii_uppercase()) else {
            continue;
        };
        let arguments = &words[1..];
        register = apply(graph, search, &operation, arguments, register, &mut source)
            .map_err(|error| format!("{audit}step {} `{step}`: {error}", number + 1))?;
        let _ = writeln!(audit, "{}. {step} → {}", number + 1, describe(&register));
    }
    let mut text = audit;
    let mut files = Vec::new();
    match &register {
        Register::Set(records) => {
            for record in records.iter().take(200) {
                let _ = writeln!(text, "{}", line(graph, *record));
                files.push(graph.records[*record].path.clone());
            }
            if records.len() > 200 {
                let _ = writeln!(
                    text,
                    "… {} more; add a FILTER or COUNT",
                    records.len() - 200
                );
            }
        }
        Register::Count(count) => {
            let _ = writeln!(text, "count: {count}");
        }
        Register::Empty => {}
    }
    files.extend(
        source
            .iter()
            .map(|record| graph.records[*record].path.clone()),
    );
    Ok(Outcome {
        text,
        files,
        source,
    })
}

fn apply(
    graph: &Graph,
    search: &Search,
    operation: &str,
    arguments: &[String],
    register: Register,
    source: &mut Vec<usize>,
) -> Result<Register, String> {
    let set = || match &register {
        Register::Set(records) => Ok(records.clone()),
        _ => Err("needs a set of records first (FIND or AT)".to_owned()),
    };
    Ok(match operation {
        "FIND" => Register::Set(find(graph, search, arguments)?),
        "AT" => {
            let target = arguments.join(" ");
            match lookup(graph, search, &target) {
                Lookup::Found(record) => Register::Set(vec![record]),
                other => return Err(failure(graph, &target, &other)),
            }
        }
        "UP" => Register::Set(dedup(
            set()?.iter().filter_map(|record| graph.records[*record].up),
        )),
        "DOWN" => Register::Set(dedup(
            set()?
                .iter()
                .flat_map(|record| graph.records[*record].down.iter().copied()),
        )),
        "FOLLOW" => Register::Set(follow(graph, &set()?, arguments)?),
        "FILTER" => Register::Set(filter(graph, set()?, arguments)?),
        "PATH" => Register::Set(path(graph, search, &set()?, &arguments.join(" "))?),
        "COUNT" => Register::Count(set()?.len()),
        "READ" | "SOURCE" => {
            let records = set()?;
            if operation == "SOURCE" {
                source.extend(records.iter().take(20));
            }
            Register::Set(records)
        }
        other => {
            return Err(format!(
                "unknown step `{other}`; use FIND, AT, UP, DOWN, FOLLOW, FILTER, PATH, COUNT, SOURCE"
            ));
        }
    })
}

/// `FIND "text" [limit]`: the best matches, as `graph_find` ranks them.
fn find(graph: &Graph, search: &Search, arguments: &[String]) -> Result<Vec<usize>, String> {
    let query = arguments.first().ok_or("FIND needs a query")?;
    let limit = arguments
        .get(1)
        .and_then(|limit| limit.parse().ok())
        .unwrap_or(10usize)
        .min(50);
    Ok(search
        .find(graph, query, limit, |_| true)
        .into_iter()
        .map(|hit| hit.record)
        .collect())
}

fn describe(register: &Register) -> String {
    match register {
        Register::Empty => "nothing".into(),
        Register::Set(records) => format!("{} records", records.len()),
        Register::Count(count) => format!("{count}"),
    }
}

/// `FOLLOW <relation|*> [in|out] [all]`: neighbours along a relation. Ambiguous links are
/// followed only with `all` (F19 accuracy rules).
fn follow(graph: &Graph, records: &[usize], arguments: &[String]) -> Result<Vec<usize>, String> {
    let relation = arguments.first().ok_or(
        "FOLLOW needs a relation: calls, implements, references, mentions, imports, depends or *",
    )?;
    let relation = match relation.as_str() {
        "*" => None,
        name => Some(Relation::parse(name).ok_or(format!("unknown relation `{name}`"))?),
    };
    let incoming = arguments.iter().any(|word| word.eq_ignore_ascii_case("in"));
    let all = arguments
        .iter()
        .any(|word| word.eq_ignore_ascii_case("all"));
    let mut out = Vec::new();
    for record in records {
        let edges = if incoming {
            &graph.incoming[*record]
        } else {
            &graph.outgoing[*record]
        };
        for edge in edges {
            let edge = &graph.edges[*edge];
            if relation.is_some_and(|wanted| wanted != edge.relation) {
                continue;
            }
            if !all && matches!(edge.provenance, Provenance::Ambiguous(_)) {
                continue;
            }
            out.push(if incoming { edge.from } else { edge.to });
        }
    }
    let out = dedup(out.into_iter());
    if out.len() > SET_LIMIT {
        return Err(format!(
            "{} records: too many to follow; FILTER first",
            out.len()
        ));
    }
    Ok(out)
}

/// `FILTER kind=<k> | plane=<n> | path=<glob> | name=<text>`; `!=` keeps the rest.
fn filter(graph: &Graph, records: Vec<usize>, arguments: &[String]) -> Result<Vec<usize>, String> {
    let condition = arguments.join(" ");
    let (key, value, negate) = if let Some((key, value)) = condition.split_once("!=") {
        (
            key.trim().to_ascii_lowercase(),
            value.trim().to_owned(),
            true,
        )
    } else if let Some((key, value)) = condition.split_once('=') {
        (
            key.trim().to_ascii_lowercase(),
            value.trim().to_owned(),
            false,
        )
    } else {
        // A bare glob is a path filter: `FILTER crates/velocity-git/**`.
        ("path".to_owned(), condition.trim().to_owned(), false)
    };
    let test = |record: usize| -> Result<bool, String> {
        let record = &graph.records[record];
        Ok(match key.as_str() {
            "kind" => {
                record.kind.label() == value
                    || format!("{:?}", record.kind)
                        .to_ascii_lowercase()
                        .contains(&value.to_ascii_lowercase())
            }
            "plane" => record.kind.plane().to_string() == value,
            "path" => glob(&value, &record.path),
            "name" => record
                .qualified
                .to_lowercase()
                .contains(&value.to_lowercase()),
            other => {
                return Err(format!(
                    "unknown filter `{other}`; use kind, plane, path or name"
                ));
            }
        })
    };
    let mut kept = Vec::new();
    for record in records {
        if test(record)? != negate {
            kept.push(record);
        }
    }
    Ok(kept)
}

/// `PATH <target>`: the shortest chain of links (either direction, ambiguous links left out)
/// from the register's first record to the target.
fn path(
    graph: &Graph,
    search: &Search,
    records: &[usize],
    target: &str,
) -> Result<Vec<usize>, String> {
    let start = *records.first().ok_or("PATH needs a starting record")?;
    let goal = match lookup(graph, search, target) {
        Lookup::Found(record) => record,
        other => return Err(failure(graph, target, &other)),
    };
    let mut previous = vec![usize::MAX; graph.records.len()];
    let mut queue = VecDeque::from([start]);
    previous[start] = start;
    while let Some(record) = queue.pop_front() {
        if record == goal {
            break;
        }
        let neighbours = graph.outgoing[record]
            .iter()
            .map(|edge| (&graph.edges[*edge], graph.edges[*edge].to))
            .chain(
                graph.incoming[record]
                    .iter()
                    .map(|edge| (&graph.edges[*edge], graph.edges[*edge].from)),
            );
        for (edge, next) in neighbours {
            if matches!(edge.provenance, Provenance::Ambiguous(_)) || previous[next] != usize::MAX {
                continue;
            }
            previous[next] = record;
            queue.push_back(next);
        }
    }
    if previous[goal] == usize::MAX {
        return Err(format!(
            "no path from {} to {} without ambiguous links",
            short(graph, start),
            short(graph, goal)
        ));
    }
    let mut chain = vec![goal];
    while let Some(last) = chain.last().copied()
        && last != start
    {
        chain.push(previous[last]);
    }
    chain.reverse();
    Ok(chain)
}

fn dedup(records: impl Iterator<Item = usize>) -> Vec<usize> {
    let mut seen = HashSet::new();
    records.filter(|record| seen.insert(*record)).collect()
}

fn split_steps(program: &str) -> Vec<String> {
    let mut steps = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in program.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                current.push(character);
            }
            ';' | '\n' if !quoted => steps.push(std::mem::take(&mut current)),
            _ => current.push(character),
        }
    }
    steps.push(current);
    steps
        .into_iter()
        .map(|step| step.trim().to_owned())
        .filter(|step| !step.is_empty())
        .collect()
}

/// Words of a step, with `"quoted text"` kept as one word.
fn tokens(step: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in step.chars() {
        match character {
            '"' => quoted = !quoted,
            ' ' | '\t' if !quoted => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(character),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// `*` matches within a folder, `**` across folders, `?` one character.
pub fn glob(pattern: &str, path: &str) -> bool {
    fn matches(pattern: &[u8], text: &[u8]) -> bool {
        match pattern.split_first() {
            None => text.is_empty(),
            Some((b'*', rest)) if rest.first() == Some(&b'*') => {
                let rest = rest[1..].strip_prefix(b"/").unwrap_or(&rest[1..]);
                (0..=text.len()).any(|skip| matches(rest, &text[skip..]))
            }
            Some((b'*', rest)) => (0..=text.len())
                .take_while(|skip| *skip == 0 || text[skip - 1] != b'/')
                .any(|skip| matches(rest, &text[skip..])),
            Some((b'?', rest)) => !text.is_empty() && text[0] != b'/' && matches(rest, &text[1..]),
            Some((character, rest)) => text.first() == Some(character) && matches(rest, &text[1..]),
        }
    }
    let pattern = if pattern.contains('/') || pattern.contains("**") {
        pattern.to_owned()
    } else {
        format!("**/{pattern}")
    };
    matches(pattern.as_bytes(), path.as_bytes())
}
