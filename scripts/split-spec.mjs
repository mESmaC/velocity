// #400, part 2. Run against a monolithic SPEC.md on any newer base:
//   node scripts/split-spec.mjs [checkout]
// Recover the moved text in its original order (IPC lists become table rows):
//   node scripts/split-spec.mjs --join [checkout]
// Source-order comments are migration metadata, not section IDs. Keep them when
// editing a moved file if you want --join to keep working. No source text is copied
// into the script, so replaying the move never reinstates an older spec.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";

const indexMarker = "<!-- split-spec index -->";
const sourceMarker = /<!-- split-spec: (.*?) -->\n/;
const rowMarker = "<!-- split-spec IPC row -->";
const navigationMarker = "<!-- split-spec links -->";
const ipcFeatures = {
  F1: ["get_window_state", "log_window_error", "get_layout_state", "window://state", "layout://state", "get_update_status", "update://status"],
  F2: ["list_projects", "get_new_project_defaults", "open_project", "project://selected"],
  F3: ["get_launch_eligibility"],
  F4: ["list_threads", "get_model_menu", "thread://status"],
  F5: ["set_worktree_settings", "worktree://created", "worktree://inventory"],
  F6: ["list_pull_requests", "get_pull_request_panel", "pr://opened", "pr://panel"],
  F7: ["get_duty_dialog_defaults", "duty://event"],
  F8: ["get_continuous_dispatch", "add_task"],
  F9: ["get_issue_panel", "issue://panel"],
  F10: ["get_swarm_dialog"],
  F11: ["read_agents_md"],
  F12: ["get_thread_menu"],
  F13: ["get_settings"],
  F14: ["set_airc_message_mode", "airc_send", "get_airc_modal", "airc_events", "airc://modal", "airc://event"],
  F15: ["list_providers", "set_provider_command", "provider://status"],
  F16: ["get_work_pool", "get_goal_swarm_dialog", "launch_goal_swarm", "get_goal_swarm_board"],
  F17: ["get_proofs_settings"],
  F18: ["get_skill_settings", "get_skill_library", "generate_design_system"],
  F19: ["get_code_graph"],
  "3.2": ["edit_shared_mcp_servers"],
};

function slug(title) {
  return title.toLowerCase().replaceAll(/[^a-z0-9]+/g, "-").replaceAll(/^-|-$/g, "");
}

