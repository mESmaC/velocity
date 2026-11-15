// Inputs and dependency closure for Rosen's fast checks (#387). A successful stamp
// belongs to this folder, never to a moving base or an agent's declared path list.
import { createHash } from "node:crypto";
import { existsSync, readdirSync, readFileSync, realpathSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";

export function digest(value) {
  return createHash("sha256").update(value).digest("hex");
}

const outputs = new Set([
  "ui/node_modules",
  "ui/dist",
  "src-tauri/gen",
  "proofs/.lake",
  "proofs/.lake-wsl",
]);

function filesUnder(folder, root) {
  if (!existsSync(folder)) return [];
  return readdirSync(folder, { withFileTypes: true }).flatMap((entry) => {
    const path = join(folder, entry.name);
    if (entry.isDirectory()) {
      const key = relative(root, path).replaceAll("\\", "/");
      return entry.name === "target" || outputs.has(key)
        ? []
        : filesUnder(path, root);
    }
    return [path];
  });
}

export function inputs(root, metadata) {
  const members = new Set(metadata.workspace_members);
  const packages = metadata.packages.filter((item) => members.has(item.id));
  const paths = new Set([
    ...packages.flatMap((item) =>
      filesUnder(dirname(item.manifest_path), root),
    ),
    ...["ui", "proofs/fixtures", ".cargo", "scripts"].flatMap((path) =>
      filesUnder(join(root, path), root),
    ),
    ...[
      "Cargo.toml",
      "Cargo.lock",
      "rust-toolchain.toml",
      "rustfmt.toml",
      "clippy.toml",
      "rosen.toml",
    ]
      .map((path) => join(root, path))
      .filter(existsSync),
  ]);
  // include_str!/include_bytes! can reach outside a crate (theme tokens and proof
  // fixtures do). Track those bytes against the crate that actually embeds them.
  const embedded = {};
  for (const path of paths) {
    if (!path.endsWith(".rs")) continue;
    for (const match of readFileSync(path, "utf8").matchAll(
      /include(?:_(?:str|bytes))?!\s*\(\s*"([^"]+)"/g,
    )) {
      const included = resolve(dirname(path), match[1]);
      paths.add(included);
      (embedded[relative(root, included).replaceAll("\\", "/")] ??= []).push(
        path,
      );
    }
  }
  const hashes = Object.fromEntries(
    [...paths]
      .sort()
      .map((path) => [
        relative(root, path).replaceAll("\\", "/"),
        existsSync(path) ? digest(readFileSync(path)) : "missing",
      ]),
  );
  return { packages, embedded, hashes };
}

function folderKey(path) {
  const key = resolve(path).replaceAll("\\", "/");
  return process.platform === "win32" ? key.toLowerCase() : key;
}

export function affected(root, current, previous) {
  const changed = Object.keys({ ...previous, ...current.hashes }).filter(
    (path) => previous[path] !== current.hashes[path],
  );
  const byFolder = new Map(
    current.packages.map((item) => [
      folderKey(dirname(item.manifest_path)),
      item.name,
    ]),
  );
  const selected = new Set();
  const owner = (path) =>
    current.packages.find((item) => {
      const folder =
        relative(root, dirname(item.manifest_path)).replaceAll("\\", "/") + "/";
      return path.startsWith(folder);
    })?.name;
  for (const path of changed) {
    const name = owner(path);
    if (name) selected.add(name);
    for (const source of current.embedded[path] ?? []) {
      const embeddedOwner = owner(relative(root, source).replaceAll("\\", "/"));
      if (embeddedOwner) selected.add(embeddedOwner);
    }
    // Workspace manifests, checker code and unknown Cargo configuration can
    // change resolution or what a pass means. Never narrow those checks.
    if (
      /^(Cargo\.(toml|lock)|rust-toolchain.toml|rustfmt.toml|clippy.toml|rosen.toml)$/.test(
        path,
      ) ||
      path.startsWith(".cargo/") ||
      path.startsWith("scripts/")
    ) {
      current.packages.forEach((item) => selected.add(item.name));
    }
  }
  let grew = true;
  while (grew) {
    grew = false;
    for (const item of current.packages) {
      const reaches = item.dependencies.some(
        (dependency) =>
          dependency.path &&
          selected.has(byFolder.get(folderKey(dependency.path))),
      );
      if (reaches && !selected.has(item.name)) {
        selected.add(item.name);
        grew = true;
      }
    }
  }
  return { changed, packages: [...selected].sort() };
}

