# Risk (#674): Rosen parking must derive F16 stack parents from live Claim blockers of the
# active wait rather than general `overlaps` results (which include shares and snapshot edits).
# Adapted from Scout #674 reproduction: Cargo.toml moved the pinned Rosen commit past
# 127d6504941f90040a1854a9441b20af3a7a39ae and the release gate provides the pinned Rosen
# binaries via VELOCITY_ROSEN_BIN / PATH rather than %LOCALAPPDATA%\velocity-rosen. Binary
# discovery checks VELOCITY_ROSEN_BIN, PATH, and %LOCALAPPDATA%\velocity-rosen (preferring
# Cargo.toml's pinned rev); all original Scout assertions are preserved unchanged.
from pathlib import Path
import json
import os
import re
import shutil
import subprocess
import tempfile
import time
import uuid

workspace = Path.cwd()


def find_rosen_bin():
    env_bin = os.environ.get("VELOCITY_ROSEN_BIN")
    if env_bin:
        folder = Path(env_bin)
        if (folder / "rosen-server.exe").is_file() and (folder / "rosen.exe").is_file():
            return folder
    server_on_path = shutil.which("rosen-server")
    cli_on_path = shutil.which("rosen")
    if server_on_path and cli_on_path:
        folder = Path(server_on_path).parent
        if (folder / "rosen.exe").is_file():
            return folder
    cargo_toml = (workspace / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'rosen-client\s*=\s*\{[^}]*rev\s*=\s*"([0-9a-f]+)"', cargo_toml)
    preferred_pins = []
    if match:
        preferred_pins.append(match.group(1))
    preferred_pins.append("127d6504941f90040a1854a9441b20af3a7a39ae")
    cache = Path(os.environ["LOCALAPPDATA"]) / "velocity-rosen"
    markers = list(cache.glob("*/rosen-commit"))
    for expected in preferred_pins:
        for marker in markers:
            folder = marker.parent
            if (
                marker.read_text().strip() == expected
                and (folder / "rosen-server.exe").is_file()
                and (folder / "rosen.exe").is_file()
            ):
                return folder
    for marker in markers:
        folder = marker.parent
        if (folder / "rosen-server.exe").is_file() and (folder / "rosen.exe").is_file():
            return folder
    raise RuntimeError("Could not locate rosen-server.exe and rosen.exe")


with tempfile.TemporaryDirectory(prefix="velocity-scout-531-") as temp:
    temp = Path(temp)
    seed = temp / "seed"
    seed.mkdir()
    git = Path(os.environ["ProgramFiles"]) / "Git" / "cmd" / "git.exe"
    child_env = {key: value for key, value in os.environ.items()
                 if not key.startswith("ROSEN_")}
    child_env["PATH"] = str(git.parent) + os.pathsep + child_env["PATH"]
    def run(program, *args, cwd=seed, check=True):
        result = subprocess.run([str(program), *map(str, args)], cwd=cwd,
                                env=child_env, capture_output=True, text=True, timeout=10)
        if check and result.returncode:
            raise RuntimeError(f"{args}: {result.stdout} {result.stderr}")
        return result
    run(git, "init", "-q")
    (seed / "code.rs").write_text("fn main() {}\n")
    run(git, "add", "code.rs")
    run(git, "-c", "user.name=Scout", "-c", "user.email=scout@example.invalid", "commit", "-qm", "Fixture")
    pin = find_rosen_bin()
    endpoint = r"\\.\pipe\velocity-scout-" + uuid.uuid4().hex
    daemon = subprocess.Popen([str(pin / "rosen-server.exe"), "--data-dir", str(temp / "data"), "--endpoint", endpoint],
                              cwd=temp, env=child_env, creationflags=subprocess.CREATE_NO_WINDOW,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    def rosen(*args, check=True):
        return run(pin / "rosen.exe", "--endpoint", endpoint, "--json", *args, cwd=temp, check=check)
    try:
        for _ in range(60):
            status = rosen("server", "status", check=False)
            if status.returncode == 0:
                break
            time.sleep(0.05)
        status.check_returncode()
        commit_marker = pin / "rosen-commit"
        if commit_marker.is_file():
            assert json.loads(status.stdout)["commit"] == commit_marker.read_text().strip()
        rosen("import", seed)
        def change(owner):
            return json.loads(rosen("change", "new", "--owner", owner).stdout)["change"]["id"]
        incidental, blocker = sorted([change("candidate-a"), change("candidate-b")])
        waiter = change("waiter")
        rosen("claim", "--change", blocker, "--exclusive", "code.rs")
        rosen("share", "--change", incidental, "code.rs")
        def query_overlaps():
            result = json.loads(rosen("overlaps", "code.rs").stdout)
            return result["overlaps"]["overlaps"]
        if query_overlaps()[0]["change"] == blocker:
            rosen("release", "--change", blocker, "code.rs")
            rosen("release", "--change", incidental, "code.rs")
            incidental, blocker = blocker, incidental
            rosen("claim", "--change", blocker, "--exclusive", "code.rs")
            rosen("share", "--change", incidental, "code.rs")
        waits = subprocess.Popen(
            [str(pin / "rosen.exe"), "--endpoint", endpoint, "--json",
             "wait", "--change", waiter, "code.rs"],
            cwd=temp, env=child_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            for _ in range(60):
                claims = json.loads(rosen("status", "--change", waiter).stdout)["claims"]
                if any(claim["kind"] == "wait" for claim in claims):
                    break
                time.sleep(0.05)
            assert any(claim["kind"] == "wait" for claim in claims), claims
            overlaps = query_overlaps()
            print("Actual blocker:", blocker)
            print("Incidental share:", incidental)
            print("Rosen overlaps:", overlaps)
            rosen("release", "--change", blocker, "code.rs")
            stdout, stderr = waits.communicate(timeout=3)
            assert waits.returncode == 0, (stdout, stderr)
            print("The wait completed with the incidental share still present.")
            proxy = (workspace / "crates/velocity-rosen/src/mcp_proxy.rs").read_text(encoding="utf-8")
            stack = (workspace / "src-tauri/src/goal_stack.rs").read_text(encoding="utf-8")
            holder_query = proxy.split("pub async fn wait_holders(", 1)[-1].split("\n}\n", 1)[0]
            parent_choice = stack.split("fn rosen_wait_parent(", 1)[-1].split("\n}\n", 1)[0]
            if "client.overlaps(query)" in holder_query and "return Ok(Some(holder_change))" in parent_choice:
                # Both changes are represented by working Velocity threads in this case.
                selected = next(row["change"] for row in overlaps if row["change"] != waiter)
                print("Velocity's first matching working parent:", selected)
                assert selected == blocker, "A share was selected as parent even though it could not block the wait"
        finally:
            if waits.poll() is None:
                waits.terminate()
                waits.wait(timeout=3)
    finally:
        daemon.terminate()
        daemon.wait(timeout=10)
