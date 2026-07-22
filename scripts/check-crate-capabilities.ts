#!/usr/bin/env node

import { execFile } from "node:child_process";
import type { Dirent } from "node:fs";
import { readFile, readdir, stat } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
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
      for (const match of body.matchAll(
        /\bprocess\s*::\s*\{([^}]*)\}/gu,
      )) {
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
      if (
        entry.name !== "self" &&
        !inertStdProcessMembers.has(entry.name)
      ) {
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
  const output = [...source];
  let blockDepth = 0;
  let inLineComment = false;
  let inString = false;
  let escaped = false;
  let rawTerminator: string | undefined;

  for (let index = 0; index < source.length; index += 1) {
    const rest = source.slice(index);
    const character = source[index];
    const next = source[index + 1];

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
      if (rest.startsWith(rawTerminator)) {
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
      if (character !== "\\") {
        escaped = false;
      }
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

    const rawStart = rest.match(/^(?:br|r)(#*)"/u);
    if (rawStart) {
      for (let offset = 0; offset < rawStart[0].length; offset += 1) {
        output[index + offset] = " ";
      }
      index += rawStart[0].length - 1;
      rawTerminator = `"${rawStart[1]}`;
      continue;
    }

    if (character === '"') {
      output[index] = " ";
      inString = true;
    }
  }

  return output.join("");
}

function sourceCapabilityUses(source: string) {
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

  return uses;
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

function packageCapabilities(pkg: CargoPackage, node: CargoResolveNode) {
  const capabilities = new Set<Capability>();
  if (networkDependencies.has(pkg.name)) {
    capabilities.add("network");
  }
  if (processDependencies.has(pkg.name)) {
    capabilities.add("process");
  }
  if (pkg.name === "tokio" && node.features.includes("process")) {
    capabilities.add("process");
  }
  return capabilities;
}

function resolvedCapabilityPath(
  metadata: CargoMetadata,
  rootId: string,
  capability: Capability,
) {
  if (!metadata.resolve) {
    return undefined;
  }

  const packages = new Map(metadata.packages.map((pkg) => [pkg.id, pkg]));
  const nodes = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
  const root = packages.get(rootId);
  const pending = [{ id: rootId, path: root ? [root.name] : [rootId] }];
  const visited = new Set([rootId]);

  while (pending.length > 0) {
    const current = pending.shift();
    if (!current) {
      break;
    }
    const node = nodes.get(current.id);
    if (!node) {
      continue;
    }

    for (const dependency of node.deps) {
      if (visited.has(dependency.pkg)) {
        continue;
      }
      visited.add(dependency.pkg);
      const dependencyPackage = packages.get(dependency.pkg);
      const dependencyNode = nodes.get(dependency.pkg);
      const dependencyName = dependencyPackage?.name ?? dependency.pkg;
      const path = [...current.path, dependencyName];
      if (
        dependencyPackage &&
        dependencyNode &&
        packageCapabilities(dependencyPackage, dependencyNode).has(capability)
      ) {
        return path;
      }
      pending.push({ id: dependency.pkg, path });
    }
  }

  return undefined;
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
): Promise<CheckResult> {
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

    for (const sourcePath of await rustSourceFiles(
      dirname(pkg.manifest_path),
    )) {
      const source = stripRustCommentsAndStrings(
        await readFile(sourcePath, "utf8"),
      );
      for (const use of sourceCapabilityUses(source)) {
        if (!capabilities[use.capability]) {
          violations.add(
            `${pkg.name} (${className}): ${use.capability} API ${use.label} is forbidden in ${sourcePath}`,
          );
        }
      }
    }
  }

  if (await hasLockfile(workspaceRoot)) {
    const resolved = await cargoMetadata(manifestPath, true);
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
      for (const capability of ["network", "process"] as const) {
        if (capabilities[capability]) {
          continue;
        }
        const path = resolvedCapabilityPath(resolved, memberId, capability);
        if (path) {
          violations.add(
            `${pkg.name} (${className}): resolved dependency path ${path.join(" -> ")} grants ${capability} capability`,
          );
        }
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
