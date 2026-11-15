// Replay the last 20 landed trees through the old build set and the fast set.
// Run: node scripts/measure-checks.mjs. The replay takes one rosen run turn.
// Warm-up is reported separately; timings exclude queue wait and checkout.
// Sources and output stay under target/; no changes are created.
import { spawn, spawnSync } from "node:child_process";
import { clearInterval, setInterval } from "node:timers";
import { createConnection } from "node:net";
import {
  existsSync,
  mkdirSync,
  openSync,
  closeSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
  writeSync,
} from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { digest, physicalRoot } from "./check-inputs.mjs";

const root = physicalRoot(fileURLToPath(import.meta.url));
const script = fileURLToPath(import.meta.url);
const runName =
  process.argv.find((argument) => argument.startsWith("--name="))?.slice(7) ??
  "check-benchmark-final";
if (!/^[\w-]+$/.test(runName))
  throw new Error("Benchmark name must be a folder name.");
const mode = process.argv[2];
const work =
  ["--fast-run", "--baseline-run"].includes(mode) && process.argv[5]
    ? resolve(process.argv[5])
    : join(root, "target", runName);

function command(program, arguments_, folder, environment = process.env) {
  const result = spawnSync(program, arguments_, {
    cwd: folder,
    env: environment,
    stdio: "inherit",
    windowsHide: true,
  });
  if (result.status !== 0)
    throw new Error(
      `${program} ${arguments_.join(" ")}: ${result.status ?? result.error}`,
    );
}

function measuredRun(folder, fast) {
  if (fast) {
    const progress = JSON.parse(
      readFileSync(join(work, "progress.json"), "utf8"),
    );
    if (progress.revision !== revision())
      throw new Error(
        "Checker changed during replay; use a new --name folder.",
      );
  }
  const started = performance.now();
  let passed = true;
  try {
    if (fast) command("node", ["scripts/check-fast.mjs"], folder);
    else {
      // The original checks.pre_land set at the sampled tip, held constant
      // across the replay. No tests were in that set.
      command("cargo", ["fmt", "--all", "--", "--check"], folder);
      command("node", ["scripts/check-ui.mjs"], folder);
      command("cargo", ["build", "--workspace", "--locked"], folder);
      command(
        "cargo",
        [
          "run",
          "--locked",
          "-p",
          "velocity-app",
          "--bin",
          "export-bindings",
          "--",
          "--check",
        ],
        folder,
      );
      command(
        "cargo",
        [
          "clippy",
          "--workspace",
          "--all-targets",
          "--locked",
          "--",
          "-D",
          "warnings",
        ],
        folder,
      );
    }
  } catch (error) {
    console.error(error.message);
    passed = false;
  }
  const seconds = (performance.now() - started) / 1000;
  mkdirSync(join(folder, "target"), { recursive: true });
  writeFileSync(
    join(folder, "target", "measurement.json"),
    JSON.stringify({ seconds, passed, sample: process.argv[4] }),
  );
}

// Rosen's documented v1 length-prefixed JSON transport, used read-only. The CLI
// doesn't expose read_file; it supplies the history, and this reads those trees.
async function connection() {
  const socket = createConnection(process.env.ROSEN_ENDPOINT);
  await new Promise((accept, reject) => {
    socket.once("connect", accept);
    socket.once("error", reject);
  });
  let buffer = Buffer.alloc(0);
  const waiting = [];
  socket.on("data", (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    while (buffer.length >= 4 && buffer.length >= 4 + buffer.readUInt32BE(0)) {
      const length = buffer.readUInt32BE(0);
      const frame = JSON.parse(buffer.subarray(4, 4 + length));
      buffer = buffer.subarray(4 + length);
      waiting.shift()(frame);
    }
  });
  function send(frame) {
    return new Promise((accept) => {
      waiting.push(accept);
      const bytes = Buffer.from(JSON.stringify(frame));
      const header = Buffer.alloc(4);
      header.writeUInt32BE(bytes.length);
      socket.write(Buffer.concat([header, bytes]));
    });
  }
  const welcome = await send({
    type: "hello",
    versions: [1],
    client: "velocity-check-benchmark",
  });
  if (welcome.type !== "welcome") throw new Error(JSON.stringify(welcome));
  let id = 0;
  return {
    close: () => socket.destroy(),
    request: async (method, params) => {
      const frame = await send({
        type: "request",
        id: ++id,
        request: { method, params },
      });
      if (frame.error) throw new Error(JSON.stringify(frame.error));
      return frame.ok.body;
    },
  };
}

