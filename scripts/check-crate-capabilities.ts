#!/usr/bin/env node

/**
 * Capability boundaries per workspace crate: manifests, each member's own
 * resolved dependency tree, a lexical scan of the crate's Rust sources, and,
 * for the three crates the MongoDB driver's Tokio `process` feature reaches
 * (`compresso-sql`, `compresso-mcp`, `compresso-cli`), a semantic control.
 *
 * The semantic control (D117) is Clippy's `disallowed_types` and
 * `disallowed_methods`, configured per crate in `clippy.toml` and enforced
 * by CI's `cargo clippy --workspace --all-targets -- -D warnings`. Clippy
 * resolves paths after name resolution, so aliases, globs, nested groups,
 * leading `::`, `extern crate` renames, local macros, `#[path]` modules,
 * extra binaries, and integration tests do not evade it (each shown failing
 * in a scratch crate). This script checks that the configuration and every
 * entry are present, that no attribute or manifest lowers those lints, and
 * that no `#[path]`, build script, or target outside the scanned folders
 * exists. Residual risk: Clippy does not lint code expanded from another
 * crate's macro; a `CLIPPY_CONF_DIR` or `RUSTFLAGS` lint override in the CI
 * environment would weaken it; and the lint-level check here is lexical.
 *
 * The lexical scan is the only source control for every other crate and is
 * a secondary one for those three. It reads text, not resolved names: it
 * blanks comments, strings, and character literals; refuses direct paths,
 * groups (nested too), leading `::`, aliases, glob imports, macro-built
 * paths under `std`, `tokio`, `async_std`, and `smol`, a macro given a root
 * and a member as neighbouring tokens, `include!` outside the crate's
 * sources, and `<dependency>::process::<member>` paths. It cannot see a
 * dependency's re-export under another name (`pub use std::process as p;`
 * then `p::Command`), a macro that assembles the path some other way,
 * effects inside a dependency's own functions, or generated code; for crates
 * other than the three above those remain unchecked beyond the
 * dependency-level checks (which packages and features each member's tree
 * may contain).
 */

