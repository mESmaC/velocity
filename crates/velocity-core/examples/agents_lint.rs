//! Headless F11 lint and Covers driver: `cargo run -p velocity-core --example agents_lint -- <AGENTS.md>`.
//! Prints the Flagged list and Covers, types a hedged rule, then adds every missing topic.

use velocity_core::agents_editor::{AgentsEditor, OutlineAction, OutlineSnapshot};

fn report(label: &str, snapshot: &OutlineSnapshot) {
    println!(
        "== {label}: {} rules, {} flagged",
        snapshot.rule_count,
        snapshot.flagged.len()
    );
    for rule in &snapshot.flagged {
        println!(
            "  flagged {:>3} [{}] {:?} \u{201c}{}\u{201d}: {}",
            rule.identifier, rule.section, rule.kind, rule.phrase, rule.message
        );
    }
    for item in &snapshot.coverage {
        println!(
            "  covers {} {}",
            if item.covered { "ok  " } else { "MISS" },
            item.title
        );
    }
}

fn main() -> std::io::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "AGENTS.md".into());
    let source = match path.as_str() {
        "--empty" => String::new(),
        "--template" => velocity_core::agents_template::default_agents_markdown("Covers demo"),
        _ => std::fs::read_to_string(path)?,
    };
    let mut editor = AgentsEditor::new(source);
    let snapshot = editor.snapshot(None);
    report("as read", &snapshot);

    demonstrate_lint(&mut editor);
    let missing: Vec<u32> = editor
        .snapshot(None)
        .coverage
        .iter()
        .filter(|item| !item.covered)
        .map(|item| item.item)
        .collect();
    for item in missing {
        editor.apply(OutlineAction::AddCoverage { item });
    }
    let snapshot = editor.snapshot(None);
    report("after Add on every missing topic", &snapshot);
    for item in snapshot.coverage.iter().filter(|item| item.covered) {
        editor.apply(OutlineAction::AddCoverage { item: item.item });
    }
    if editor.markdown() != snapshot.markdown {
        return Err(std::io::Error::other("Repeated Add changed covered topics"));
    }
    // The whole file after Add; pipe it through `od -c` to check a CRLF file keeps CRLF.
    print!("{}", snapshot.markdown);
    Ok(())
}

fn demonstrate_lint(editor: &mut AgentsEditor) {
    let snapshot = editor.snapshot(None);
    let rule = snapshot
        .sections
        .iter()
        .flat_map(|section| &section.lines)
        .find(|line| line.kind == velocity_core::agents_editor::OutlineKind::Rule);
    if let Some(rule) = rule {
        let text = "You should probably keep the code clean, where possible (not `cargo clean`).";
        let snapshot = editor.apply(OutlineAction::Edit {
            identifier: rule.identifier,
            text: text.into(),
        });
        let line = snapshot
            .sections
            .iter()
            .flat_map(|section| &section.lines)
            .find(|line| line.identifier == rule.identifier);
        for part in line.map(|line| line.inline.as_slice()).unwrap_or_default() {
            println!(
                "  part code={} mark={:?} {:?}",
                part.code, part.mark, part.text
            );
        }
    }
}