function safePath(folder, path) {
  const target = resolve(folder, path);
  const inside = relative(folder, target);
  if (
    inside.startsWith("..") ||
    isAbsolute(inside) ||
    resolve(folder) === target
  )
    throw new Error(`Invalid tree path ${path}`);
  return target;
}

async function fileBytes(client, answer) {
  if (typeof answer.data === "string")
    return Buffer.from(answer.data, "base64");
  if (!answer.id || !Number.isSafeInteger(answer.len) || answer.len < 0)
    throw new Error("Unsupported Rosen file answer.");
  const chunks = [];
  let offset = 0;
  while (offset < answer.len) {
    const chunk = await client.request("get_object_chunk", {
      id: answer.id,
      offset,
      len: Math.min(16 * 1024 * 1024, answer.len - offset),
    });
    if (chunk.kind !== "blob" || chunk.total !== answer.len)
      throw new Error(`Invalid blob chunk for ${answer.id}.`);
    const bytes = Buffer.from(chunk.data, "base64");
    if (!bytes.length || offset + bytes.length > answer.len)
      throw new Error(`Incomplete blob ${answer.id}.`);
    chunks.push(bytes);
    offset += bytes.length;
  }
  return Buffer.concat(chunks);
}

async function checkout(client, commit, folder, listed = []) {
  const files = [];
  async function visit(path) {
    const answer = await client.request("read_file", { at: commit, path });
    if (answer.kind === "directory") {
      for (const entry of answer.entries)
        await visit(path ? `${path}/${entry.name}` : entry.name);
    } else {
      const target = safePath(folder, path);
      const bytes = await fileBytes(client, answer);
      files.push(path);
      if (!existsSync(target) || !readFileSync(target).equals(bytes)) {
        mkdirSync(dirname(target), { recursive: true });
        writeFileSync(target, bytes);
      }
    }
  }
  await visit("");
  for (const gone of listed.filter((path) => !files.includes(path)))
    rmSync(safePath(folder, gone), { force: true });
  return files;
}

function overlay(folder, fast) {
  const paths = fast
    ? [
        "scripts/check-fast.mjs",
        "scripts/check-inputs.mjs",
        "scripts/check-ui.mjs",
        ".cargo/config.toml",
      ]
    : ["scripts/check-ui.mjs"];
  for (const path of paths) {
    mkdirSync(dirname(join(folder, path)), { recursive: true });
    if (path === ".cargo/config.toml" && !existsSync(join(root, path))) {
      rmSync(join(folder, path), { force: true });
      continue;
    }
    const bytes = readFileSync(join(root, path));
    if (
      !existsSync(join(folder, path)) ||
      !readFileSync(join(folder, path)).equals(bytes)
    ) {
      writeFileSync(join(folder, path), bytes);
    }
  }
}

function loggedWorker(folder, fast, name, environment, log) {
  const child = spawn(
    process.execPath,
    [script, fast ? "--fast-run" : "--baseline-run", folder, name, work],
    {
      cwd: root,
      env: environment,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    },
  );
  const forward = (bytes) => {
    writeSync(log, bytes);
    process.stdout.write(bytes);
  };
  child.stdout.on("data", forward);
  child.stderr.on("data", forward);
  const heartbeat = setInterval(
    () => console.log(`Replay ${name}: check running.`),
    20000,
  );
  return new Promise((accept, reject) => {
    child.once("error", reject);
    child.once("close", (status) =>
      status === 0
        ? accept()
        : reject(new Error(`Could not measure ${name}: ${status}`)),
    );
  }).finally(() => clearInterval(heartbeat));
}