import { execFile } from "node:child_process";
import type { Dirent } from "node:fs";
import {
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import {
  basename,
  dirname,
  isAbsolute,
  join,
  relative,
  resolve,
  sep,
} from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";

type Capability = "network" | "process";

type CapabilityClass = Record<Capability, boolean>;

type CargoDependency = {
  name: string;
  rename: string | null;
  kind: "dev" | "build" | null;
  optional: boolean;
  features: string[];
  target: string | null;
};

type CargoTarget = {
  name: string;
  kind: string[];
  "required-features": string[];
  src_path: string;
};

type CargoPackage = {
  id: string;
  name: string;
  version: string;
  source?: string | null;
  manifest_path: string;
  dependencies: CargoDependency[];
  features: Record<string, string[]>;
  targets: CargoTarget[];
};

type CargoResolveNode = {
  id: string;
  features: string[];
  deps: Array<{ pkg: string }>;
};

type CargoMetadata = {
  packages: CargoPackage[];
  workspace_members: string[];
  resolve: { nodes: CargoResolveNode[] } | null;
};

export type CheckResult = {
  ok: boolean;
  checkedCrates: number;
  violations: string[];
};

const execFileAsync = promisify(execFile);

const capabilityClasses = {
  "pure-local": { network: false, process: false },
  "bounded-local": { network: false, process: false },
  "provider-network": { network: true, process: false },
  supervisor: { network: true, process: true },
  updater: { network: true, process: true },
  "database-network": { network: true, process: false },
  sandbox: { network: true, process: true },
  delegated: { network: false, process: false },
  "desktop-webview": { network: false, process: false },
} satisfies Record<string, CapabilityClass>;

type CapabilityClassName = keyof typeof capabilityClasses;

const crateClasses = new Map<string, CapabilityClassName>([
  ["compresso-core", "pure-local"],
  ["compresso-atlas", "pure-local"],
  ["compresso-store", "pure-local"],
  ["compresso-mcp", "pure-local"],
  ["compresso-search", "pure-local"],
  ["compresso-edit", "pure-local"],
  ["compresso-recall", "pure-local"],
  ["compresso-knowledge", "pure-local"],
  ["compresso-review", "pure-local"],
  ["compresso-router", "pure-local"],
  ["compresso-model-protocol", "pure-local"],
  ["compresso-integrations", "bounded-local"],
  ["compresso-cli", "bounded-local"],
  ["compresso-eval", "bounded-local"],
  ["compresso-proxy", "provider-network"],
  ["compresso-providers", "provider-network"],
  ["compresso-agent", "supervisor"],
  ["compresso-updater", "updater"],
  ["compresso-sql", "database-network"],
  ["compresso-sandbox", "sandbox"],
  ["compresso-browser", "delegated"],
  ["compresso-terminal", "delegated"],
  ["compresso-desktop", "desktop-webview"],
]);

const networkDependencies = new Set([
  "async-tungstenite",
  "awc",
  "curl",
  "hyper",
  "hyper-util",
  "isahc",
  "quinn",
  "reqwest",
  "socket2",
  "surf",
  "tokio-tungstenite",
  "tonic",
  "tungstenite",
  "ureq",
]);

const processDependencies = new Set([
  "async-process",
  "command-group",
  "duct",
  "process-wrap",
  "shared_child",
  "subprocess",
]);

const sourceCapabilityPaths: Array<{
  capability: Capability;
  root: string;
  member: string;
}> = [
  { capability: "network", root: "std", member: "net" },
  { capability: "network", root: "tokio", member: "net" },
  { capability: "network", root: "async_std", member: "net" },
  { capability: "network", root: "smol", member: "net" },
  { capability: "process", root: "tokio", member: "process" },
  { capability: "process", root: "async_process", member: "Command" },
];

const inertStdProcessMembers = new Set(["ExitCode", "id"]);

function regexEscape(value: string) {
  return value.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
}

type ProcessPrefix = {
  path: string;
  origin: string | null;
};

function parseGroupedUseEntries(body: string) {
  const entries: Array<{ name: string; alias: string | null }> = [];
  for (const item of body.split(",")) {
    const parsed = item
      .trim()
      .match(
        /^(self|[A-Za-z_][A-Za-z0-9_]*)(?:\s+as\s+([A-Za-z_][A-Za-z0-9_]*|_))?/u,
      );
    if (parsed) {
      entries.push({
        name: parsed[1],
        alias: parsed[2] && parsed[2] !== "_" ? parsed[2] : null,
      });
    }
  }
  return entries;
}

function groupedUseEntries(source: string, prefix: string) {
  const entries: Array<{ name: string; alias: string | null }> = [];
  const pattern = new RegExp(
    `\\buse\\s+${regexEscape(prefix)}\\s*::\\s*\\{([^}]*)\\}`,
    "gu",
  );
  for (const match of source.matchAll(pattern)) {
    entries.push(...parseGroupedUseEntries(match[1]));
  }
  return entries;
}

function stdProcessCapabilityUses(source: string) {
  const uses: Array<{ capability: Capability; label: string }> = [];
  const rootPrefixes = new Map<string, ProcessPrefix>([
    ["std", { path: "std", origin: null }],
  ]);

  for (const match of source.matchAll(
    /\b(?:use\s+std|extern\s+crate\s+std)\s+as\s+([A-Za-z_][A-Za-z0-9_]*)/gu,
  )) {
    rootPrefixes.set(match[1], { path: match[1], origin: "std" });
  }
  for (const entry of groupedUseEntries(source, "std")) {
    if (entry.name === "self" && entry.alias) {
      rootPrefixes.set(entry.alias, { path: entry.alias, origin: "std" });
    }
  }

  for (const root of rootPrefixes.values()) {
    const nestedUse = new RegExp(
      `\\buse\\s+${regexEscape(root.path)}\\s*::\\s*\\{([^;]*)\\}\\s*;`,
      "gu",
    );
    for (const statement of source.matchAll(nestedUse)) {
      const body = statement[1];
      if (/\bprocess\s*::\s*\*/u.test(body)) {
        const alias = root.origin ? " (alias of std)" : "";
        uses.push({
          capability: "process",
          label: `${root.path}::{process::*}${alias}`,
        });
      }
      for (const match of body.matchAll(
        /\bprocess\s*::\s*([A-Za-z_][A-Za-z0-9_]*)/gu,
      )) {
        if (!inertStdProcessMembers.has(match[1])) {
          const alias = root.origin ? " (alias of std)" : "";
          uses.push({
            capability: "process",
            label: `${root.path}::{process::${match[1]}}${alias}`,
          });
        }
      }
      for (const match of body.matchAll(/\bprocess\s*::\s*\{([^}]*)\}/gu)) {
        if (/(?:^|,)\s*\*(?:\s*,|$)/u.test(match[1])) {
          const alias = root.origin ? " (alias of std)" : "";
          uses.push({
            capability: "process",
            label: `${root.path}::{process::{*}}${alias}`,
          });
        }
        for (const entry of parseGroupedUseEntries(match[1])) {
          if (!inertStdProcessMembers.has(entry.name)) {
            const alias = root.origin ? " (alias of std)" : "";
            uses.push({
              capability: "process",
              label: `${root.path}::{process::{${entry.name}}}${alias}`,
            });
          }
        }
      }
    }
  }

  const processPrefixes = new Map<string, ProcessPrefix>();
  for (const root of rootPrefixes.values()) {
    const path = `${root.path}::process`;
    processPrefixes.set(path, {
      path,
      origin: root.origin ? "std::process" : null,
    });
    for (const entry of groupedUseEntries(source, root.path)) {
      if (entry.name === "process") {
        const alias = entry.alias ?? "process";
        processPrefixes.set(alias, {
          path: alias,
          origin: "std::process",
        });
      }
    }
  }

  let prefixCount = -1;
  while (prefixCount !== processPrefixes.size) {
    prefixCount = processPrefixes.size;
    for (const prefix of [...processPrefixes.values()]) {
      const directAlias = new RegExp(
        `\\buse\\s+${regexEscape(prefix.path)}\\s+as\\s+([A-Za-z_][A-Za-z0-9_]*)\\s*;`,
        "gu",
      );
      for (const match of source.matchAll(directAlias)) {
        processPrefixes.set(match[1], {
          path: match[1],
          origin: "std::process",
        });
      }
      const directImport = new RegExp(
        `\\buse\\s+${regexEscape(prefix.path)}\\s*;`,
        "u",
      );
      if (directImport.test(source)) {
        const localName = prefix.path.split("::").at(-1) ?? "process";
        processPrefixes.set(localName, {
          path: localName,
          origin: "std::process",
        });
      }
      for (const entry of groupedUseEntries(source, prefix.path)) {
        if (entry.name === "self") {
          const alias = entry.alias ?? "process";
          processPrefixes.set(alias, {
            path: alias,
            origin: "std::process",
          });
        }
      }
    }
  }

  for (const prefix of processPrefixes.values()) {
    const wildcardPattern = new RegExp(
      `\\b${regexEscape(prefix.path)}\\s*::\\s*\\*`,
      "u",
    );
    if (wildcardPattern.test(source)) {
      const alias = prefix.origin ? ` (alias of ${prefix.origin})` : "";
      uses.push({
        capability: "process",
        label: `${prefix.path}::*${alias}`,
      });
    }
    const memberPattern = new RegExp(
      `\\b${regexEscape(prefix.path)}\\s*::\\s*([A-Za-z_][A-Za-z0-9_]*)`,
      "gu",
    );
    for (const match of source.matchAll(memberPattern)) {
      const member = match[1];
      if (!inertStdProcessMembers.has(member)) {
        const alias = prefix.origin ? ` (alias of ${prefix.origin})` : "";
        uses.push({
          capability: "process",
          label: `${prefix.path}::${member}${alias}`,
        });
      }
    }

    for (const entry of groupedUseEntries(source, prefix.path)) {
      if (entry.name !== "self" && !inertStdProcessMembers.has(entry.name)) {
        const alias = prefix.origin ? ` (alias of ${prefix.origin})` : "";
        uses.push({
          capability: "process",
          label: `${prefix.path}::${entry.name}${alias}`,
        });
      }
    }
  }

  return uses;
}

async function cargoMetadata(manifestPath: string, resolved: boolean) {
  const args = ["metadata", "--format-version", "1"];
  if (resolved) {
    args.push("--locked");
  } else {
    args.push("--no-deps");
  }
  args.push("--manifest-path", manifestPath);

  try {
    const { stdout } = await execFileAsync("cargo", args, {
      cwd: dirname(manifestPath),
      encoding: "utf8",
      maxBuffer: 32 * 1024 * 1024,
    });
    return JSON.parse(stdout) as CargoMetadata;
  } catch (error) {
    const stderr = (error as { stderr?: string }).stderr?.trim();
    throw new Error(
      `cargo metadata failed for ${manifestPath}${stderr ? `: ${stderr}` : ""}`,
      { cause: error },
    );
  }
}

