// Instruction drift check (#534): verifies every CLI command, flag and MCP tool
// referenced in AGENTS.md, the default AGENTS.md template, and Velocity's opening
// workspace/session instructions against `rosen --help` and Velocity's MCP tool lists.
// Usage: node scripts/check-instruction-drift.mjs
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { physicalRoot } from "./check-inputs.mjs";

const root = physicalRoot(fileURLToPath(import.meta.url));
const errors = [];

function fail(message) {
  errors.push(message);
}

function readText(relativePath) {
  return readFileSync(join(root, relativePath), "utf8");
}

function findRosenBinary() {
  const candidates = [];
  if (process.env.VELOCITY_ROSEN_CLI_EXE) {
    candidates.push(process.env.VELOCITY_ROSEN_CLI_EXE);
  }
  if (process.env.VELOCITY_ROSEN_BIN) {
    candidates.push(
      join(
        process.env.VELOCITY_ROSEN_BIN,
        process.platform === "win32" ? "rosen.exe" : "rosen",
      ),
    );
  }
  candidates.push("rosen");
  for (const candidate of candidates) {
    const probe = spawnSync(candidate, ["--help"], {
      cwd: root,
      encoding: "utf8",
      windowsHide: true,
    });
    if (probe.status === 0 && probe.stdout) {
      return { binary: candidate, rootHelp: probe.stdout };
    }
  }
  return null;
}

function parseHelpCommands(helpText) {
  const commands = new Set();
  let inCommands = false;
  for (const line of helpText.split(/\r?\n/)) {
    if (/^Commands:\s*$/.test(line)) {
      inCommands = true;
      continue;
    }
    if (inCommands) {
      if (line.trim() === "" || /^\S/.test(line)) {
        if (commands.size > 0) inCommands = false;
        continue;
      }
      const match = line.match(/^\s{2,}([a-z][a-z0-9-]*)\b/);
      if (match) commands.add(match[1]);
    }
  }
  return commands;
}

function parseHelpFlags(helpText) {
  const flags = new Set();
  for (const match of helpText.matchAll(/(?:^|[\s,])(--[a-z0-9-]+|-[a-zA-Z])\b/g)) {
    flags.add(match[1]);
  }
  return flags;
}

