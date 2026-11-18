const DISCOVER: &str = "Take the applicable spec section IDs or headings from the work item, its issue or its §7 entry. If none are given, use `graph_find` (kind: section) with the task's terms to find headings in the spec index.";
const READ: &str = "Read only the relevant headings with `graph_read` (source: true). If the graph is unavailable or clips a section, read just that section's lines. Never load the entire spec for a task.";

/// Explicit references travel with the work. The agent resolves headings in its own
/// workspace, where the graph can check that the spans still match the source.
pub(crate) fn instructions(evidence: &[&str]) -> String {
    let mut sections = Vec::new();
    for word in evidence.iter().flat_map(|text| {
        text.split(|character: char| {
            !character.is_alphanumeric() && character != '.' && character != '§'
        })
    }) {
        if word.starts_with(['F', '§'])
            && let Some(section) = velocity_spec::section_id(word)
            && !sections.contains(&section)
        {
            sections.push(section);
        }
    }
    if sections.is_empty() {
        return format!("{DISCOVER} {READ}");
    }
    format!(
        "Spec references in this work item: {}. Use any additional references in its evidence. Locate the applicable headings with `graph_find` (kind: section). {READ}",
        sections.join(", ")
    )
}

pub(crate) fn named_section(section: &str) -> String {
    format!(
        "The work item names spec section `{section}`. Find its heading with `graph_find` (kind: section). {READ}"
    )
}
