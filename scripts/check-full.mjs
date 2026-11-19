// The integration suite runs on a staged Rosen base, in one verify turn.
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { physicalRoot } from "./check-inputs.mjs";

const root = physicalRoot(fileURLToPath(import.meta.url));
const environment = { ...process.env };
if (process.platform === "win32") {
  environment.Path = `${process.env.SystemRoot}\\System32;${process.env.Path}`;
}
if (
  !environment.RUSTC_WRAPPER &&
  spawnSync("sccache", ["--version"], { windowsHide: true }).status === 0
) {
  environment.RUSTC_WRAPPER = "sccache";
}

function run(program, arguments_) {
  console.log(`> ${program} ${arguments_.join(" ")}`);
  const result = spawnSync(program, arguments_, {
    cwd: root,
    env: environment,
    windowsHide: true,
    stdio: "inherit",
    encoding: "utf8",
  });
  if (result.status !== 0) {
    console.error(`Failed: ${program} ${arguments_.join(" ")}`);
    process.exitCode = result.status ?? 1;
    throw new Error(result.error?.message ?? "Integration command failed");
  }
}

function checkBindings() {
  const path = join(root, "ui", "src", "lib", "bindings.ts");
  const saved = existsSync(path) ? readFileSync(path) : null;
  try {
    run("cargo", [
      "run",
      "--locked",
      "-p",
      "velocity-app",
      "--bin",
      "export-bindings",
      "--",
      "--check",
    ]);
  } finally {
    // A failed bindings check must leave the landed input intact for the retry.
    if (saved) writeFileSync(path, saved);
    else rmSync(path, { force: true });
  }
}

try {
  run("cargo", ["fmt", "--all", "--", "--check"]);
  run("node", ["scripts/check-ui.mjs"]);
  // No separate `cargo build --workspace`: clippy with --all-targets compiles every
  // target, and the tests and bindings check build the libraries, so a build step only
  // added a third full compile (#498).
  run("cargo", [
    "clippy",
    "--workspace",
    "--all-targets",
    "--locked",
    "--",
    "-D",
    "warnings",
  ]);
  run("cargo", ["test", "--workspace", "--locked"]);
  checkBindings();
} catch (error) {
  console.error(error);
  process.exitCode ||= 1;
}
