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
  { capability: "process", root: "std", member: "process" },
  { capability: "process", root: "tokio", member: "process" },
  { capability: "process", root: "async_process", member: "Command" },
];

function regexEscape(value: string) {
  return value.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
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
  const uses: Array<{ capability: Capability; label: string }> = [];

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
