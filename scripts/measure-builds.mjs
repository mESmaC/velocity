// Times Velocity's local builds (#435): a cold `cargo build --workspace`, a rebuild after a
// one-crate edit, and `scripts/check-fast.mjs` after the same edit.
//
// usage: node scripts/measure-builds.mjs <work-folder> [--source <tree>] [--label <name>]
//          [--edit <crate,...>] [--cases cold,edit,fast] [--repeat <n>]
//
// The source tree (default: this checkout) is copied to <work-folder>/<label>/tree first,
// so the timing edits never land in a Rosen snapshot of the measured workspace. Each case
// keeps Cargo's HTML timing report in <work-folder>/<label>/reports, and every result is
// appended to <work-folder>/results.json. Run each label alone on the machine: concurrent
// builds change the timings more than most of the fixes being measured. Run it from an
// ordinary terminal: agent shells refuse `cargo build` (#480), so agents cannot measure.
//
// The cold case runs without sccache, so it measures compilation rather than the cache.
// The edit adds a private function to the crate's library root, which rebuilds the crate
// and every workspace crate that depends on it, like a function-body edit.
import { spawnSync } from "node:child_process";
import {
  copyFileSync,
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { basename, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { physicalRoot } from "./check-inputs.mjs";

const options = parseOptions(process.argv.slice(2));
const run = join(options.work, options.label);
const tree = join(run, "tree");
const reports = join(run, "reports");
// Build products, installed packages and Rosen's own state are not sources.
const skipped = new Set([
  ".rosen",
  ".tools",
  "target",
  "node_modules",
  "dist",
  ".lake",
  ".lake-wsl",
  "generated-llbc",
  "gen",
]);

function parseOptions(argv) {
  const value = (flag, fallback) => {
    const index = argv.indexOf(flag);
    return index >= 0 && argv[index + 1] ? argv[index + 1] : fallback;
  };
  const work = argv[0] && !argv[0].startsWith("--") ? resolve(argv[0]) : null;
  if (!work)
    throw new Error("usage: measure-builds.mjs <work-folder> [options]");
  const label = value("--label", "current");
  if (!/^[\w.-]+$/.test(label))
    throw new Error("--label must be a folder name");
  return {
    work,
    label,
    source: resolve(
      value("--source", physicalRoot(fileURLToPath(import.meta.url))),
    ),
    edits: value("--edit", "velocity-core,velocity-app")
      .split(",")
      .filter(Boolean),
    cases: value("--cases", "cold,edit,fast").split(","),
    repeat: Number.parseInt(value("--repeat", "1"), 10),
  };
}

function environment() {
  const variables = { ...process.env, CARGO_TARGET_DIR: join(tree, "target") };
  if (process.platform === "win32")
    variables.Path = `${process.env.SystemRoot}\\System32;${process.env.Path}`;
  // An empty wrapper turns sccache off for Cargo; check-fast.mjs still adds its own.
  variables.RUSTC_WRAPPER = "";
  variables.RUSTC_WORKSPACE_WRAPPER = "";
  return variables;
}

function command(program, arguments_, folder = tree) {
  console.log(`> ${program} ${arguments_.join(" ")}`);
  const started = performance.now();
  const result = spawnSync(program, arguments_, {
    cwd: folder,
    env: environment(),
    stdio: "inherit",
    windowsHide: true,
    shell: program === "pnpm",
  });
  if (result.status !== 0)
    throw new Error(
      `${program} ${arguments_.join(" ")}: ${result.status ?? result.error}`,
    );
  return (performance.now() - started) / 1000;
}

function copyTree() {
  rmSync(tree, { recursive: true, force: true });
  mkdirSync(reports, { recursive: true });
  cpSync(options.source, tree, {
    recursive: true,
    filter: (path) => !skipped.has(basename(path)) || path === options.source,
  });
  // Tauri's context macro embeds ui/dist even in `cargo build`, so build the UI once.
  command("pnpm", ["-C", "ui", "install", "--frozen-lockfile"]);
  command("pnpm", ["-C", "ui", "build"]);
}

/** Keeps Cargo's report under the case's name and lists its slowest units. */
function keepReport(name) {
  const report = join(tree, "target", "cargo-timings", "cargo-timing.html");
  if (!existsSync(report)) return [];
  copyFileSync(report, join(reports, `${name}.html`));
  const units = /const UNIT_DATA = (\[.*?\]);/s.exec(
    readFileSync(report, "utf8"),
  );
  if (!units) return [];
  return JSON.parse(units[1])
    .sort((left, right) => right.duration - left.duration)
    .slice(0, 8)
    .map(
      (unit) =>
        `${unit.name} ${unit.target.trim()} ${unit.duration.toFixed(1)}s`,
    );
}

function libraryRoot(crate) {
  const metadata = JSON.parse(
    spawnSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], {
      cwd: tree,
      env: environment(),
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
      windowsHide: true,
    }).stdout,
  );
  const target = metadata.packages
    .find((item) => item.name === crate)
    ?.targets.find((item) => item.kind.includes("lib"));
  if (!target) throw new Error(`${crate} has no library target`);
  return target.src_path;
}

