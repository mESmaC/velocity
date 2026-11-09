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
    let mut skipping = false;
    for line in spec.split_inclusive('\n') {
        let trimmed = line.trim_end();
        if trimmed.eq_ignore_ascii_case(HEADING) {
            skipping = true;
            continue;
        }
        if skipping && (trimmed.starts_with("## ") || trimmed.starts_with("# ")) {
            skipping = false;
        }
        if !skipping {
            kept.push_str(line);
        }
    }
    kept
}
