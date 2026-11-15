// Rosen checks.pre_land: fast build checks (after each land in after-land mode).
// Usage: node scripts/check-fast.mjs [--plan] [--full]
import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  affected,
  bindingsHash,
  digest,
  inputs,
  physicalRoot,
} from "./check-inputs.mjs";

const root = physicalRoot(fileURLToPath(import.meta.url));
const state = join(root, "target", ".velocity-checks");
const stamp = join(state, "fast.json");
const feed = join(state, "feed.txt");
const force = process.argv.includes("--full");
const planOnly = process.argv.includes("--plan");
// A separate target per folder avoids Cargo locks between agents; only sccache
// is shared. Do not replace an explicitly configured compiler wrapper.
const environment = { ...process.env };
if (process.platform === "win32")
  environment.Path = `${process.env.SystemRoot}\\System32;${process.env.Path}`;
if (!environment.RUSTC_WRAPPER) {
  const probe = spawnSync("sccache", ["--version"], {
    env: environment,
    windowsHide: true,
  });
  if (probe.status === 0) environment.RUSTC_WRAPPER = "sccache";
}

function command(program, arguments_, capture = false) {
  console.log(`> ${program} ${arguments_.join(" ")}`);
  const result = spawnSync(program, arguments_, {
    cwd: root,
    env: environment,
    windowsHide: true,
    stdio: capture ? ["ignore", "pipe", "inherit"] : "inherit",
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.status !== 0)
    throw new Error(
      `${program} failed (${result.status ?? result.error?.message ?? "signal"})`,
    );
  return result.stdout;
}

function readState() {
  try {
    const value = JSON.parse(readFileSync(stamp, "utf8"));
    return value.version === 1 ? value : {};
  } catch {
    return {};
  }
}

function cacheStats() {
  const result = spawnSync(
    "sccache",
    ["--show-stats", "--stats-format", "json"],
    {
      cwd: root,
      env: environment,
      windowsHide: true,
      encoding: "utf8",
    },
  );
  try {
    const stats = JSON.parse(result.stdout).stats;
    const total = (value) =>
      Object.values(value.counts).reduce((sum, count) => sum + count, 0);
    return { hits: total(stats.cache_hits), misses: total(stats.cache_misses) };
  } catch {
    return null;
  }
}

function cacheLine(before, after) {
  if (
    !before ||
    !after ||
    !/(^|[\\/])sccache(?:\.exe)?$/.test(environment.RUSTC_WRAPPER ?? "")
  ) {
    return "sccache unavailable";
  }
  const hits = after.hits - before.hits;
  const misses = after.misses - before.misses;
  if (hits < 0 || misses < 0) return "sccache counters reset";
  const total = hits + misses;
  return total
    ? `sccache ${((hits / total) * 100).toFixed(1)}% hits (${hits} hits, ${misses} misses; machine delta)`
    : "sccache no cacheable requests";
}

function checkBindings() {
  const path = join(root, "ui", "src", "lib", "bindings.ts");
  const saved = existsSync(path) ? readFileSync(path) : null;
  try {
    command("cargo", [
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
    // --check currently regenerates even on failure. A failed run must not fix
    // its own input and let the next attempt certify stale committed bindings.
    if (saved) writeFileSync(path, saved);
    else rmSync(path, { force: true });
  }
}

function plan() {
  const metadata = JSON.parse(
    command(
      "cargo",
      ["metadata", "--no-deps", "--format-version", "1", "--locked"],
      true,
    ),
  );
  const current = inputs(root, metadata);
  const previous = readState();
  const tools = command("rustc", ["--version"], true).trim();
  const configuration = digest(
    JSON.stringify([
      tools,
      process.version,
      Object.entries(environment)
        .filter(([name]) =>
          /^(RUST|CARGO_|VELOCITY_ROSEN_(CLI|SERVER)_EXE)/.test(name),
        )
        .sort(([left], [right]) => left.localeCompare(right)),
    ]),
  );
  const cold =
    force || previous.configuration !== configuration || !previous.hashes;
  const selected = affected(root, current, cold ? {} : previous.hashes);
  if (cold)
    selected.packages = current.packages.map((item) => item.name).sort();
  const bindings = bindingsHash(root, current);
  const checkIpc = cold || bindings !== previous.bindings;
  const checkUi =
    cold ||
    selected.changed.some(
      (path) => path.startsWith("ui/") || path === "scripts/check-ui.mjs",
    );
  return { current, configuration, selected, bindings, checkIpc, checkUi };
}

function run(plan) {
  const { selected, checkIpc, checkUi } = plan;
  console.log(
    `Fast check: ${selected.packages.join(", ") || "no affected Rust crates"}; bindings=${checkIpc}; UI=${checkUi}`,
  );
  if (planOnly) return;
  // UI build first: Tauri's build script reads ui/dist even for cargo check.
  const missingUiBuild = !existsSync(join(root, "ui", "dist", "index.html"));
  if (checkUi || missingUiBuild) {
    command("node", [
      "scripts/check-ui.mjs",
      ...(missingUiBuild ? ["--force"] : []),
    ]);
  }
  if (selected.packages.length) {
    const packages = selected.packages.flatMap((name) => ["-p", name]);
    command("cargo", ["check", "--locked", "--all-targets", ...packages]);
    command("cargo", [
      "clippy",
      "--locked",
      "--all-targets",
      ...packages,
      "--",
      "-D",
      "warnings",
    ]);
  }
  if (checkIpc) checkBindings();
  writeFileSync(
    `${stamp}.tmp`,
    JSON.stringify({
      version: 1,
      configuration: plan.configuration,
      hashes: plan.current.hashes,
      bindings: plan.bindings,
    }),
  );
  renameSync(`${stamp}.tmp`, stamp);
}

const started = performance.now();
let before;
if (!planOnly) {
  mkdirSync(state, { recursive: true });
  rmSync(feed, { force: true });
  before = cacheStats();
}
try {
  const wanted = plan();
  // Remove the old success before executing anything: interrupted or failed
  // checks, including bisection runs, must retry everything they did not pass.
  if (!planOnly) rmSync(stamp, { force: true });
  run(wanted);
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
} finally {
  if (!planOnly) {
    const elapsed = ((performance.now() - started) / 1000).toFixed(2);
    const line = `fast check ${process.exitCode ? "failed" : "passed"} in ${elapsed}s; ${cacheLine(before, cacheStats())}`;
    writeFileSync(feed, line);
    console.log(line);
  }
}
