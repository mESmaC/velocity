// Risk: F11 Undo must restore a rewritten bullet's wording without merging it into the bullet
// after it. A file's last line may have no newline; the restored copy must not bring that back.
// Run against the real velocity-core library: copy into crates/velocity-core/examples/ and
// `cargo run -p velocity-core --example pr77-undo-line-ending`.
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

fn reopened(markdown: &str) -> Vec<String> {
    rules(&AgentsEditor::new(markdown.to_owned()).snapshot(None))
        .into_iter()
        .map(|rule| rule.1)
        .collect()
}

fn report(name: &str, passed: bool, detail: &str) -> usize {
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

// Rewrite the last rule (no trailing newline), add a rule after it, then Undo the rewrite.
fn undo_after_adding(name: &str, source: &str, split: bool) -> usize {
    let mut editor = AgentsEditor::new(source.to_owned());
    let last = rules(&editor.snapshot(None)).last().map(|rule| rule.0);
    let Some(last) = last else {
        return report(name, false, "no rules");
    };
    let Some(ticket) = editor.begin_rewrite(last) else {
        return report(name, false, "rewrite did not start");
    };
    let answer = if split {
        vec!["First tightened rule".into(), "Second split rule".into()]
    } else {
        vec!["Tightened rule".into()]
    };
    editor.finish_rewrite(&ticket, Ok(answer));
    let added = editor
        .apply(OutlineAction::Enter { identifier: last })
        .focus
        .map(|focus| focus.identifier);
    if let Some(added) = added {
        editor.apply(OutlineAction::Edit {
            identifier: added,
            text: "Rule typed after the rewrite".into(),
        });
    }
    editor.apply(OutlineAction::Undo { identifier: last });
    let markdown = editor.markdown();
    let survived = reopened(&markdown).contains(&"Rule typed after the rewrite".to_owned());
    report(name, survived, &format!("saved={markdown:?}"))
}

fn main() {
    let mut failures = 0;
    for (name, source) in [
        ("lf-final-newline", "## A\n- one\n- two\n"),
        ("no-final-newline", "## A\n- one\n- two"),
        ("crlf-no-final-newline", "## A\r\n- one\r\n- two"),
    ] {
        failures += undo_after_adding(&format!("{name}/tighten"), source, false);
        failures += undo_after_adding(&format!("{name}/split"), source, true);
    }
    println!("failures: {failures}");
    std::process::exit(i32::from(failures > 0));
}