function stripRustCommentsAndStrings(source: string) {
  // Works on code points, so a character outside the Basic Multilingual
  // Plane cannot shift later positions.
  const chars = [...source];
  const output = [...chars];
  const ahead = (index: number, length: number) =>
    chars.slice(index, index + length).join("");
  let blockDepth = 0;
  let inLineComment = false;
  let inString = false;
  let escaped = false;
  let rawTerminator: string | undefined;

  for (let index = 0; index < chars.length; index += 1) {
    const character = chars[index];
    const next = chars[index + 1];

    if (inLineComment) {
      if (character === "\n") {
        inLineComment = false;
      } else {
        output[index] = " ";
      }
      continue;
    }

    if (blockDepth > 0) {
      output[index] = " ";
      if (character === "/" && next === "*") {
        blockDepth += 1;
        output[index + 1] = " ";
        index += 1;
      } else if (character === "*" && next === "/") {
        blockDepth -= 1;
        output[index + 1] = " ";
        index += 1;
      }
      continue;
    }

    if (rawTerminator) {
      output[index] = " ";
      if (ahead(index, rawTerminator.length) === rawTerminator) {
        for (let offset = 1; offset < rawTerminator.length; offset += 1) {
          output[index + offset] = " ";
        }
        index += rawTerminator.length - 1;
        rawTerminator = undefined;
      }
      continue;
    }

    if (inString) {
      output[index] = " ";
      if (character === '"' && !escaped) {
        inString = false;
      }
      escaped = character === "\\" && !escaped;
      continue;
    }

    if (character === "/" && next === "/") {
      output[index] = " ";
      output[index + 1] = " ";
      index += 1;
      inLineComment = true;
      continue;
    }
    if (character === "/" && next === "*") {
      output[index] = " ";
      output[index + 1] = " ";
      index += 1;
      blockDepth = 1;
      continue;
    }

    const previous = chars[index - 1] ?? " ";
    const startsToken = !/[A-Za-z0-9_]/u.test(previous);
    const window = ahead(index, 300);
    const rawStart = startsToken ? window.match(/^(?:br|cr|r)(#*)"/u) : null;
    if (rawStart) {
      const length = [...rawStart[0]].length;
      for (let offset = 0; offset < length; offset += 1) {
        output[index + offset] = " ";
      }
      index += length - 1;
      rawTerminator = `"${rawStart[1]}`;
      continue;
    }

    // Character and byte literals ('"', b'"', '\'', '\u{..}'), so a quote
    // inside one cannot open a string; a lifetime ('a) is left alone.
    const literal = startsToken
      ? window.match(
          /^b?'(?:\\(?:x[0-9A-Fa-f]{2}|u\{[0-9A-Fa-f]{1,6}\}|[^\n])|[^\\'\n])'/u,
        )
      : null;
    if (literal) {
      const length = [...literal[0]].length;
      for (let offset = 0; offset < length; offset += 1) {
        output[index + offset] = " ";
      }
      index += length - 1;
      continue;
    }

    if (character === '"') {
      output[index] = " ";
      inString = true;
    }
  }

  return output.join("");
}

function sourceCapabilityUses(
  source: string,
  dependencyRoots: Set<string> = new Set(),
) {
  const uses = stdProcessCapabilityUses(source);

  for (const path of sourceCapabilityPaths) {
    const root = regexEscape(path.root);
    const member = regexEscape(path.member);
    const direct = new RegExp(`\\b${root}\\s*::\\s*${member}\\b`, "u");
    if (direct.test(source)) {
      uses.push({
        capability: path.capability,
        label: `${path.root}::${path.member}`,
      });
    }

    const grouped = new RegExp(
      `\\buse\\s+${root}\\s*::\\s*\\{[^}]*\\b${member}\\b`,
      "u",
    );
    if (grouped.test(source)) {
      uses.push({
        capability: path.capability,
        label: `${path.root}::{${path.member}}`,
      });
    }

    const aliases = new Set<string>();
    for (const match of source.matchAll(
      new RegExp(
        `\\b(?:use\\s+${root}|extern\\s+crate\\s+${root})\\s+as\\s+([A-Za-z_][A-Za-z0-9_]*)`,
        "gu",
      ),
    )) {
      aliases.add(match[1]);
    }
    for (const match of source.matchAll(
      new RegExp(
        `\\buse\\s+${root}\\s*::\\s*\\{[^}]*\\bself\\s+as\\s+([A-Za-z_][A-Za-z0-9_]*)`,
        "gu",
      ),
    )) {
      aliases.add(match[1]);
    }

    for (const alias of aliases) {
      const aliased = new RegExp(
        `\\b${regexEscape(alias)}\\s*::\\s*${member}\\b`,
        "u",
      );
      if (aliased.test(source)) {
        uses.push({
          capability: path.capability,
          label: `${alias}::${path.member} (alias of ${path.root})`,
        });
      }
    }
  }

  // D117 review: forms a lexical scan cannot follow are refused outright.
  // A glob import of a capability root brings `process` or `net` into scope
  // under a bare name; a macro that builds a path under such a root can name
  // any member.
  for (const root of capabilityRoots) {
    const escaped = regexEscape(root.name);
    const glob = new RegExp(
      `\\buse\\s+(?:::\\s*)?${escaped}\\s*::\\s*(?:\\*|\\{[^}]*\\*)`,
      "u",
    );
    const macroPath = new RegExp(`\\b${escaped}\\s*::\\s*\\$`, "u");
    for (const capability of root.capabilities) {
      if (glob.test(source)) {
        uses.push({ capability, label: `${root.name}::* (glob import)` });
      }
      if (macroPath.test(source)) {
        uses.push({
          capability,
          label: `${root.name}::$... (path built by a macro)`,
        });
      }
    }
  }
  // Groups (nested ones too, up to the end of the `use` statement), a
  // leading `::`, aliases made by `use ROOT as x` or `extern crate ROOT as
  // x` and then used in a group, and a macro given both the root and the
  // member as tokens (`m!(tokio, process)`).
  const memberNames: Record<string, Capability> = {
    net: "network",
    process: "process",
  };
  for (const root of capabilityRoots) {
    const escaped = regexEscape(root.name);
    const aliases = new Set<string>([root.name]);
    for (const match of source.matchAll(
      new RegExp(
        `\\b(?:use\\s+(?:::\\s*)?${escaped}|extern\\s+crate\\s+${escaped})\\s+as\\s+([A-Za-z_][A-Za-z0-9_]*)`,
        "gu",
      ),
    )) {
      aliases.add(match[1]);
    }
    for (const match of source.matchAll(
      new RegExp(
        `\\buse\\s+(?:::\\s*)?${escaped}\\s*::\\s*\\{[^;]*\\bself\\s+as\\s+([A-Za-z_][A-Za-z0-9_]*)`,
        "gu",
      ),
    )) {
      aliases.add(match[1]);
    }
    for (const [member, capability] of Object.entries(memberNames)) {
      if (!root.capabilities.includes(capability)) {
        continue;
      }
      for (const alias of aliases) {
        const group = new RegExp(
          `\\buse\\s+(?:::\\s*)?${regexEscape(alias)}\\s*::\\s*\\{([^;]*)`,
          "gu",
        );
        for (const match of source.matchAll(group)) {
          if (
            groupReaches(
              match[1],
              member,
              root.name === "std" && member === "process",
            )
          ) {
            uses.push({
              capability,
              label: `${alias}::{..${member}..} (group of ${root.name})`,
            });
          }
        }
        if (alias !== root.name) {
          const direct = new RegExp(
            `(?:^|[^A-Za-z0-9_])(?:::\\s*)?${regexEscape(alias)}\\s*::\\s*${member}\\b`,
            "u",
          );
          if (direct.test(source)) {
            uses.push({
              capability,
              label: `${alias}::${member} (alias of ${root.name})`,
            });
          }
        }
      }
      // A macro given the root and the member as neighbouring bare tokens
      // (`m!(tokio, process)`, either order); other spellings are left to
      // Clippy (see the file header).
      const pair = new RegExp(
        `!\\s*[([{][^;]*?(?:\\b${escaped}\\s*,\\s*${member}\\b|\\b${member}\\s*,\\s*${escaped}\\b)`,
        "u",
      );
      if (pair.test(source)) {
        uses.push({
          capability,
          label: `a macro given ${root.name} and ${member}`,
        });
      }
    }
  }
  // `std::os::<platform>::process` (process extension traits).
  if (
    /\bstd\s*::\s*os\s*::\s*[A-Za-z_][A-Za-z0-9_]*\s*::\s*process\b/u.test(
      source,
    )
  ) {
    uses.push({ capability: "process", label: "std::os::<platform>::process" });
  }
  // A `process` module of a dependency (one it re-exports) counts as
  // process use unless the member is inert. Local modules named `process`
  // (`crate::process`, `jobs::process`) are not dependencies and pass.
  for (const match of source.matchAll(
    /(?:^|[^A-Za-z0-9_:])(?:::\s*)?([A-Za-z_][A-Za-z0-9_]*)\s*::\s*process\s*::\s*([A-Za-z_][A-Za-z0-9_]*)/gu,
  )) {
    if (
      match[1] !== "std" &&
      dependencyRoots.has(match[1]) &&
      !inertStdProcessMembers.has(match[2])
    ) {
      uses.push({
        capability: "process",
        label: `${match[1]}::process::${match[2]} (a process module of a dependency)`,
      });
    }
  }

  return uses;
}