/** Runs `measure` with a fresh private function appended to the crate's library root. */
function withEdit(crate, sequence, measure) {
  const path = libraryRoot(crate);
  const original = readFileSync(path);
  const marker = `${Date.now()}${sequence}`;
  writeFileSync(
    path,
    `${original}\n#[allow(dead_code)]\nfn measure_builds_edit() -> u64 {\n    ${marker}\n}\n`,
  );
  try {
    return measure();
  } finally {
    writeFileSync(path, original);
  }
}

function record(entry) {
  const file = join(options.work, "results.json");
  const results = existsSync(file)
    ? JSON.parse(readFileSync(file, "utf8"))
    : [];
  const saved = {
    label: options.label,
    time: new Date().toISOString(),
    ...entry,
  };
  results.push(saved);
  writeFileSync(file, `${JSON.stringify(results, null, 2)}\n`);
  console.log(JSON.stringify(saved, null, 2));
}

const build = ["build", "--workspace", "--locked", "--timings"];

function measureCold() {
  rmSync(join(tree, "target"), { recursive: true, force: true });
  const seconds = command("cargo", build);
  record({ case: "cold build", seconds, slowest: keepReport("cold") });
}

function measureEdits() {
  for (const crate of options.edits) {
    for (let attempt = 1; attempt <= options.repeat; attempt += 1) {
      const seconds = withEdit(crate, attempt, () => command("cargo", build));
      const slowest = keepReport(`edit-${crate}-${attempt}`);
      record({
        case: `one-crate edit build (${crate})`,
        attempt,
        seconds,
        slowest,
      });
      // Rebuild the restored tree so the next attempt starts from the same state.
      command("cargo", build);
    }
  }
}

function measureFast() {
  // The first pass checks everything and writes check-fast's stamp; it is not measured.
  command("node", ["scripts/check-fast.mjs"]);
  for (const crate of options.edits) {
    for (let attempt = 1; attempt <= options.repeat; attempt += 1) {
      const seconds = withEdit(crate, attempt, () =>
        command("node", ["scripts/check-fast.mjs"]),
      );
      record({ case: `check-fast after edit (${crate})`, attempt, seconds });
      command("node", ["scripts/check-fast.mjs"]);
    }
  }
}

copyTree();
console.log(`Measuring ${relative(options.work, run)} from ${options.source}`);
console.log(`CARGO_BUILD_JOBS=${process.env.CARGO_BUILD_JOBS ?? "(default)"}`);
if (options.cases.includes("cold")) measureCold();
else command("cargo", build);
if (options.cases.includes("edit")) measureEdits();
if (options.cases.includes("fast")) measureFast();
console.log(`Reports: ${readdirSync(reports).length} in ${reports}`);
