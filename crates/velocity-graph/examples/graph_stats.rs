//! Headless proof of the index: `cargo run -p velocity-graph --example graph_stats -- <worktree> <cache-folder>`.
use std::path::PathBuf;

fn main() -> Result<(), velocity_graph::Error> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [root, cache] = arguments.as_slice() else {
        return Err(velocity_graph::Error::Invalid(
            "usage: <worktree> <cache-folder>".into(),
        ));
    };
    let index = velocity_graph::Index::open(&PathBuf::from(root), &PathBuf::from(cache))?;
    let graph = &index.graph;
    println!("{:?}", index.report);
    let mut planes = [0usize; 4];
    for record in &graph.records {
        planes[record.kind.plane() as usize] += 1;
    }
    println!(
        "symbols {} files {} subsystems {}",
        planes[1], planes[2], planes[3]
    );
    let mut kinds = std::collections::BTreeMap::new();
    for edge in &graph.edges {
        *kinds
            .entry((
                edge.relation.label(),
                edge.provenance
                    .label()
                    .split(' ')
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            ))
            .or_insert(0) += 1;
    }
    for ((relation, provenance), count) in kinds {
        println!("{relation:>10} {provenance:>10} {count}");
    }
    Ok(())
}