export function bindingsHash(root, current) {
  // Conservative IPC inputs: type-bearing modules, command modules and the
  // registration/export code. Ordinary implementation-only modules don't export
  // bindings. Include manifests and the toolchain for feature/derive changes.
  const entries = [];
  const sources = new Map(
    Object.keys(current.hashes)
      .filter((path) => path.endsWith(".rs"))
      .map((path) => [path, readFileSync(join(root, path), "utf8")]),
  );
  const buildScripts = new Set(
    (current.packages ?? []).flatMap((item) =>
      item.targets
        .filter((target) => target.kind.includes("custom-build"))
        .map((target) => relative(root, target.src_path).replaceAll("\\", "/")),
    ),
  );
  // A hand-written Type implementation can call arbitrary schema helpers.
  const computedSchema = [...sources].some(
    ([path, source]) =>
      computesSchema(source) ||
      (buildScripts.has(path) &&
        rustTokens(source).join("") !== "fnmain(){tauri_build::build();}"),
  );
  if (computedSchema) return digest(JSON.stringify(current.hashes));
  const interfaces = new Map();
  for (const [path, hash] of Object.entries(current.hashes)) {
    if (
      /Cargo\.(toml|lock)$|rust-toolchain.toml$/.test(path) ||
      path === "ui/src/lib/bindings.ts" ||
      path.startsWith(".cargo/")
    )
      entries.push([path, hash]);
    else if (path.endsWith(".rs")) {
      const source = sources.get(path);
      const projected = interfaceSource(path, source, buildScripts.has(path));
      if (projected) {
        interfaces.set(path, projected.text);
        entries.push([
          path,
          projected.complete ? hash : digest(projected.text),
        ]);
      }
    }
  }
  entries.push(...embeddedInterfaceInputs(root, current, sources, interfaces));
  return digest(JSON.stringify(entries));
}

function interfaceSource(path, source, buildScript) {
  // Registration/export functions can compute schema constants.
  const complete =
    buildScript ||
    path === "src-tauri/src/lib.rs" ||
    path === "src-tauri/src/bin/export-bindings.rs";
  if (
    !complete &&
    !/specta|\bType\b|\b(?:type|const|static)\b|tauri\s*::\s*command/.test(
      source,
    )
  )
    return null;
  return { text: complete ? source : rustInterface(source), complete };
}

function embeddedInterfaceInputs(root, current, sources, interfaces) {
  const entries = [];
  for (const [path, owners] of Object.entries(current.embedded ?? {})) {
    const retained = owners.some((owner) => {
      const key = relative(root, owner).replaceAll("\\", "/");
      const projected = interfaces.get(key);
      if (!projected) return false;
      return [
        ...sources
          .get(key)
          .matchAll(/include(?:_(?:str|bytes))?!\s*\(\s*"([^"]+)"/g),
      ].some(
        (match) =>
          relative(root, resolve(dirname(owner), match[1])).replaceAll(
            "\\",
            "/",
          ) === path && projected.includes(`"${match[1]}"`),
      );
    });
    if (retained) entries.push([path, current.hashes[path]]);
  }
  return entries;
}

// This is a conservative lexical projection, not a Rust parser. Unknown or
// unbalanced input falls back to the whole source. Keep macro definitions and
// manual trait implementations verbatim: they can compute a specta schema.
export function rustInterface(source) {
  try {
    const tokens = rustTokens(source);
    if (tokens.some((token) => token === "macro")) return source;
    return projectFunctions(tokens, matchingGroups(tokens)).join("\0");
  } catch {
    return source;
  }
}