/**
 * Whether the inside of a `use ROOT::{...}` group reaches `member`. For
 * `std::process` the inert members alone (`ExitCode`, `id`) do not count.
 */
function groupReaches(group: string, member: string, inertAllowed: boolean) {
  for (const match of group.matchAll(
    new RegExp(
      `\\b${member}\\b(\\s*::\\s*(?:([A-Za-z_][A-Za-z0-9_]*)|\\{([^}]*)\\}))?`,
      "gu",
    ),
  )) {
    if (!inertAllowed) {
      return true;
    }
    const single = match[2];
    const list = match[3];
    if (single && inertStdProcessMembers.has(single)) {
      continue;
    }
    if (
      list !== undefined &&
      list
        .split(",")
        .map((item) => item.trim().split(/\s+/u)[0])
        .filter((item) => item.length > 0)
        .every((item) => inertStdProcessMembers.has(item))
    ) {
      continue;
    }
    return true;
  }
  return false;
}

/**
 * The semantic control (D117 confirmation round). Clippy's
 * `disallowed_types` and `disallowed_methods` resolve paths after name
 * resolution, so aliases, globs, groups, local macros, `#[path]` modules,
 * extra binaries, and integration tests cannot evade them; CI runs Clippy
 * with `-D warnings`. Each crate that the MongoDB driver's Tokio `process`
 * feature reaches carries a `clippy.toml` naming these paths, and this
 * script checks that the file and every entry are there and that nothing
 * lowers the lint. `compresso-cli` keeps its sanctioned `std::process`
 * files (ADR-0028, ADR-0031) and tests, so only Tokio's are disallowed there.
 */
const tokioProcessPaths = {
  types: ["tokio::process::Command", "tokio::process::Child"],
  methods: ["tokio::process::Command::new"],
};
const stdProcessPaths = {
  types: ["std::process::Command", "std::process::Child"],
  methods: ["std::process::Command::new"],
};
const semanticProcessCrates = new Map<
  string,
  { types: string[]; methods: string[] }
>([
  [
    "compresso-sql",
    {
      types: [...stdProcessPaths.types, ...tokioProcessPaths.types],
      methods: [...stdProcessPaths.methods, ...tokioProcessPaths.methods],
    },
  ],
  [
    "compresso-mcp",
    {
      types: [...stdProcessPaths.types, ...tokioProcessPaths.types],
      methods: [...stdProcessPaths.methods, ...tokioProcessPaths.methods],
    },
  ],
  ["compresso-cli", tokioProcessPaths],
]);

