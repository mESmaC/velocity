use velocity_airc::{Error, SERVER_SWITCH, serve_stdio};

fn main() -> Result<(), Error> {
    let mut arguments: Vec<_> = std::env::args_os().skip(1).collect();
    // Sessions start the code graph from the same executable as AIRC (F19), so the standalone
    // server serves it too when it stands in for the app's executable.
    if arguments.first().and_then(|first| first.to_str()) == Some(velocity_graph::SERVER_SWITCH) {
        return velocity_graph::serve_stdio(&arguments[1..])
            .map_err(|error| Error::Invalid(error.to_string()));
    }
    // Accept Velocity's own switch too, so the standalone server can stand in for the app's
    // executable in an `AircLaunch` (headless examples).
    if arguments.first().and_then(|first| first.to_str()) == Some(SERVER_SWITCH) {
        arguments.remove(0);
    }
    serve_stdio(&arguments)
}
