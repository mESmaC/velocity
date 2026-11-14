fn main() -> anyhow::Result<()> {
    // `--check` fails when the committed bindings were stale (rosen.toml's pre-land checks).
    if std::env::args().any(|argument| argument == "--check") {
        return velocity_app_lib::check_bindings();
    }
    velocity_app_lib::export_bindings()
}