async function sample(folder, fast, name) {
  const log = openSync(
    join(work, `${name}-${fast ? "after" : "before"}.log`),
    "w",
  );
  const environment = { ...process.env };
  environment.VELOCITY_CHECK_REPLAY = "1";
  // Baseline trees predate .cargo/config.toml; counteract the ancestor config
  // because the scratch folders live under this workspace's ignored target/.
  if (!fast) {
    environment.CARGO_PROFILE_DEV_INCREMENTAL = "true";
  }
  const started = performance.now();
  try {
    await loggedWorker(folder, fast, name, environment, log);
  } finally {
    closeSync(log);
  }
  const result = JSON.parse(
    readFileSync(join(folder, "target", "measurement.json"), "utf8"),
  );
  if (result.sample !== name) throw new Error(`Missing result for ${name}`);
  result.wall_seconds = (performance.now() - started) / 1000;
  const report = join(folder, "target", ".velocity-checks", "feed.txt");
  if (fast && existsSync(report))
    result.telemetry = readFileSync(report, "utf8");
  return result;
}

function saveProgress(progress) {
  const path = join(work, "progress.json");
  writeFileSync(`${path}.tmp`, JSON.stringify(progress, null, 2));
  renameSync(`${path}.tmp`, path);
  writeFileSync(
    join(work, "results.json"),
    JSON.stringify(progress.rows, null, 2),
  );
}

function revision() {
  const files = [
    "scripts/check-fast.mjs",
    "scripts/check-inputs.mjs",
    "scripts/check-ui.mjs",
    ".cargo/config.toml",
  ];
  return digest(
    Buffer.concat(
      files.map((path) =>
        path === ".cargo/config.toml" && !existsSync(join(root, path))
          ? Buffer.from("<missing .cargo/config.toml>")
          : readFileSync(join(root, path)),
      ),
    ),
  );
}

function newProgress(history, prior, legacy) {
  const seeded = new Map(prior.map((row) => [row.commit, row.before]));
  const rows = history.entries.reverse().map((entry, index) => ({
    commit: entry.commit,
    summary: entry.summary,
    warmup: index === 0,
    before: seeded.get(entry.commit),
  }));
  return {
    revision: revision(),
    rows,
    folders: {
      before: prior.length
        ? join(dirname(legacy), "before")
        : join(work, "before"),
      after: join(work, "after"),
    },
    positions: {
      before: prior.filter((row) => row.before).at(-1)?.commit,
      after: null,
    },
    listed: { before: [], after: [] },
    pending: null,
  };
}

async function startProgress(client) {
  mkdirSync(work, { recursive: true });
  const path = join(work, "progress.json");
  if (existsSync(path)) {
    const progress = JSON.parse(readFileSync(path, "utf8"));
    if (progress.revision !== revision())
      throw new Error("Checker changed: rerun with --name=another-folder.");
    return progress;
  }
  const history = await client.request("log", {
    at: "base",
    paths: [],
    limit: 21,
  });
  if (history.entries.length < 21)
    throw new Error("Need 20 base commits and their parent.");
  const legacy = join(root, "target", "check-benchmark", "results.json");
  const prior =
    process.argv.includes("--reuse-before") && existsSync(legacy)
      ? JSON.parse(readFileSync(legacy, "utf8"))
      : [];
  const progress = newProgress(history, prior, legacy);
  saveProgress(progress);
  return progress;
}

async function pathsAt(client, commit, path = "") {
  const answer = await client.request("read_file", { at: commit, path });
  if (answer.kind !== "directory") return [path];
  const paths = [];
  for (const entry of answer.entries)
    paths.push(
      ...(await pathsAt(
        client,
        commit,
        path ? `${path}/${entry.name}` : entry.name,
      )),
    );
  return paths;
}

async function resumeProgress(client, progress) {
  for (const mode of ["before", "after"]) {
    const position = progress.positions[mode];
    if (position && !progress.listed[mode].length)
      progress.listed[mode] = await pathsAt(client, position);
  }
  if (progress.pending) {
    const { mode, commit } = progress.pending;
    const union = new Set([
      ...progress.listed[mode],
      ...(await pathsAt(client, commit)),
    ]);
    progress.listed[mode] = await checkout(
      client,
      commit,
      progress.folders[mode],
      [...union],
    );
    progress.positions[mode] = commit;
    progress.pending = null;
  }
  saveProgress(progress);
}

