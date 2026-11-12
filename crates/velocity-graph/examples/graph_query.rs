//! Headless proof of the tools: `cargo run -p velocity-graph --example graph_query -- <worktree> <cache-folder> <tool> <json-arguments>`.
use std::path::PathBuf;

fn main() -> Result<(), velocity_graph::Error> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [root, cache, tool, json] = arguments.as_slice() else {
        return Err(velocity_graph::Error::Invalid(
            "usage: <worktree> <cache-folder> <tool> <json>".into(),
        ));
    };
    let index = velocity_graph::Index::open(&PathBuf::from(root), &PathBuf::from(cache))?;
    let mut tools = velocity_graph::Tools::new(index);
    let arguments: serde_json::Value = serde_json::from_str(json)?;
    let started = std::time::Instant::now();
    let answer = tools.call(tool, &arguments);
    let elapsed = started.elapsed();
    match answer {
        Ok(text) => println!("{text}"),
        Err(text) => println!("[failure]\n{text}"),
    }
    eprintln!("answered in {elapsed:?}");
    Ok(())
}
