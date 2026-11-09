//! Exercise F11's production document path without launching the desktop app.
use velocity_core::agents_editor::OutlineAction;
use velocity_git::agents_file::AgentsFiles;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::path::PathBuf::from(std::env::args().nth(1).ok_or("Pass a checkout")?);
    let mut documents = AgentsFiles::default();
    let before = documents.read(&directory)?;
    let identifier = before
        .sections
        .iter()
        .flat_map(|section| &section.lines)
        .find(|line| line.text == "no unwrap")
        .ok_or("Add a 'no unwrap' bullet")?
        .identifier;
    let ticket = documents
        .begin_rewrite(&directory, identifier)?
        .ok_or("No rewrite")?;
    let answer = velocity_core::agents_rewrite::parse_rewrite(
        "- Never call `unwrap()`.\n- Return errors to the caller.",
    );
    let split = documents.finish_rewrite(&directory, &ticket, answer)?;
    println!("Split draft:\n{}", split.markdown);
    let restored = documents.edit(&directory, OutlineAction::Undo { identifier })?;
    if restored.markdown != before.markdown {
        return Err("Undo changed original bytes".into());
    }
    let stale = documents
        .begin_rewrite(&directory, identifier)?
        .ok_or("No rewrite")?;
    documents.edit(
        &directory,
        OutlineAction::Edit {
            identifier,
            text: "Keep my new text".into(),
        },
    )?;
    let draft = documents.finish_rewrite(&directory, &stale, Ok(vec!["Stale answer".into()]))?;
    if !draft.markdown.contains("Keep my new text") {
        return Err("Late answer lost edits".into());
    }
    documents.save(&directory)?;
    println!("Exact Undo, stale-answer rejection and save/commit succeeded");
    Ok(())
}