async function runSample(client, progress, index, fast) {
  if (progress.revision !== revision())
    throw new Error("Checker changed during replay; use a new --name folder.");
  const row = progress.rows[index];
  const mode = fast ? "after" : "before";
  if (row[mode]) return;
  const folder = progress.folders[mode];
  progress.pending = { mode, commit: row.commit };
  saveProgress(progress);
  progress.listed[mode] = await checkout(
    client,
    row.commit,
    folder,
    progress.listed[mode],
  );
  progress.positions[mode] = row.commit;
  saveProgress(progress);
  overlay(folder, fast);
  row[mode] = await sample(folder, fast, String(index).padStart(2, "0"));
  progress.pending = null;
  saveProgress(progress);
  console.log(
    `${index === 0 ? "warmup" : `${index}/20`} ${row.commit.slice(0, 8)} ${mode}: ${JSON.stringify(row[mode])}`,
  );
}

async function replay() {
  if (!process.env.ROSEN_ENDPOINT)
    throw new Error("ROSEN_ENDPOINT required; run from the Rosen workspace.");
  const client = await connection();
  try {
    const progress = await startProgress(client);
    const submitted = Number(
      process.argv
        .find((argument) => argument.startsWith("--submitted="))
        ?.slice(12),
    );
    writeFileSync(
      join(work, "queue.json"),
      JSON.stringify({
        queue_and_startup_seconds: (Date.now() - submitted) / 1000,
      }),
    );
    await resumeProgress(client, progress);
    for (const [index] of progress.rows.entries()) {
      const modes = process.argv.includes("--before-only")
        ? [false]
        : process.argv.includes("--after-only")
          ? [true]
          : index % 2
            ? [true, false]
            : [false, true];
      for (const fast of modes) await runSample(client, progress, index, fast);
    }
  } finally {
    client.close();
  }
}

async function verifyChange() {
  const progress = JSON.parse(
    readFileSync(join(work, "progress.json"), "utf8"),
  );
  if (!progress.rows.every((row) => row.before?.passed && row.after?.passed)) {
    throw new Error(
      "Finish the timing replay before reusing its build folder.",
    );
  }
  const client = await connection();
  try {
    await resumeProgress(client, progress);
    const merge = await client.request("merge_result", {
      change: process.env.ROSEN_CHANGE,
    });
    if (!merge.tree || merge.conflicts.length)
      throw new Error(`Merge conflicts: ${merge.conflicts.join(", ")}`);
    const folder = join(work, "after");
    progress.pending = { mode: "after", commit: merge.tree };
    saveProgress(progress);
    progress.listed.after = await checkout(
      client,
      merge.tree,
      folder,
      progress.listed.after,
    );
    progress.positions.after = merge.tree;
    progress.pending = null;
    saveProgress(progress);
    command(
      "rosen",
      ["run", "--", "node", join(folder, "scripts/check-full.mjs")],
      folder,
    );
    writeFileSync(
      join(work, "merge-check.json"),
      JSON.stringify(merge, null, 2),
    );
    console.log(
      `Full checks passed on merge ${merge.tree} onto ${merge.base}.`,
    );
  } finally {
    client.close();
  }
}

if (mode === "--fast-run" || mode === "--baseline-run") {
  if (process.env.VELOCITY_CHECK_REPLAY !== "1")
    throw new Error(
      "Replay worker needs the queued batch; restart the replay.",
    );
  measuredRun(resolve(process.argv[3]), mode === "--fast-run");
} else if (mode === "--verify-change") await verifyChange();
else if (mode === "--queued-run") await replay();
else
  command(
    "rosen",
    [
      "run",
      "--",
      "node",
      script,
      "--queued-run",
      `--name=${runName}`,
      `--submitted=${Date.now()}`,
      ...["--reuse-before", "--before-only", "--after-only"].filter((flag) =>
        process.argv.includes(flag),
      ),
    ],
    root,
  );
