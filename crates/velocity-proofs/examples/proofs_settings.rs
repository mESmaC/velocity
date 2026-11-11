//! Prints Settings → Proofs (SPEC.md F13) for a checkout without a window: the proof list from
//! SPEC.md, each design proof with its levels, and each language's translator.
//!
//! `cargo run -p velocity-proofs --example proofs_settings -- <repository> [stress inputs]`

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some(repository) = arguments.first() else {
        return Err("usage: proofs_settings <repository> [stress inputs]".into());
    };
    let inputs = match arguments.get(1) {
        Some(count) => count.parse()?,
        None => 10_000,
    };
    let view = velocity_proofs::proofs_settings(&PathBuf::from(repository), inputs)?;
    println!(
        "SPEC.md found: {} · proof list found: {}",
        view.spec_found, view.proof_list_found
    );
    for feature in &view.proof_list {
        println!("  list  {} — {}", feature.feature, feature.detail);
    }
    println!("Lake project: {:?}", view.lake_project);
    for design in &view.design_proofs {
        println!("  proof {}", design.folder);
        for level in &design.levels {
            let flag = if level.flagged { " (flagged)" } else { "" };
            println!(
                "        {} · {}{flag} · {}",
                level.language,
                level.level.label(),
                level.detail
            );
        }
    }
    for row in &view.translators {
        let translator = row.translator.as_deref().unwrap_or("none");
        println!(
            "  lang  {} · translator {translator} · strongest {}",
            row.language,
            row.strongest.label()
        );
    }
    println!("Stress-diff inputs: {}", view.stress_diff_input_count);
    Ok(())
}