function extractBacktickedSpans(text) {
  const spans = [];
  for (const match of text.matchAll(/`([^`\r\n]+)`/g)) {
    spans.push(match[1].trim());
  }
  return spans;
}

function tokenizeCommand(command) {
  const tokens = [];
  let current = "";
  let quote = null;
  for (let i = 0; i < command.length; i += 1) {
    const ch = command[i];
    if (quote) {
      if (ch === quote) {
        quote = null;
      } else {
        current += ch;
      }
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
      continue;
    }
    if (/\s/.test(ch)) {
      if (current) {
        tokens.push(current);
        current = "";
      }
      continue;
    }
    current += ch;
  }
  if (current) tokens.push(current);
  return tokens;
}

function collectMcpToolsFromCode() {
  const tools = new Set();
  const addMatch = (text, regex) => {
    for (const match of text.matchAll(regex)) {
      tools.add(match[1]);
    }
  };

  // AIRC tools (protocol.rs, task_tools.rs, issue_tools.rs, next_work_tools.rs)
  const aircFiles = [
    "crates/velocity-airc/src/protocol.rs",
    "crates/velocity-airc/src/task_tools.rs",
    "crates/velocity-airc/src/issue_tools.rs",
    "crates/velocity-airc/src/next_work_tools.rs",
  ];
  for (const file of aircFiles) {
    if (!existsSync(join(root, file))) continue;
    const text = readText(file);
    addMatch(text, /(?:protocol::)?tool\(\s*"([a-z0-9_]+)"/g);
    addMatch(text, /const [A-Z0-9_]+_TOOL:\s*&str\s*=\s*"([a-z0-9_]+)"/g);
  }

  // Graph tools
  const graphMcp = readText("crates/velocity-graph/src/mcp.rs");
  addMatch(graphMcp, /\btool\(\s*"([a-z0-9_]+)"/g);

  // Browser tools
  const browserTools = readText("crates/velocity-browser/src/tools.rs");
  addMatch(browserTools, /\btool\(\s*"([a-z0-9_]+)"/g);

  // velocity-run tools
  for (const file of [
    "crates/velocity-acp/src/client/queued_run_tool.rs",
    "crates/velocity-acp/src/client/preflight_tool.rs",
    "crates/velocity-acp/src/client/workflow_run_tool.rs",
  ]) {
    const text = readText(file);
    addMatch(text, /pub const [A-Z0-9_]+_TOOL:\s*&str\s*=\s*"([a-z0-9_]+)"/g);
  }

  return tools;
}

function collectRosenMcpTools(rosenBinary) {
  const tools = new Set();
  if (!rosenBinary) return tools;
  const payload = [
    JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        protocolVersion: "2024-11-05",
        capabilities: {},
        clientInfo: { name: "drift-check", version: "1.0" },
      },
    }),
    JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/list", params: {} }),
  ].join("\n") + "\n";

  const result = spawnSync(rosenBinary, ["mcp", "--workspace", root], {
    cwd: root,
    input: payload,
    encoding: "utf8",
    windowsHide: true,
    timeout: 10_000,
  });
  if (result.status !== 0 || !result.stdout) return tools;
  for (const line of result.stdout.split(/\r?\n/)) {
    if (!line.trim()) continue;
    try {
      const message = JSON.parse(line);
      const list = message?.result?.tools;
      if (Array.isArray(list)) {
        for (const item of list) {
          if (typeof item?.name === "string") tools.add(item.name);
        }
      }
    } catch {
      // Ignore non-JSON lines.
    }
  }
  return tools;
}

function isLikelyMcpToolName(token) {
  if (!/^[a-z][a-z0-9_]*$/.test(token)) return false;
  return (
    token.startsWith("graph_") ||
    token.startsWith("airc_") ||
    token.startsWith("task_") ||
    token.startsWith("issue_") ||
    token.startsWith("read_") ||
    token.startsWith("append_") ||
    [
      "send_message",
      "list_nicks",
      "next_work",
      "queue_run",
      "run_status",
      "run_cancel",
      "workflow_run",
      "preflight",
      "snapshot",
      "screenshot",
    ].includes(token)
  );
}

const rosen = findRosenBinary();
const helpCache = new Map();

function rosenSubcommandHelp(subcommand) {
  if (!rosen) return null;
  if (helpCache.has(subcommand)) return helpCache.get(subcommand);
  const result = spawnSync(rosen.binary, [subcommand, "--help"], {
    cwd: root,
    encoding: "utf8",
    windowsHide: true,
  });
  const text = result.status === 0 ? result.stdout : null;
  helpCache.set(subcommand, text);
  return text;
}

const rosenCommands = rosen ? parseHelpCommands(rosen.rootHelp) : null;
const mcpTools = collectMcpToolsFromCode();
const rosenMcpTools = collectRosenMcpTools(rosen?.binary ?? null);
for (const name of rosenMcpTools) mcpTools.add(name);

const auditedFiles = [
  "AGENTS.md",
  "crates/velocity-core/src/agents_template.rs",
  "crates/velocity-rosen/src/agent.rs",
  "crates/velocity-acp/src/client/session_card.rs",
  "crates/velocity-acp/src/client/queued_run_tool.rs",
  "crates/velocity-acp/src/client/workflow_run_tool.rs",
  "crates/velocity-acp/src/client/run_queue.rs",
];

let checkedRosenCommands = 0;
let checkedMcpTools = 0;

for (const file of auditedFiles) {
  const raw = readText(file);
  // Strip Rust test blocks so negative assertions in tests are not mistaken for docs.
  const text = file.endsWith(".rs")
    ? raw.split("#[cfg(test)]")[0]
    : raw;
  const spans = extractBacktickedSpans(text);

  for (const span of spans) {
    // 1. Verify every `rosen ...` CLI command and its flags against `rosen --help`.
    if (span.startsWith("rosen ")) {
      const tokens = tokenizeCommand(span);
      const sub = tokens[1];
      if (!sub || sub.startsWith("<")) continue;
      checkedRosenCommands += 1;
      if (rosenCommands && sub !== "--help" && !rosenCommands.has(sub)) {
        fail(`${file}: unknown rosen subcommand \`${sub}\` in \`${span}\``);
        continue;
      }
      if (rosen && sub !== "--help") {
        const subHelp = rosenSubcommandHelp(sub);
        if (!subHelp) {
          fail(`${file}: \`rosen ${sub} --help\` failed for \`${span}\``);
          continue;
        }
        const validFlags = parseHelpFlags(subHelp);
        for (const token of tokens.slice(2)) {
          if (token === "--") break;
          if (!token.startsWith("-") || token.startsWith("<")) continue;
          const flag = token.split("=")[0];
          if (!validFlags.has(flag)) {
            fail(`${file}: unknown flag \`${flag}\` on \`rosen ${sub}\` in \`${span}\``);
          }
        }
      }
      continue;
    }

    // 2. Verify every MCP tool name referenced in backticks exists in Velocity's MCP catalog.
    if (isLikelyMcpToolName(span)) {
      checkedMcpTools += 1;
      if (!mcpTools.has(span)) {
        fail(`${file}: unknown or retired MCP tool \`${span}\``);
      }
    }
  }
}

