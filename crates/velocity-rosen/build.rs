use std::env;
use std::io;
use std::path::PathBuf;
use std::process::Command;

#[allow(clippy::disallowed_methods)]
fn main() -> io::Result<()> {
    const SOURCE: &str = "src/agent_launcher.rs";
    println!("cargo:rerun-if-changed={SOURCE}");
    let variable = |name| env::var_os(name).ok_or_else(|| io::Error::other(name));
    let output = PathBuf::from(variable("OUT_DIR")?).join("agent-launcher.exe");
    let mut compiler = Command::new(variable("RUSTC")?);
    compiler
        .args([SOURCE, "--edition=2024", "--target"])
        .arg(variable("TARGET")?)
        .args(["-C", "opt-level=s", "-C", "strip=symbols", "-o"])
        .arg(output);
    if let Some(linker) = env::var_os("RUSTC_LINKER") {
        let mut flag = std::ffi::OsString::from("linker=");
        flag.push(linker);
        compiler.arg("-C").arg(flag);
    }
    if compiler.status()?.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            "could not build the agent console launcher",
        ))
    }
}
