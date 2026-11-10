use velocity_airc::{Error, SERVER_SWITCH, serve_stdio};

fn main() -> Result<(), Error> {
    let mut arguments: Vec<_> = std::env::args_os().skip(1).collect();
    // Accept Velocity's own switch too, so the standalone server can stand in for the app's
    // executable in an `AircLaunch` (headless examples).
    if arguments.first().and_then(|first| first.to_str()) == Some(SERVER_SWITCH) {
        arguments.remove(0);
    }
    serve_stdio(&arguments)
}