/** Lint-level settings that would switch the disallowed lints off. */
const lintLowering =
  /\b(?:allow|expect|warn)\s*\(\s*[^)"]*\b(?:warnings|clippy::all|clippy::style|clippy::disallowed_[a-z_]+)/u;
const manifestLintLowering =
  /^\s*(?:"?(?:style|all|disallowed_[a-z_]+|warnings)"?)\s*=/mu;

async function allRustFiles(root: string) {
  const files: string[] = [];
  async function visit(directory: string) {
    let entries: Dirent[];
    try {
      entries = await readdir(directory, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      if (entry.name === "target" || entry.name === "node_modules") {
        continue;
      }
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        await visit(path);
      } else if (entry.isFile() && entry.name.endsWith(".rs")) {
        files.push(path);
      }
    }
  }
  await visit(root);
  return files.sort();
}

async function semanticControlViolations(
  pkg: CargoPackage,
  crateRoot: string,
  workspaceRoot: string,
) {
  const required = semanticProcessCrates.get(pkg.name);
  const violations: string[] = [];
  if (!required) {
    return violations;
  }
  let config: string;
  try {
    config = await readFile(join(crateRoot, "clippy.toml"), "utf8");
  } catch {
    return [
      `clippy.toml is missing; it must disallow ${[...required.types, ...required.methods].join(", ")}`,
    ];
  }
  try {
    await stat(join(crateRoot, ".clippy.toml"));
    violations.push(".clippy.toml must not exist beside clippy.toml");
  } catch {
    // Absent, as required.
  }
  const section = (name: string) => {
    const match = config.match(
      new RegExp(`^\\s*${name}\\s*=\\s*\\[([\\s\\S]*?)^\\]`, "mu"),
    );
    return match?.[1] ?? "";
  };
  for (const [name, paths] of [
    ["disallowed-types", required.types],
    ["disallowed-methods", required.methods],
  ] as const) {
    const body = section(name);
    for (const path of paths) {
      if (!body.includes(`path = "${path}"`)) {
        violations.push(`clippy.toml ${name} must list ${path}`);
      }
    }
  }
  for (const file of await allRustFiles(crateRoot)) {
    const text = await readFile(file, "utf8");
    if (lintLowering.test(text)) {
      violations.push(
        `a lint attribute that can switch off clippy::disallowed_* is forbidden in ${file}`,
      );
    }
  }
  for (const file of [
    pkg.manifest_path,
    join(workspaceRoot, "Cargo.toml"),
    join(workspaceRoot, ".cargo", "config.toml"),
    join(workspaceRoot, ".cargo", "config"),
  ]) {
    let text: string;
    try {
      text = await readFile(file, "utf8");
    } catch {
      continue;
    }
    if (manifestLintLowering.test(text) || /disallowed/u.test(text)) {
      violations.push(`${file} may not set the level of the disallowed lints`);
    }
  }
  return violations;
}

/**
 * Sources the lexical scan does not read: a `#[path]` module, a target
 * (binary, test, build script) outside the crate's scanned directories.
 */
async function layoutViolations(pkg: CargoPackage, crateRoot: string) {
  const violations: string[] = [];
  const scanned = ["src", "tests", "benches", "examples"].map((directory) =>
    join(crateRoot, directory),
  );
  for (const target of pkg.targets) {
    const path = resolve(target.src_path);
    if (target.kind.includes("custom-build")) {
      violations.push(`a build script (${path}) is forbidden`);
    } else if (
      !scanned.some((directory) => path.startsWith(`${directory}${sep}`))
    ) {
      violations.push(
        `target ${target.name} at ${path} is outside the scanned directories`,
      );
    }
  }
  for (const file of await allRustFiles(crateRoot)) {
    const text = stripRustCommentsAndStrings(await readFile(file, "utf8"));
    if (
      /#\s*!?\s*\[\s*path\s*=/u.test(text) ||
      /#\s*!?\s*\[\s*cfg_attr\s*\([^\]]*\bpath\s*=/u.test(text)
    ) {
      violations.push(`a #[path] module is forbidden in ${file}`);
    }
  }
  return violations;
}

/**
 * Roots whose glob import or macro-built path the scan refuses, with the
 * capabilities their members can grant.
 */
const capabilityRoots: Array<{ name: string; capabilities: Capability[] }> = [
  { name: "std", capabilities: ["network", "process"] },
  { name: "tokio", capabilities: ["network", "process"] },
  { name: "async_std", capabilities: ["network", "process"] },
  { name: "smol", capabilities: ["network", "process"] },
];

/**
 * `include!` targets in one source file (read before strings are stripped).
 * Each is refused unless it is a plain string literal naming a file inside
 * the crate's scanned directories (which the scan reads anyway); a target
 * built by another macro cannot be checked and is refused.
 */
function includeViolations(
  rawSource: string,
  sourcePath: string,
  crateRoot: string,
) {
  const violations: string[] = [];
  const scanned = ["src", "tests", "benches", "examples"].map((directory) =>
    join(crateRoot, directory),
  );
  for (const match of rawSource.matchAll(
    /\binclude\s*!\s*[(\[{]\s*([^)\]}]*)[)\]}]/gu,
  )) {
    const argument = match[1].trim();
    const literal = argument.match(/^"([^"\\]*)"$/u);
    if (!literal) {
      violations.push(
        `include! with a computed path is forbidden in ${sourcePath}`,
      );
      continue;
    }
    const target = resolve(dirname(sourcePath), literal[1]);
    if (!scanned.some((directory) => target.startsWith(`${directory}${sep}`))) {
      violations.push(
        `include! of ${literal[1]} (outside the crate's scanned sources) is forbidden in ${sourcePath}`,
      );
    }
  }
  return violations;
}

async function rustSourceFiles(crateRoot: string) {
  const files: string[] = [];
  const roots = ["src", "tests", "benches", "examples"];

  async function visit(directory: string) {
    let entries: Dirent[];
    try {
      entries = await readdir(directory, { withFileTypes: true });
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") {
        return;
      }
      throw error;
    }

    for (const entry of entries) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        await visit(path);
      } else if (entry.isFile() && entry.name.endsWith(".rs")) {
        files.push(path);
      }
    }
  }

  for (const directory of roots) {
    await visit(join(crateRoot, directory));
  }

  try {
    await stat(join(crateRoot, "build.rs"));
    files.push(join(crateRoot, "build.rs"));
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
      throw error;
    }
  }

  return files.sort();
}

function isPathWithin(parent: string, candidate: string) {
  const relativePath = relative(parent, candidate);
  return (
    relativePath !== "" &&
    relativePath !== ".." &&
    !relativePath.startsWith(`..${sep}`) &&
    !isAbsolute(relativePath)
  );
}

// ADR-0028 and ADR-0031: the CLI's bounded MCP health probe and its confirmed
// pull request post are the only production sources in a bounded-local crate
// that may start a child process. Each allowance is one exact file in one
// exact crate; it grants no process-capable dependency.
const sanctionedProcessSources = new Map<string, string[]>([
  ["compresso-cli", ["src/commands/mcp_probe.rs", "src/commands/pr_post.rs"]],
]);

function boundedLocalTestProcessSources(
  pkg: CargoPackage,
  className: CapabilityClassName,
  crateRoot: string,
) {
  const sources = new Set<string>();
  if (className !== "bounded-local") {
    return sources;
  }
  for (const relativePath of sanctionedProcessSources.get(pkg.name) ?? []) {
    sources.add(resolve(crateRoot, relativePath));
  }

  const testsRoot = resolve(crateRoot, "tests");
  const isIntegrationTest = (target: CargoTarget) =>
    target.kind.length === 1 && target.kind[0] === "test";
  const nonTestTargetSources = new Set(
    pkg.targets
      .filter((target) => !isIntegrationTest(target))
      .map((target) => resolve(target.src_path)),
  );

  for (const target of pkg.targets.filter(isIntegrationTest)) {
    const sourcePath = resolve(target.src_path);
    if (
      isPathWithin(testsRoot, sourcePath) &&
      !nonTestTargetSources.has(sourcePath)
    ) {
      sources.add(sourcePath);
    }
  }

  return sources;
}

function featureReaches(
  features: Record<string, string[]>,
  start: string,
  target: string,
) {
  const pending = [start];
  const visited = new Set<string>();
  while (pending.length > 0) {
    const current = pending.pop();
    if (!current || visited.has(current)) {
      continue;
    }
    visited.add(current);
    for (const reference of features[current] ?? []) {
      if (reference === target) {
        return true;
      }
      if (Object.hasOwn(features, reference)) {
        pending.push(reference);
      }
    }
  }
  return false;
}

