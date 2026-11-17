//! Headless proof of the index: `cargo run -p velocity-graph --example graph_stats -- <worktree> <cache-folder>`.
use std::path::PathBuf;

fn main() -> Result<(), velocity_graph::Error> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [root, cache] = arguments.as_slice() else {
        return Err(velocity_graph::Error::Invalid(
            "usage: <worktree> <cache-folder>".into(),
        ));
    };
    let clock = std::time::Instant::now();
    let index = velocity_graph::Index::open(&PathBuf::from(root), &PathBuf::from(cache))?;
    let cold_or_warm = clock.elapsed().as_millis();
    let graph = &index.graph;
    println!("{:?}", index.report);
    println!("cache bytes {}", index.cache_bytes());
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
    let clock = std::time::Instant::now();
    let mut tools = velocity_graph::Tools::new(index);
    let search_milliseconds = clock.elapsed().as_millis();
    let clock = std::time::Instant::now();
    let result = tools.call(
        "graph_find",
        &serde_json::json!({"query": "workspace snapshot"}),
    );
    println!(
        "open ms {cold_or_warm}, search ms {search_milliseconds}, find ms {}",
        clock.elapsed().as_millis()
    );
    if let Err(error) = result {
        return Err(velocity_graph::Error::Invalid(error));
    }
    Ok(())
}
