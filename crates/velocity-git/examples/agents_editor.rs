//! Headless F11 entry point: `cargo run -p velocity-git --example agents_editor -- <checkout>`.
//! Commands: list, markdown, edit <id> <text>, enter/indent/outdent/up/down/delete <id>,
//! rule [section-id], section, reload, save, quit. Edits stay in memory until Save.

use std::error::Error;
use std::io::{self, BufRead};
use std::path::PathBuf;
use velocity_core::agents_editor::{OutlineAction, OutlineSnapshot};
use velocity_git::agents_file::AgentsFiles;

fn main() -> Result<(), Box<dyn Error>> {
    let directory = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("Pass a checkout directory")?,
    );
    let mut documents = AgentsFiles::default();
    let mut snapshot = documents.read(&directory)?;
    show(&snapshot);
    for input in io::stdin().lock().lines() {
        let input = input?;
        let (command, argument) = input.split_once(' ').unwrap_or((&input, ""));
        match command {
            "quit" => break,
            "list" => show(&snapshot),
            "markdown" => print!("{}", snapshot.markdown),
            "reload" => {
                snapshot = documents.reload(&directory)?;
                show(&snapshot);
            }
            "save" => match documents.save(&directory) {
                Ok(saved) => {
                    snapshot = saved;
                    println!("Saved and committed AGENTS.md");
                }
                Err(error) => eprintln!("{error}"),
            },
            _ => {
                let action = action(command, argument)?;
                snapshot = documents.edit(&directory, action)?;
                show(&snapshot);
            }
        }
    }
    Ok(())
}

fn action(command: &str, argument: &str) -> Result<OutlineAction, Box<dyn Error>> {
    use OutlineAction::*;
    if command == "section" {
        return Ok(AddSection);
    }
    if command == "rule" {
        return Ok(AddRule {
            section: argument.parse().ok(),
        });
    }
    let (identifier, text) = argument.split_once(' ').unwrap_or((argument, ""));
    let identifier = identifier.parse()?;
    Ok(match command {
        "edit" => Edit {
            identifier,
            text: text.into(),
        },
        "enter" => Enter { identifier },
        "indent" | "outdent" => Indent {
            identifier,
            nested: command == "indent",
        },
        "up" | "down" => Move {
            identifier,
            down: command == "down",
        },
        "delete" => DeleteEmpty { identifier },
        _ => return Err("Unknown editor command".into()),
    })
}

fn show(snapshot: &OutlineSnapshot) {
    println!("{} rules · dirty={}", snapshot.rule_count, snapshot.dirty);
    for line in snapshot.sections.iter().flat_map(|section| &section.lines) {
        println!(
            "{} {:?} {}{}",
            line.identifier,
            line.kind,
            if line.nested { "  " } else { "" },
            line.text
        );
    }
    if let Some(focus) = &snapshot.focus {
        println!("Focus {}", focus.identifier);
    }
}