function isEvalCalibrationDependency(
  pkg: CargoPackage,
  dependency: CargoDependency,
) {
  if (
    pkg.name !== "compresso-eval" ||
    dependency.name !== "reqwest" ||
    dependency.rename !== null ||
    dependency.kind !== null ||
    dependency.target !== null ||
    !dependency.optional
  ) {
    return false;
  }

  const calibration = pkg.features.calibration ?? [];
  if (!calibration.includes("dep:reqwest")) {
    return false;
  }

  for (const [feature, references] of Object.entries(pkg.features)) {
    if (
      feature !== "calibration" &&
      (references.some(
        (reference) =>
          reference === "dep:reqwest" ||
          reference === "reqwest" ||
          reference.startsWith("reqwest/"),
      ) ||
        featureReaches(pkg.features, feature, "calibration"))
    ) {
      return false;
    }
  }

  return pkg.targets.some(
    (target) =>
      target.name === "compresso-eval-calibrate" &&
      target.kind.includes("bin") &&
      target["required-features"].length === 1 &&
      target["required-features"][0] === "calibration",
  );
}

function dependencyCapabilities(dependency: CargoDependency) {
  const capabilities = new Set<Capability>();
  if (networkDependencies.has(dependency.name)) {
    capabilities.add("network");
  }
  if (processDependencies.has(dependency.name)) {
    capabilities.add("process");
  }
  if (dependency.name === "tokio" && dependency.features.includes("process")) {
    capabilities.add("process");
  }
  return capabilities;
}

function packageCapabilities(name: string, features: string[]) {
  const capabilities = new Set<Capability>();
  if (networkDependencies.has(name)) {
    capabilities.add("network");
  }
  if (processDependencies.has(name)) {
    capabilities.add("process");
  }
  if (name === "tokio" && features.includes("process")) {
    capabilities.add("process");
  }
  return capabilities;
}

/**
 * The workspace crate through which `compresso-mcp` and `compresso-cli` may
 * reach network-capable packages, and only it (D115, ADR-0036).
 */
const databaseNetworkCrate = "compresso-sql";
const databaseNetworkConsumers = new Set(["compresso-mcp", "compresso-cli"]);

type TreeLine = { depth: number; name: string; features: string[] };

/**
 * One member's own resolved dependency tree: the packages and features Cargo
 * builds for that member alone (amendment (a), D115), not the
 * workspace-unified resolution.
 */
async function memberTree(
  manifestPath: string,
  member: string,
  locked: boolean,
): Promise<TreeLine[]> {
  const args = [
    "tree",
    locked ? "--locked" : "--offline",
    "-p",
    member,
    "-e",
    "normal,build,dev",
    "--target",
    "all",
    "--prefix",
    "depth",
    "-f",
    "{p}|{f}",
    "--manifest-path",
    manifestPath,
  ];
  let stdout: string;
  try {
    ({ stdout } = await execFileAsync("cargo", args, {
      cwd: dirname(manifestPath),
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
    }));
  } catch (error) {
    const stderr = (error as { stderr?: string }).stderr?.trim();
    const hint = locked
      ? ""
      : " (the capability check resolves a copy of the workspace offline; run `cargo fetch --locked` first so every crate is in the local cache)";
    throw new Error(
      `cargo tree failed for ${member}${hint}${stderr ? `: ${stderr}` : ""}`,
      { cause: error },
    );
  }
  const lines: TreeLine[] = [];
  for (const line of stdout.split("\n")) {
    const match = line.match(/^(\d+)(\S+) v\S+[^|]*\|(.*)$/u);
    if (!match) {
      continue;
    }
    lines.push({
      depth: Number(match[1]),
      name: match[2],
      features: match[3]
        .replace(/ \(\*\)$/u, "")
        .split(",")
        .filter((feature) => feature.length > 0),
    });
  }
  return lines;
}

/** The first dependency path in `lines` that grants `capability`. */
function treeCapabilityPath(
  lines: TreeLine[],
  capability: Capability,
  ignoreTokioProcess = false,
) {
  const stack: string[] = [];
  for (const line of lines) {
    stack.length = line.depth;
    stack.push(line.name);
    if (line.depth === 0) {
      continue;
    }
    const capabilities = packageCapabilities(line.name, line.features);
    if (
      ignoreTokioProcess &&
      capability === "process" &&
      line.name === "tokio" &&
      !processDependencies.has(line.name)
    ) {
      capabilities.delete("process");
    }
    if (capabilities.has(capability)) {
      return [...stack];
    }
  }
  return undefined;
}

/**
 * The MongoDB driver (E17-52, D117) turns on Tokio's `process` feature in
 * its own manifest, unconditionally; its only use of it (spawning the
 * encryption helper) is compiled only with the driver's `in-use-encryption`
 * feature. The feature itself grants no process API to Compresso's code: the
 * source scan still forbids `std::process` and `tokio::process` in every
 * workspace crate. So for the database crate and its two consumers the
 * feature is not counted when, in the member's own resolved tree, every
 * enabler of `tokio feature "process"` is the `mongodb` package itself and
 * `mongodb` is built without `in-use-encryption`. Anything else (another
 * enabler, a Tokio feature such as `full` that implies it, the encryption
 * feature) keeps the capability, so the check fails closed.
 */
const tokioProcessDriver = "mongodb";
/**
 * The exemption holds only for the driver from crates.io at exactly the
 * version `compresso-sql` pins; a path, git, or `[patch]` crate named
 * `mongodb`, or another version, keeps the capability (fail closed). A
 * version bump must update this pin after reading the new manifest.
 */
export type TokioProcessDriverPin = { source: string | null; version: string };
const tokioProcessDriverPin: TokioProcessDriverPin = {
  source: "registry+https://github.com/rust-lang/crates.io-index",
  version: "3.10.2",
};

function driverMatchesPin(resolved: CargoMetadata, pin: TokioProcessDriverPin) {
  const drivers = resolved.packages.filter(
    (pkg) => pkg.name === tokioProcessDriver,
  );
  return (
    drivers.length > 0 &&
    drivers.every(
      (pkg) =>
        (pkg.source ?? null) === pin.source && pkg.version === pin.version,
    )
  );
}
const tokioProcessDriverForbiddenFeatures = [
  "in-use-encryption",
  "in-use-encryption-unstable",
];

async function tokioProcessOnlyFromDriver(
  manifestPath: string,
  member: string,
  tree: TreeLine[],
) {
  const driverLines = tree.filter((line) => line.name === tokioProcessDriver);
  if (
    driverLines.length === 0 ||
    driverLines.some((line) =>
      line.features.some((feature) =>
        tokioProcessDriverForbiddenFeatures.includes(feature),
      ),
    )
  ) {
    return false;
  }
  let stdout: string;
  try {
    ({ stdout } = await execFileAsync(
      "cargo",
      [
        "tree",
        "--locked",
        "-p",
        member,
        "-e",
        "normal,build,dev,features",
        "--target",
        "all",
        "--prefix",
        "depth",
        "-f",
        "{p}",
        "--manifest-path",
        manifestPath,
      ],
      {
        cwd: dirname(manifestPath),
        encoding: "utf8",
        maxBuffer: 256 * 1024 * 1024,
      },
    ));
  } catch (error) {
    const stderr = (error as { stderr?: string }).stderr?.trim();
    throw new Error(
      `cargo tree -e features failed for ${member}${stderr ? `: ${stderr}` : ""}`,
      { cause: error },
    );
  }
  const stack: string[] = [];
  let enablers = 0;
  for (const line of stdout.split("\n")) {
    const match = line.match(/^(\d+)(.+?)(?: \(\*\))?$/u);
    if (!match) {
      continue;
    }
    const depth = Number(match[1]);
    const text = match[2];
    stack.length = depth;
    stack.push(text);
    if (depth === 0 || text !== 'tokio feature "process"') {
      continue;
    }
    enablers += 1;
    const parent = stack[depth - 1] ?? "";
    if (
      !new RegExp(`^${tokioProcessDriver} v\\S+(?: \\(.*\\))?$`, "u").test(
        parent,
      )
    ) {
      return false;
    }
  }
  return enablers > 0;
}

