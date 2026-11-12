//! `graph_run`: pointer programs over one register (F19, after A.TXY's N6 programs). Several
//! hops run in one tool call, with a one-line audit per step.

use std::collections::{HashSet, VecDeque};
use std::fmt::Write;

use crate::graph::{Graph, Provenance, Relation};
use crate::query::{Lookup, failure, hub_threshold, line, lookup, short};
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
    let mut extra = Extra::default();
    let mut last = String::new();
    for (number, step) in steps.iter().enumerate() {
        let words = tokens(step);
        let Some(operation) = words.first().map(|word| word.to_ascii_uppercase()) else {
            continue;
        };
        let arguments = &words[1..];
        last.clone_from(&operation);
        register = apply(graph, search, &operation, arguments, register, &mut extra)
            .map_err(|error| format!("{audit}step {} `{step}`: {error}", number + 1))?;
        let _ = writeln!(audit, "{}. {step} → {}", number + 1, describe(&register));
    }
    let mut text = audit;
    for note in &extra.notes {
        text.push_str(note);
    }
    let mut files = Vec::new();
    match &register {
        // A path's note already lists its records.
        Register::Set(_) if last == "PATH" => {}
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
        extra
            .source
            .iter()
            .map(|record| graph.records[*record].path.clone()),
    );
    Ok(Outcome {
        text,
        files,
        source: extra.source,
    })
}

/// What steps add besides the register: records whose source to show, and notes such as a
/// path's hops.
#[derive(Default)]
struct Extra {
    source: Vec<usize>,
    notes: Vec<String>,
}

fn apply(
    graph: &Graph,
    search: &Search,
    operation: &str,
    arguments: &[String],
    register: Register,
    extra: &mut Extra,
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
        "PATH" => {
            let (chain, note) = path(graph, search, &set()?, &arguments.join(" "))?;
            extra.notes.push(note);
            Register::Set(chain)
        }
        "COUNT" => Register::Count(set()?.len()),
        "READ" | "SOURCE" => {
            let records = set()?;
            if operation == "SOURCE" {
                extra.source.extend(records.iter().take(20));
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

/// One step of a path: the record reached and how (`calls`, `calls (in)`, `in`, `contains`),
/// with the link's provenance.
type Hop = (usize, String, Provenance);

/// `PATH <target>`: the shortest chain of links from the register's first record to the
/// target, in either direction and through containment, leaving ambiguous links out. The note
/// lists each hop and the path's confidence: that of its weakest hop (after Graphify).
fn path(
    graph: &Graph,
    search: &Search,
    records: &[usize],
    target: &str,
) -> Result<(Vec<usize>, String), String> {
    let start = *records.first().ok_or("PATH needs a starting record")?;
    let goal = match lookup(graph, search, target) {
        Lookup::Found(record) => record,
        other => return Err(failure(graph, target, &other)),
    };
    let mut previous: Vec<Option<(usize, String, Provenance)>> = vec![None; graph.records.len()];
    let mut queue = VecDeque::from([start]);
    let mut seen = HashSet::from([start]);
    let hub = hub_threshold(graph);
    while let Some(record) = queue.pop_front() {
        if record == goal {
            break;
        }
        // A path through a hub (a shared `Error` type, a logging helper) says nothing.
        if record != start && graph.incoming[record].len() >= hub {
            continue;
        }
        for (next, how, provenance) in neighbours(graph, record) {
            if seen.insert(next) {
                previous[next] = Some((record, how, provenance));
                queue.push_back(next);
            }
        }
    }
    if !seen.contains(&goal) {
        return Err(format!(
            "no path from {} to {} without ambiguous links",
            short(graph, start),
            short(graph, goal)
        ));
    }
    let mut hops: Vec<Hop> = Vec::new();
    let mut at = goal;
    while let Some((from, how, provenance)) = previous[at].clone() {
        hops.push((at, how, provenance));
        at = from;
    }
    hops.reverse();
    Ok(path_note(graph, start, hops))
}

fn neighbours(graph: &Graph, record: usize) -> Vec<Hop> {
    let mut out: Vec<Hop> = Vec::new();
    for edge in &graph.outgoing[record] {
        let edge = &graph.edges[*edge];
        out.push((edge.to, edge.relation.label().to_owned(), edge.provenance));
    }
    for edge in &graph.incoming[record] {
        let edge = &graph.edges[*edge];
        out.push((
            edge.from,
            format!("{} (in)", edge.relation.label()),
            edge.provenance,
        ));
    }
    let node = &graph.records[record];
    out.extend(
        node.up
            .map(|up| (up, "in".to_owned(), Provenance::Extracted)),
    );
    out.extend(
        node.down
            .iter()
            .map(|down| (*down, "contains".to_owned(), Provenance::Extracted)),
    );
    out.retain(|(_, _, provenance)| !matches!(provenance, Provenance::Ambiguous(_)));
    out
}

fn path_note(graph: &Graph, start: usize, hops: Vec<Hop>) -> (Vec<usize>, String) {
    let weakest = hops
        .iter()
        .map(|(_, _, provenance)| *provenance)
        .max_by_key(|provenance| match provenance {
            Provenance::Extracted => 0,
            Provenance::Inferred => 1,
            Provenance::Ambiguous(_) => 2,
        })
        .unwrap_or(Provenance::Extracted);
    let mut note = format!("path, {} hops:\n  {}\n", hops.len(), short(graph, start));
    let mut chain = vec![start];
    for (record, how, provenance) in hops {
        note.push_str(&format!(
            "  --{how} [{}]--> {}\n",
            provenance.label(),
            short(graph, record)
        ));
        chain.push(record);
    }
    note.push_str(&format!(
        "path confidence: {} (its weakest hop)\n",
        weakest.label()
    ));
    (chain, note)
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