function computesSchema(source) {
  try {
    const tokens = rustTokens(source);
    const text = declarationText(tokens).join(" ");
    const implementations = [
      ...text.matchAll(/\bimpl\b[^{};]*\bType\b[^{};]*\bfor\b/g),
    ].length;
    const definitions = implementations
      ? [...text.matchAll(/\bfn\s+definition\s*\(/g)].length
      : 0;
    // The store's ProviderKind delegates to a derived wire type. Such a
    // delegate has no arbitrary helpers; keep its implementation and target
    // declaration in the projection. Other manual schemas need all Rust.
    const compact = text.replaceAll(/\s+/g, "");
    const delegates = [
      ...compact.matchAll(
        /impl(?:specta::)?Typefor\w+\{fndefinition\((\w+):&mutspecta::Types\)->specta::datatype::DataType\{((?:\w+::)+)definition\(\1\)\}\}/g,
      ),
    ].filter((match) => derivedLocalTarget(tokens, match[2])).length;
    return (
      implementations !== delegates ||
      definitions !== delegates ||
      /\buse\s+specta\b[^;]*\bType\s+as\b/.test(text) ||
      (/macro_rules\s*!/.test(text) && /specta/.test(text))
    );
  } catch {
    return true;
  }
}

function declarationText(tokens) {
  return tokens.map((token) =>
    /^(?:"|\/|(?:b|c)?r#*")/.test(token) ? "<literal>" : token,
  );
}

function derivedLocalTarget(tokens, target) {
  const path = target.split("::").filter(Boolean);
  if (path.length !== 2) return false;
  const groups = matchingGroups(tokens);
  for (let index = 0; index < tokens.length; index++) {
    if (
      tokens[index] === "mod" &&
      tokens[index + 1] === path[0] &&
      tokens[index + 2] === "{"
    ) {
      const body = tokens.slice(index + 3, groups.get(index + 2));
      if (
        body.some((token) =>
          ["impl", "fn", "mod", "macro_rules"].includes(token),
        )
      )
        return false;
      const text = declarationText(body).join("");
      const declarations = [
        ...text.matchAll(
          /#\[derive\(([^)]*)\)\](?:#\[[^\]]*\])*pub(?:\([^)]*\))?(?:struct|enum)(\w+)/g,
        ),
      ];
      return declarations.some(
        (match) => /\bType\b/.test(match[1]) && match[2] === path[1],
      );
    }
    if (tokens[index] === "{") index = groups.get(index);
  }
  return false;
}

function quotedEnd(source, start, quote) {
  for (let index = start + 1; index < source.length; index++) {
    if (source[index] === "\\") index++;
    else if (source[index] === quote) return index + 1;
  }
  throw new Error("Unclosed Rust string");
}

function commentEnd(source, start) {
  let depth = 1;
  let index = start + 2;
  while (depth && index < source.length) {
    if (source.slice(index, index + 2) === "/*") {
      depth++;
      index += 2;
    } else if (source.slice(index, index + 2) === "*/") {
      depth--;
      index += 2;
    } else index++;
  }
  if (depth) throw new Error("Unclosed Rust comment");
  return index;
}

function rustTokens(source) {
  const tokens = [];
  for (let index = 0; index < source.length;) {
    const rest = source.slice(index);
    let length;
    if (/^\s/.test(rest)) {
      index++;
      continue;
    }
    if (rest.startsWith("//")) {
      length = rest.indexOf("\n");
      if (length < 0) length = rest.length;
      if (!/^\/\/[!/]/.test(rest)) {
        index += length;
        continue;
      }
    } else if (rest.startsWith("/*")) {
      length = commentEnd(source, index) - index;
      if (!/^\/\*[!*]/.test(rest)) {
        index += length;
        continue;
      }
    } else {
      const raw = rest.match(/^(?:b|c)?r(#{0,255})"/);
      if (raw) {
        const end = source.indexOf(`"${raw[1]}`, index + raw[0].length);
        if (end < 0) throw new Error("Unclosed raw Rust string");
        length = end + 1 + raw[1].length - index;
      } else if (rest[0] === '"')
        length = quotedEnd(source, index, '"') - index;
      else {
        const token = rest.match(
          /^'(?:\\(?:u\{[\da-fA-F_]+\}|x[\da-fA-F]{2}|[^])|[^'\\])'|^(?:r#)?[\p{ID_Start}_][\p{ID_Continue}_]*|^(?:->|=>)/u,
        );
        length = token ? token[0].length : 1;
      }
    }
    tokens.push(source.slice(index, index + length));
    index += length;
  }
  return tokens;
}

function matchingGroups(tokens) {
  const closes = new Map();
  const stack = [];
  const pairs = { "(": ")", "[": "]", "{": "}" };
  for (const [index, token] of tokens.entries()) {
    if (Object.hasOwn(pairs, token)) stack.push([index, pairs[token]]);
    else if ([")", "]", "}"].includes(token)) {
      const opened = stack.pop();
      if (!opened || opened[1] !== token)
        throw new Error("Unbalanced Rust groups");
      closes.set(opened[0], index);
    }
  }
  if (stack.length) throw new Error("Unclosed Rust group");
  return closes;
}

function functionEnd(tokens, groups, start) {
  if (
    !/^(?:r#)?[\p{ID_Start}_][\p{ID_Continue}_]*$/u.test(
      tokens[start + 1] ?? "",
    )
  )
    return null;
  let angles = 0;
  for (let index = start + 2; index < tokens.length; index++) {
    const token = tokens[index];
    if (token === "<") angles++;
    else if (token === ">") angles = Math.max(0, angles - 1);
    else if (groups.has(index)) {
      if (token === "{" && angles === 0)
        return { body: index, end: groups.get(index) };
      index = groups.get(index);
    } else if (token === ";" && angles === 0)
      return { body: index, end: index };
  }
  throw new Error("Unclosed Rust function");
}

const functionAttributes = new Set([
  "cfg",
  "cfg_attr",
  "allow",
  "warn",
  "deny",
  "forbid",
  "expect",
  "inline",
  "doc",
  "cold",
  "must_use",
  "track_caller",
  "test",
  "deprecated",
  "tauri",
  "specta",
]);

function projectFunction(tokens, groups, index, scope) {
  const end = functionEnd(tokens, groups, index);
  if (!end) return null;
  const prefix = tokens.slice(scope.start, index).join(" ");
  const command = /\b(?:specta|command)\b/.test(prefix);
  const constant = /\bconst\b/.test(prefix);
  const opaque = [...prefix.matchAll(/#\s*\[\s*(\w+)/g)].some(
    (match) => !functionAttributes.has(match[1]),
  );
  const keptEnd = constant || opaque ? end.end + 1 : end.body;
  return {
    end: end.end,
    kept:
      command || constant || opaque ? tokens.slice(scope.start, keptEnd) : [],
  };
}

function projectFunctions(tokens, groups) {
  const output = [];
  const scopes = [{ start: 0, output: 0, trait: false }];
  for (let index = 0; index < tokens.length; index++) {
    const scope = scopes.at(-1);
    const token = tokens[index];
    if (token === "fn" && !scope.trait) {
      const projected = projectFunction(tokens, groups, index, scope);
      if (projected) {
        output.splice(scope.output);
        output.push(...projected.kept);
        index = projected.end;
        scope.start = index + 1;
        scope.output = output.length;
        continue;
      }
    }
    output.push(token);
    if (
      token === "macro_rules" &&
      tokens[index + 1] === "!" &&
      groups.has(index + 3)
    ) {
      const end = groups.get(index + 3);
      output.push(...tokens.slice(index + 1, end + 1));
      index = end;
      scope.start = index + 1;
      scope.output = output.length;
    } else if (token === "!" && groups.has(index + 1)) {
      const end = groups.get(index + 1);
      output.push(...tokens.slice(index + 1, end + 1));
      index = end;
    } else if (["(", "["].includes(token)) {
      const end = groups.get(index);
      output.push(...tokens.slice(index + 1, end + 1));
      index = end;
    } else if (token === "{") {
      const header = tokens.slice(scope.start, index);
      const trait =
        scope.trait ||
        (header.includes("impl") && header.includes("for")) ||
        header.includes("trait");
      scopes.push({ start: index + 1, output: output.length, trait });
    } else if (token === "}") {
      if (scopes.length === 1) throw new Error("Unexpected Rust close");
      scopes.pop();
      scopes.at(-1).start = index + 1;
      scopes.at(-1).output = output.length;
    } else if (token === ";") {
      scope.start = index + 1;
      scope.output = output.length;
    }
  }
  return output;
}

export function physicalRoot(url) {
  // pnpm resolves aliases/junctions before running cargo; use that same identity.
  return realpathSync.native(resolve(dirname(url), ".."));
}