/**
 * Every path from `member` to a network package in its real resolved tree
 * (inverted, without de-duplication), returning the first that passes
 * through a workspace crate other than compresso-sql (or the other exempt
 * consumer: compresso-cli reaches compresso-sql through compresso-mcp).
 */
async function networkPathThroughOtherMember(
  manifestPath: string,
  member: string,
  tree: TreeLine[],
  members: Set<string>,
) {
  const targets = [
    ...new Set(
      tree
        .filter((line) => line.depth > 0 && networkDependencies.has(line.name))
        .map((line) => line.name),
    ),
  ];
  for (const target of targets) {
    let stdout: string;
    try {
      ({ stdout } = await execFileAsync(
        "cargo",
        [
          "tree",
          "--locked",
          "-p",
          member,
          "-i",
          target,
          "--no-dedupe",
          "-e",
          "normal,build,dev",
          "--target",
          "all",
          "--prefix",
          "depth",
          "-f",
          "{p}",
          "--manifest-path",
          manifestPath,
        ],
        {
          cwd: dirname(manifestPath),
          encoding: "utf8",
          maxBuffer: 256 * 1024 * 1024,
        },
      ));
    } catch (error) {
      const stderr = (error as { stderr?: string }).stderr?.trim();
      throw new Error(
        `cargo tree -i ${target} failed for ${member}${stderr ? `: ${stderr}` : ""}`,
        { cause: error },
      );
    }
    const stack: string[] = [];
    for (const line of stdout.split("\n")) {
      const match = line.match(/^(\d+)(\S+) v/u);
      if (!match) {
        continue;
      }
      const depth = Number(match[1]);
      stack.length = depth;
      stack.push(match[2]);
      if (match[2] !== member || depth === 0) {
        continue;
      }
      // stack is target -> ... -> member; the path runs the other way.
      const path = [...stack].reverse();
      const others = path
        .slice(1, -1)
        .filter(
          (name) =>
            members.has(name) &&
            name !== databaseNetworkCrate &&
            !databaseNetworkConsumers.has(name),
        );
      if (others.length > 0) {
        return { path, member: others[0] };
      }
    }
  }
  return undefined;
}

/**
 * Copies the workspace's manifests (with stub target files) into a temporary
 * directory with the database crate and every dependency on it removed, and
 * returns that copy's root manifest. Resolving a member there shows what the
 * member would reach if `compresso-sql` did not exist: a network package
 * still reachable there reaches the member by some other route. Fails closed
 * on any declaration of the crate it cannot remove.
 */
async function shadowWithoutDatabaseCrate(
  workspaceRoot: string,
  resolved: CargoMetadata,
) {
  if (
    !resolved.packages.some(
      (pkg) =>
        pkg.name === databaseNetworkCrate &&
        resolved.workspace_members.includes(pkg.id),
    ) ||
    resolved.packages.some(
      (pkg) =>
        pkg.name === databaseNetworkCrate &&
        !resolved.workspace_members.includes(pkg.id),
    )
  ) {
    throw new Error(
      `${databaseNetworkCrate} must be exactly one workspace member to grant its network exception`,
    );
  }
  const shadow = await mkdtemp(join(tmpdir(), "compresso-capabilities-"));
  const strip = (text: string, source: string) => {
    const kept = text
      .split("\n")
      .filter(
        (line) =>
          !new RegExp(
            `^\\s*${databaseNetworkCrate}\\s*(=|\\.workspace\\s*=)`,
            "u",
          ).test(line),
      )
      .join("\n")
      .replace(new RegExp(`"[^"\\n]*${databaseNetworkCrate}"\\s*,?`, "gu"), "");
    if (kept.includes(databaseNetworkCrate)) {
      throw new Error(
        `${source}: declare ${databaseNetworkCrate} on one inline dependency line so the capability check can remove it`,
      );
    }
    return kept;
  };
  const place = async (source: string, contents?: string) => {
    const relativePath = relative(workspaceRoot, source);
    if (relativePath.startsWith("..") || isAbsolute(relativePath)) {
      throw new Error(`${source}: local package outside the workspace root`);
    }
    const target = join(shadow, relativePath);
    await mkdir(dirname(target), { recursive: true });
    if (contents === undefined) {
      await copyFile(source, target);
    } else {
      await writeFile(target, contents);
    }
  };
  const rootManifest = join(workspaceRoot, "Cargo.toml");
  let rootCopied = false;
  for (const pkg of resolved.packages) {
    if (pkg.source !== null && pkg.source !== undefined) {
      continue;
    }
    if (pkg.name === databaseNetworkCrate) {
      continue;
    }
    const manifest = resolve(pkg.manifest_path);
    await place(manifest, strip(await readFile(manifest, "utf8"), manifest));
    rootCopied ||= manifest === rootManifest;
    for (const target of pkg.targets) {
      await place(resolve(target.src_path), "");
    }
  }
  if (!rootCopied) {
    await place(
      rootManifest,
      strip(await readFile(rootManifest, "utf8"), rootManifest),
    );
  }
  for (const name of ["Cargo.lock", "rust-toolchain.toml"]) {
    const source = join(workspaceRoot, name);
    try {
      await stat(source);
    } catch {
      continue;
    }
    await place(source);
  }
  return shadow;
}

async function hasLockfile(root: string) {
  try {
    await stat(join(root, "Cargo.lock"));
    return true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") {
      return false;
    }
    throw error;
  }
}

