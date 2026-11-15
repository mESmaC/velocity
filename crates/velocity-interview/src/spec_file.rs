//! Writing the locked proof list into SPEC.md (F3 Ready, F17: "SPEC.md records it").

const HEADING: &str = "## Proof list";

/// `spec` with its proof list section replaced by `features`, or added at the end. The
/// agent may already have written one; the list you edited on the Ready card wins.
pub fn with_proof_list(spec: &str, features: &[String]) -> String {
    let without = without_section(spec);
    let mut text = without.trim_end().to_owned();
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str(HEADING);
    text.push_str(
        "\n\nThese features are proven in Lean after their code (F17). Every other feature \
ships with no Lean.\n\n",
    );
    if features.is_empty() {
        text.push_str("- None.\n");
    }
    for feature in features {
        text.push_str(&format!("- {}\n", feature.trim()));
    }
    text
}

/// `spec` without the `## Proof list` section: from its heading to the next heading of the
/// same or a higher level.
fn without_section(spec: &str) -> String {
    let mut kept = String::new();
    let mut skipping: Option<Option<usize>> = None;
    let mut saw_entry = false;
    for line in spec.split_inclusive('\n') {
        let trimmed = line.trim_end();
        let heading = velocity_spec::headings(trimmed).next();
        let plain = trimmed.replace(['*', '`'], "").trim().to_lowercase();
        let list_heading = heading.is_some_and(|(_, title)| {
            title
                .replace(['*', '`'], "")
                .trim()
                .to_lowercase()
                .ends_with("proof list")
        });
        let list_paragraph = heading.is_none() && plain.ends_with("proof list:");
        if list_heading || list_paragraph {
            skipping = Some(heading.map(|(level, _)| level));
            saw_entry = false;
            continue;
        }
        if let Some(level) = skipping {
            if heading.is_some_and(|(depth, _)| level.is_none_or(|level| depth <= level))
                || (level.is_none() && saw_entry && trimmed.is_empty())
            {
                skipping = None;
            } else if trimmed.trim_start().starts_with(['-', '*', '|']) {
                saw_entry = true;
            }
        }
        if skipping.is_none() {
            kept.push_str(line);
        }
    }
    kept
}
