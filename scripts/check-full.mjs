// Rosen checks.post_land: linking, formatting, isolated builds and the full tests.
// Fast checks don't link the desktop app or run tests; this set still does both.
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { physicalRoot } from "./check-inputs.mjs";

const root = physicalRoot(fileURLToPath(import.meta.url));
const environment = { ...process.env };
if (
  !environment.RUSTC_WRAPPER &&
  spawnSync("sccache", ["--version"], { windowsHide: true }).status === 0
) {
  environment.RUSTC_WRAPPER = "sccache";
}

function run(program, arguments_, capture = false) {
  console.log(`> ${program} ${arguments_.join(" ")}`);
  const result = spawnSync(program, arguments_, {
    cwd: root,
    env: environment,
    windowsHide: true,
    stdio: capture ? ["ignore", "pipe", "inherit"] : "inherit",
    encoding: "utf8",
  });
  if (result.status !== 0) process.exit(result.status ?? 1);
  return result.stdout;
}

run("cargo", ["fmt", "--all", "--", "--check"]);
run("node", ["scripts/check-fast.mjs", "--full"]);
run("cargo", ["build", "--workspace", "--locked"]);
const metadata = JSON.parse(
  run(
    "cargo",
    ["metadata", "--no-deps", "--format-version", "1", "--locked"],
    true,
  ),
);
for (const item of metadata.packages.filter((item) =>
  metadata.workspace_members.includes(item.id),
)) {
  run("cargo", ["check", "--locked", "-p", item.name]);
}
run("cargo", ["test", "--workspace", "--locked"]);
