//! `velocity-graph`: the standalone code graph indexer Collapse pins (C11). Its commands and
//! output schema are in `velocity_graph::command_line`.

fn main() -> std::process::ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    velocity_graph::command_line::run(&arguments)
}