export async function checkWorkspace(
  root = process.cwd(),
  options: { tokioProcessDriverPin?: TokioProcessDriverPin } = {},
): Promise<CheckResult> {
  const driverPin = options.tokioProcessDriverPin ?? tokioProcessDriverPin;
  const workspaceRoot = resolve(root);
  const manifestPath = join(workspaceRoot, "Cargo.toml");
  const declared = await cargoMetadata(manifestPath, false);
  const workspacePackages = new Map(
    declared.packages.map((pkg) => [pkg.id, pkg]),
  );
  const violations = new Set<string>();

  if (declared.workspace_members.length === 0) {
    violations.add(`${workspaceRoot}: no Rust workspace members found`);
  }

  for (const memberId of declared.workspace_members) {
    const pkg = workspacePackages.get(memberId);
    if (!pkg) {
      violations.add(`${memberId}: Cargo omitted workspace member metadata`);
      continue;
    }
    const className = crateClasses.get(pkg.name);
    if (!className) {
      violations.add(
        `${pkg.name}: unknown capability class; add an approved matrix entry before use`,
      );
      continue;
    }

    const capabilities = capabilityClasses[className];
    for (const dependency of pkg.dependencies) {
      for (const capability of dependencyCapabilities(dependency)) {
        if (
          !capabilities[capability] &&
          !(
            capability === "network" &&
            isEvalCalibrationDependency(pkg, dependency)
          )
        ) {
          const alias = dependency.rename
            ? ` (renamed ${dependency.rename})`
            : "";
          violations.add(
            `${pkg.name} (${className}): dependency ${dependency.name}${alias} grants ${capability} capability`,
          );
        }
      }
    }

    const crateRoot = dirname(pkg.manifest_path);
    const dependencyRoots = new Set(
      pkg.dependencies.map((dependency) =>
        (dependency.rename ?? dependency.name).replaceAll("-", "_"),
      ),
    );
    const testProcessSources = boundedLocalTestProcessSources(
      pkg,
      className,
      crateRoot,
    );
    for (const sourcePath of await rustSourceFiles(crateRoot)) {
      const rawSource = await readFile(sourcePath, "utf8");
      if (!capabilities.network || !capabilities.process) {
        for (const violation of includeViolations(
          rawSource,
          sourcePath,
          crateRoot,
        )) {
          violations.add(`${pkg.name} (${className}): ${violation}`);
        }
      }
      const source = stripRustCommentsAndStrings(rawSource);
      for (const use of sourceCapabilityUses(source, dependencyRoots)) {
        const allowedTestProcess =
          use.capability === "process" &&
          testProcessSources.has(resolve(sourcePath));
        if (!capabilities[use.capability] && !allowedTestProcess) {
          violations.add(
            `${pkg.name} (${className}): ${use.capability} API ${use.label} is forbidden in ${sourcePath}`,
          );
        }
      }
    }
    if (!capabilities.network || !capabilities.process) {
      for (const violation of await layoutViolations(pkg, crateRoot)) {
        violations.add(`${pkg.name} (${className}): ${violation}`);
      }
    }
    if (semanticProcessCrates.has(pkg.name)) {
      for (const violation of await semanticControlViolations(
        pkg,
        crateRoot,
        workspaceRoot,
      )) {
        violations.add(`${pkg.name} (${className}): ${violation}`);
      }
    }
  }

  if (await hasLockfile(workspaceRoot)) {
    const resolved = await cargoMetadata(manifestPath, true);
    let shadow: string | undefined;
    try {
      for (const memberId of resolved.workspace_members) {
        const pkg = resolved.packages.find(
          (candidate) => candidate.id === memberId,
        );
        if (!pkg) {
          continue;
        }
        const className = crateClasses.get(pkg.name);
        if (!className) {
          continue;
        }
        const capabilities = capabilityClasses[className];
        if (capabilities.network && capabilities.process) {
          continue;
        }
        const tree = await memberTree(manifestPath, pkg.name, true);
        for (const capability of ["network", "process"] as const) {
          if (capabilities[capability]) {
            continue;
          }
          let path = treeCapabilityPath(tree, capability);
          let through = "";
          if (
            path &&
            capability === "process" &&
            path.at(-1) === "tokio" &&
            (pkg.name === databaseNetworkCrate ||
              databaseNetworkConsumers.has(pkg.name)) &&
            driverMatchesPin(resolved, driverPin) &&
            (await tokioProcessOnlyFromDriver(manifestPath, pkg.name, tree))
          ) {
            // D117: Tokio's process feature, turned on only by the MongoDB
            // driver without its encryption feature; any other process
            // package still counts.
            path = treeCapabilityPath(tree, capability, true);
          }
          if (
            path &&
            capability === "network" &&
            databaseNetworkConsumers.has(pkg.name)
          ) {
            // Amendment (b), D115: these two crates may reach network
            // packages only through compresso-sql. Resolve the member
            // again with that crate removed; anything still reachable
            // arrives by another route.
            shadow ??= await shadowWithoutDatabaseCrate(
              workspaceRoot,
              resolved,
            );
            path = treeCapabilityPath(
              await memberTree(join(shadow, "Cargo.toml"), pkg.name, false),
              capability,
            );
            through = ` without ${databaseNetworkCrate}`;
            // In the real tree, no path to a network package may pass
            // through a workspace crate other than compresso-sql (a feature
            // compresso-sql turns on in a pure-local crate would otherwise
            // pass the shadow resolve).
            if (!path) {
              const members = new Set(
                resolved.packages
                  .filter((candidate) =>
                    resolved.workspace_members.includes(candidate.id),
                  )
                  .map((candidate) => candidate.name),
              );
              const via = await networkPathThroughOtherMember(
                manifestPath,
                pkg.name,
                tree,
                members,
              );
              if (via) {
                path = via.path;
                through = ` passes through workspace crate ${via.member}, not only ${databaseNetworkCrate}, and`;
              }
            }
          }
          if (path) {
            violations.add(
              `${pkg.name} (${className}): resolved dependency path ${path.join(" -> ")}${through} grants ${capability} capability`,
            );
          }
        }
      }
    } finally {
      if (shadow) {
        await rm(shadow, { recursive: true, force: true });
      }
    }
  }

  const sortedViolations = [...violations].sort();
  return {
    ok: sortedViolations.length === 0,
    checkedCrates: declared.workspace_members.length,
    violations: sortedViolations,
  };
}

function parseRootArgument(argv: string[]) {
  if (argv.length === 0) {
    return process.cwd();
  }
  if (argv.length === 1 && (argv[0] === "--help" || argv[0] === "-h")) {
    console.log("Usage: check-crate-capabilities.ts [--root <workspace>]");
    process.exit(0);
  }
  if (argv.length === 2 && argv[0] === "--root") {
    return argv[1];
  }
  throw new Error("expected no arguments or --root <workspace>");
}

const invokedPath = process.argv[1]
  ? pathToFileURL(resolve(process.argv[1])).href
  : "";
if (import.meta.url === invokedPath) {
  try {
    const result = await checkWorkspace(
      parseRootArgument(process.argv.slice(2)),
    );
    if (!result.ok) {
      for (const violation of result.violations) {
        console.error(`error: ${violation}`);
      }
      process.exitCode = 1;
    } else {
      const scriptName = basename(fileURLToPath(import.meta.url));
      console.log(
        `${scriptName}: checked ${result.checkedCrates} crates; capability boundaries pass`,
      );
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 2;
  }
}