function label(line) {
  const bold = line.match(/\*\*(.*?)\*\*/)?.[1];
  return (bold ?? line.replace(/^\s*- (?:\[[ x]\] )?/, "").split(/(?<=[.!?:]) /)[0])
    .replaceAll(/[*`]/g, "").replace(/[.:]+$/, "");
}

function ruleSlug(title) {
  // Stable, short paths matter on Windows, and labels should not change when
  // another issue number is added to the rule's heading.
  const path = title.split(" · ").map((part) =>
    slug(part.replaceAll(/\([^)]*\)/g, "")).split("-").slice(0, 6).join("-")).join("-");
  if (path.length <= 100) return path;
  const suffix = createHash("sha256").update(path).digest("hex").slice(0, 8);
  return `${path.slice(0, 36)}-${path.slice(-48)}-${suffix}`;
}

// Only prose is wrapped. Code spans, links, fences, tables and decimal numbers
// retain their contents. List continuation lines retain the item's indentation.
function sentenceLines(line) {
  const protectedText = line.replaceAll(/(`+)[\s\S]*?\1|\[[^\]]*\]\([^)]*\)/g,
    (part) => part.replaceAll(" ", "\u0000"));
  const item = line.match(/^(\s*)(?:- (?:\[[ x]\] )?|\d+\. )/);
  const continuation = item ? " ".repeat(item[0].length) : line.match(/^\s*/)[0];
  return protectedText.replaceAll(/([.!?](?:[*”"')\]]*)) +(?=\S)/g, (match, end, offset) => {
    const before = protectedText.slice(0, offset + 1);
    return /(?:\be\.g|\bi\.e|\betc)\.$/i.test(before) ? match : `${end}\n${continuation}`;
  }).replaceAll("\u0000", " ");
}

function reflow(text) {
  let fence = null;
  return text.split("\n").map((line) => {
    const marker = line.match(/^\s*(`{3,}|~{3,})/);
    if (marker) {
      if (!fence) fence = marker[1];
      else if (marker[1][0] === fence[0] && marker[1].length >= fence.length) fence = null;
      return line;
    }
    if (fence || /^\s*(?:\||#|<!--)/.test(line)) return line;
    return sentenceLines(line);
  }).join("\n");
}

function sourceBlock(order, text, kind = "markdown", indentation = 0) {
  const metadata = JSON.stringify({ order, kind, indentation });
  const dedented = text.split("\n").map((line) => line.slice(Math.min(indentation, line.match(/^ */)[0].length))).join("\n");
  return `<!-- split-spec: ${metadata} -->\n${reflow(dedented).trim()}\n`;
}

function addFile(files, path, order, text, title = "", indentation = 0) {
  if (files.has(path)) throw new Error(`Two sections map to ${path}; give the new rule a distinct label.`);
  const heading = title ? `#### ${title}\n\n` : "";
  files.set(path, heading + sourceBlock(order, text, "markdown", indentation));
}

function addNavigation(files, parent, children) {
  const links = children.map(({ path, title }) => `- [${title}](${basename(path)})`).join("\n");
  files.set(parent, files.get(parent) + `\n${navigationMarker}\n\nRule files:\n\n${links}\n`);
}

function splitRules(files, folder, lines, start, end) {
  const boundaries = [start];
  for (let index = start + 1; index < end; index++) {
    const line = lines[index];
    // F5's opening Rosen paragraph is the section intro, kept in its README.
    if (/^\*\*Rosen first/.test(line)) continue;
    if (/^\*\*/.test(line) || /^\s*- /.test(line)) boundaries.push(index);
  }
  boundaries.push(end);
  let context = folder.includes("F5-") ? "Rosen" : "";
  const parents = [];
  const children = new Map();
  let contextPath = `${folder}/README.md`;
  for (let position = 0; position < boundaries.length - 1; position++) {
    const from = boundaries[position];
    const line = lines[from];
    const text = lines.slice(from, boundaries[position + 1]).join("\n");
    if (position === 0) {
      addFile(files, `${folder}/README.md`, from, text);
      continue;
    }
    const indentation = line.match(/^ */)[0].length;
    if (/^\*\*/.test(line)) {
      context = label(line);
      parents.length = 0;
    } else {
      while (parents.length && parents.at(-1).indentation >= indentation) parents.pop();
    }
    const title = [...new Set([context, ...parents.map((parent) => parent.title), label(line)])].filter(Boolean).join(" · ");
    const path = `${folder}/${ruleSlug(title)}.md`;
    const parent = /^\*\*/.test(line) ? `${folder}/README.md` : parents.at(-1)?.path ?? contextPath;
    if (!children.has(parent)) children.set(parent, []);
    children.get(parent).push({ path, title: label(line) });
    addFile(files, path, from, text, title, indentation);
    if (/^\*\*/.test(line)) contextPath = path;
    if (/^\s*- /.test(line)) parents.push({ indentation, title: label(line), path });
  }
  for (const [parent, entries] of children) addNavigation(files, parent, entries);
}

function ipcEntries(names) {
  // Commas inside argument braces or backticks belong to the signature. Keep
  // separators on the entries, so --join recovers the Name cell without guessing.
  return names.split(/(?<=`), (?=`)/).map((entry, index, all) =>
    `  - ${entry}${index < all.length - 1 ? "," : ""}`);
}

function ipcFeature(names, purpose) {
  if (names.startsWith("`dispatch_layout` with")) return names.includes("ToggleSettled") ? "F1" : "F3";
  const name = names.match(/^`([^` {]+)/)?.[1];
  const known = Object.entries(ipcFeatures).find(([, entries]) => entries.includes(name));
  const feature = known?.[0] ?? purpose.match(/\bF\d+\b/)?.[0];
  if (!feature) throw new Error(`Assign the new IPC row to a feature in ipcFeatures: ${names}`);
  return feature;
}

function splitIpc(files, lines, start, end) {
  const rows = lines.slice(start, end).map((line, offset) => ({ line, order: start + offset }))
    .filter(({ line }) => /^\| (?:command|event) \|/.test(line));
  const folder = "spec/3.1-ipc";
  addFile(files, `${folder}/README.md`, start, lines[start]);
  for (const { line, order } of rows) {
    const [, kind, names, purpose] = line.match(/^\| (command|event) \| (.*?) \| (.*?) \|$/) ?? [];
    if (!kind) throw new Error(`Cannot split IPC row at line ${order + 1}`);
    const feature = ipcFeature(names, purpose);
    const path = `${folder}/${feature}.md`;
    const entry = `${rowMarker}\n- **${kind}**\n${ipcEntries(names).join("\n")}\n\n${reflow(purpose)}\n`;
    const block = sourceBlock(order, entry, "ipc");
    files.set(path, (files.get(path) ?? `#### §3.1 · ${feature} IPC\n\n`) + block + "\n");
  }
  const features = [...files.keys()].filter((path) => path.startsWith(folder + "/") && !path.endsWith("README.md"));
  addNavigation(files, `${folder}/README.md`, features.map((path) => ({ path, title: basename(path, ".md") })));
}

function sectionPath(heading) {
  const feature = heading.match(/^### (F\d+) · (.*)/);
  if (feature) return `spec/${feature[1]}-${slug(feature[2])}.md`;
  const section = heading.match(/^#{2,3} ([\d.]+) (.*)/);
  if (!section) throw new Error(`Unrecognised section: ${heading}`);
  return `spec/${section[1].replace(/\.$/, "")}-${slug(section[2])}.md`;
}

function splitSpec(original) {
  const lines = original.replaceAll("\r\n", "\n").split("\n");
  const starts = lines.flatMap((line, index) => /^## 3\. |^## [4-8]\. |^### 3\.[123] |^### F\d+ · /.test(line) ? [index] : []);
  if (!starts.length || !/^## 3\. Architecture$/.test(lines[starts[0]])) {
    throw new Error("Expected a monolithic Velocity SPEC.md, with §1–§8 and F1–F19.");
  }
  const files = new Map();
  starts.push(lines.length);
  for (let index = 0; index < starts.length - 1; index++) {
    const start = starts[index];
    const end = starts[index + 1];
    const heading = lines[start];
    if (/^### F[57] · /.test(heading)) {
      splitRules(files, sectionPath(heading).replace(/\.md$/, ""), lines, start, end);
    } else if (/^### 3\.1 /.test(heading)) {
      splitIpc(files, lines, start, end);
    } else addFile(files, sectionPath(heading), start, lines.slice(start, end).join("\n"));
  }
  const prefix = lines.slice(0, starts[0]).join("\n").trimEnd();
  const index = [...files.keys()].map((path) => `- [${path}](${path})`).join("\n");
  const guidance = "Velocity reads this file and Markdown recursively under `spec/` as one spec.\nSection IDs (including F5, F7 and §3.1) are unchanged.\n§7 is in `spec/7-implementation-order.md`; §8 is in `spec/8-open-questions.md`.\nF5, F7 and §3.1 have a README for their introduction and separate files for their rules or each feature's IPC.\n";
  files.set("SPEC.md", `${prefix}\n\n${indexMarker}\n\n## Spec file index\n\n${guidance}\n${index}\n`);
  return files;
}

function recoverIpc(body) {
  const [names, purpose] = body.trim().split(/\n\n/, 2);
  const lines = names.split("\n");
  const kind = lines[1].match(/^- \*\*(command|event)\*\*$/)?.[1];
  if (!kind || lines[0] !== rowMarker) throw new Error("Unrecognised IPC list block");
  return `| ${kind} | ${lines.slice(2).map((line) => line.replace(/^  - /, "")).join(" ")} | ${purpose.replaceAll(/\s*\n\s*/g, " ")} |`;
}

function joinSpec(files) {
  const root = files.get("SPEC.md").split(indexMarker)[0].trimEnd();
  const blocks = [];
  for (const [path, text] of files) {
    if (path === "SPEC.md") continue;
    const parts = text.split(navigationMarker)[0].split(sourceMarker);
    for (let index = 1; index < parts.length; index += 2) {
      const { order, kind, indentation } = JSON.parse(parts[index]);
      const body = parts[index + 1].trimEnd();
      const restored = kind === "ipc" ? recoverIpc(body) : body.split("\n").map((line) => line ? " ".repeat(indentation) + line : line).join("\n");
      blocks.push({ order, kind, text: restored });
    }
  }
  blocks.sort((left, right) => left.order - right.order);
  let previousKind = "markdown";
  return root + "\n\n" + blocks.map((block) => {
    const header = block.kind === "ipc" && previousKind !== "ipc" ? "| Kind | Name | Purpose |\n|---|---|---|\n" : "";
    const separator = block.kind === "ipc" && previousKind === "ipc" ? "\n" : "\n\n";
    previousKind = block.kind;
    return separator + header + block.text;
  }).join("") + "\n";
}

function readSplit(root, folder = "spec") {
  const files = new Map([["SPEC.md", readFileSync(join(root, "SPEC.md"), "utf8")]]);
  function visit(relative) {
    for (const entry of readdirSync(join(root, relative), { withFileTypes: true })) {
      const path = `${relative}/${entry.name}`;
      if (entry.isDirectory()) visit(path);
      else if (entry.name.endsWith(".md")) files.set(path, readFileSync(join(root, path), "utf8"));
    }
  }
  visit(folder);
  return files;
}

function main() {
  const args = process.argv.slice(2);
  const joining = args[0] === "--join";
  if (joining) args.shift();
  if (args.length > 1 || args[0]?.startsWith("--")) throw new Error("Usage: node scripts/split-spec.mjs [--join] [checkout]");
  const root = resolve(args[0] ?? ".");
  if (joining) return process.stdout.write(joinSpec(readSplit(root)));
  const original = readFileSync(join(root, "SPEC.md"), "utf8");
  if (original.includes(indexMarker)) throw new Error("This checkout is already split; replay on a monolithic base instead.");
  const files = splitSpec(original);
  // Refuse to replace the monolith unless the in-memory inverse preserves all
  // source words, punctuation, headings and checkboxes in their original order.
  const words = (text) => text.replaceAll(/\s+/g, " ").trim();
  if (words(joinSpec(files)) !== words(original)) throw new Error("The move would change spec text; no files were written.");
  for (const path of files.keys()) {
    if (path !== "SPEC.md" && existsSync(join(root, path))) throw new Error(`Refusing to overwrite ${path}`);
  }
  for (const [path, text] of files) {
    mkdirSync(dirname(join(root, path)), { recursive: true });
    writeFileSync(join(root, path), text);
  }
  console.log(`Split SPEC.md into ${files.size - 1} files; original text preserved apart from whitespace and IPC list markup.`);
}

main();