// 3. Catch the `rosen claim` exclusive-flag mismatch (#534).
const claimHelp = rosenSubcommandHelp("claim");
const claimNeedsExclusive =
  claimHelp !== null &&
  claimHelp.includes("--exclusive") &&
  /first[- ]claim[- ]wins/i.test(claimHelp);

for (const file of ["AGENTS.md", "crates/velocity-core/src/agents_template.rs"]) {
  const text = readText(file);
  if (claimNeedsExclusive) {
    for (const line of text.split(/\r?\n/)) {
      if (/first.*claim wins/i.test(line) && /`rosen claim\b/.test(line)) {
        if (!/`rosen claim --exclusive\b/.test(line)) {
          fail(
            `${file}: describes first-claim-wins without \`rosen claim --exclusive\`: ${line.trim()}`,
          );
        }
      }
    }
  }

  // 4. Contradiction guards (#534).
  if (/integration build[^.;\n]*stress/i.test(text)) {
    fail(`${file}: claims the integration build runs stress (the release gate runs stress)`);
  }
  if (/rosen run -- cargo check/i.test(text)) {
    fail(`${file}: recommends \`rosen run -- cargo check\` instead of the single \`queue_run\` rule`);
  }
  if (/more than a minute/i.test(text)) {
    fail(`${file}: contains duration-based \`queue_run\` rule ("more than a minute")`);
  }
}

for (const file of [
  "crates/velocity-rosen/src/agent.rs",
  "crates/velocity-acp/src/client/queued_run_tool.rs",
  "crates/velocity-acp/src/client/run_queue.rs",
]) {
  const text = readText(file).split("#[cfg(test)]")[0];
  if (/integration build[^.;\n]*stress/i.test(text)) {
    fail(`${file}: claims the integration build runs stress`);
  }
  if (/rosen run -- cargo check/i.test(text)) {
    fail(`${file}: recommends \`rosen run -- cargo check\` instead of \`queue_run\``);
  }
  if (/more than a minute/i.test(text)) {
    fail(`${file}: contains duration-based \`queue_run\` rule ("more than a minute")`);
  }
}

// 5. Verify AGENTS.md defines core Velocity terms and documents the cargo shim.
const agentsMd = readText("AGENTS.md");
for (const required of [
  ["Session card", /\*\*Session card\*\*/],
  ["Scout", /\*\*Scout\*\*/],
  ["Review swarm", /\*\*Review swarm\*\*/],
  ["Receipts", /\*\*Receipts\*\*/],
  ["cargo shim", /`cargo` shim/],
]) {
  if (!required[1].test(agentsMd)) {
    fail(`AGENTS.md: missing definition/documentation for ${required[0]}`);
  }
}
for (const forbidden of [
  ["dangling 'the workspace note'", /\bthe workspace note\b/i],
  ["dangling 'AIRC Task List instructions'", /\bAIRC Task List instructions\b/i],
]) {
  if (forbidden[1].test(agentsMd)) {
    fail(`AGENTS.md: contains ${forbidden[0]}`);
  }
}

// 6. Verify cargo commands documented as allowed in AGENTS.md match agent_run.rs policy.
const agentRunSource = readText("crates/velocity-rosen/src/agent_run.rs");
if (!agentRunSource.includes('command == "fmt"')) {
  fail("crates/velocity-rosen/src/agent_run.rs: cargo fmt policy drifted");
}
if (!agentRunSource.includes('command == "check" && flag == "-p"')) {
  fail("crates/velocity-rosen/src/agent_run.rs: cargo check -p policy drifted");
}
if (!agentRunSource.includes('command == "metadata" && flag == "--format-version"')) {
  fail("crates/velocity-rosen/src/agent_run.rs: cargo metadata policy drifted");
}

if (errors.length > 0) {
  console.error("Instruction drift check failed:");
  for (const error of errors) {
    console.error(`  - ${error}`);
  }
  process.exit(1);
}

console.log(
  `Instruction drift check passed (${checkedRosenCommands} rosen CLI refs, ${checkedMcpTools} MCP tool refs, ${mcpTools.size} live MCP tools, rosen CLI=${rosen ? "verified" : "unavailable"}).`,
);
