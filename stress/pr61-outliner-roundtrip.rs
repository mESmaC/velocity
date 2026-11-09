// Risk: the F11 outliner must never corrupt AGENTS.md. Opening and saving unchanged must be
// byte-identical, and structural edits must never merge one bullet into another line.
// Run against the real velocity-core library: copy into crates/velocity-core/examples/ and
// `cargo run -p velocity-core --example pr61-outliner-roundtrip -- <markdown files...>`.
use velocity_core::agents_editor::{AgentsEditor, OutlineAction, OutlineSnapshot};

fn rules(snapshot: &OutlineSnapshot) -> Vec<(u32, String)> {
    snapshot
        .sections
        .iter()
        .flat_map(|section| section.lines.iter())
        .filter(|line| format!("{:?}", line.kind) == "Rule")
        .map(|line| (line.identifier, line.text.clone()))
        .collect()
}

fn reopened_rule_texts(markdown: &str) -> Vec<String> {
    let snapshot = AgentsEditor::new(markdown.to_owned()).snapshot(None);
    rules(&snapshot).into_iter().map(|(_, text)| text).collect()
}

fn report(name: &str, passed: bool, detail: &str) -> usize {
    println!("{} {name}{}", if passed { "PASS" } else { "FAIL" }, detail);
    usize::from(!passed)
}

fn round_trip(name: &str, source: &str) -> usize {
    let editor = AgentsEditor::new(source.to_owned());
    let markdown = editor.markdown();
    let dirty = editor.snapshot(None).dirty;
    report(
        &format!("round-trip {name}"),
        markdown == source && !dirty,
        &format!(" ({} bytes, dirty={dirty})", source.len()),
    )
}

// Type a new rule after the last rule, then reopen the saved Markdown: the new rule must
// survive as its own bullet.
fn new_rule_survives(name: &str, source: &str) -> usize {
    let mut editor = AgentsEditor::new(source.to_owned());
    let last = rules(&editor.snapshot(None)).last().map(|rule| rule.0);
    let Some(last) = last else {
        return report(&format!("enter-after-last {name}"), false, " (no rules)");
    };
    let snapshot = editor.apply(OutlineAction::Enter { identifier: last });
    let Some(focus) = snapshot.focus else {
        return report(&format!("enter-after-last {name}"), false, " (no focus)");
    };
    editor.apply(OutlineAction::Edit {
        identifier: focus.identifier,
        text: "Brand new rule".into(),
    });
    let markdown = editor.markdown();
    let survived = reopened_rule_texts(&markdown).contains(&"Brand new rule".to_owned());
    report(
        &format!("enter-after-last {name}"),
        survived,
        &format!(" saved={markdown:?}"),
    )
}

// Move the last rule up: every rule must still be its own bullet after reopening.
fn move_keeps_rules(name: &str, source: &str) -> usize {
    let mut editor = AgentsEditor::new(source.to_owned());
    let before = rules(&editor.snapshot(None));
    let Some(last) = before.last().map(|rule| rule.0) else {
        return report(&format!("move-last-up {name}"), false, " (no rules)");
    };
    editor.apply(OutlineAction::Move {
        identifier: last,
        down: false,
    });
    let markdown = editor.markdown();
    let mut expected: Vec<String> = before.into_iter().map(|rule| rule.1).collect();
    let mut actual = reopened_rule_texts(&markdown);
    expected.sort();
    actual.sort();
    report(
        &format!("move-last-up {name}"),
        expected == actual,
        &format!(" saved={markdown:?}"),
    )
}

fn main() {
    let fixtures: Vec<(&str, String)> = vec![
        ("lf", "# T\n\n## A\n- one\n- two\n".into()),
        ("crlf", "# T\r\n\r\n## A\r\n- one\r\n  - sub\r\n- two\r\n".into()),
        ("no-final-newline", "## A\n- one\n- two".into()),
        ("bom", "\u{feff}# T\n## A\n- one\n".into()),
        ("fence", "## A\n```\n- not a rule\n## not a section\n```\n- rule\n".into()),
        ("tabs-and-table", "## A\n\t- tab rule\n| a | b |\n|---|---|\n- r\n".into()),
        ("continuation", "## A\n- one\n- two\n    continued\n".into()),
        ("continuation-no-final-newline", "## A\n- one\n- two\n    continued".into()),
    ];
    let mut failures = 0;
    for (name, source) in &fixtures {
        failures += round_trip(name, source);
        failures += new_rule_survives(name, source);
        failures += move_keeps_rules(name, source);
    }
    for path in std::env::args().skip(1) {
        match std::fs::read_to_string(&path) {
            Ok(source) => failures += round_trip(&path, &source),
            Err(error) => failures += report(&path, false, &format!(" ({error})")),
        }
    }
    println!("{failures} failure(s)");
    std::process::exit(i32::from(failures > 0));
}
