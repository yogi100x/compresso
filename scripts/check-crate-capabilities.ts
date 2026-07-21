#!/usr/bin/env node

import type { Dirent } from "node:fs";
import { readFile, readdir } from "node:fs/promises";
import { basename, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

type CapabilityClass = {
  network: boolean;
  process: boolean;
};

type Dependency = {
  name: string;
  features: Set<string>;
  workspace: boolean;
};

export type CheckResult = {
  ok: boolean;
  checkedCrates: number;
  violations: string[];
};

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

const sourceCapabilities = {
  network: [
    { label: "std::net", pattern: /\bstd\s*::\s*net\b/u },
    { label: "tokio::net", pattern: /\btokio\s*::\s*net\b/u },
    { label: "async_std::net", pattern: /\basync_std\s*::\s*net\b/u },
    { label: "smol::net", pattern: /\bsmol\s*::\s*net\b/u },
  ],
  process: [
    { label: "std::process", pattern: /\bstd\s*::\s*process\b/u },
    { label: "tokio::process", pattern: /\btokio\s*::\s*process\b/u },
    { label: "async_process", pattern: /\basync_process\b/u },
  ],
};

function stripTomlComment(line: string) {
  let quoted = false;
  let escaped = false;

  for (let index = 0; index < line.length; index += 1) {
    const character = line[index];
    if (character === '"' && !escaped) {
      quoted = !quoted;
    }
    if (character === "#" && !quoted) {
      return line.slice(0, index);
    }
    escaped = character === "\\" && !escaped;
    if (character !== "\\") {
      escaped = false;
    }
  }

  return line;
}

function packageName(manifest: string) {
  const packageSection = manifest.match(
    /(?:^|\n)\s*\[package\]\s*\n([\s\S]*?)(?=\n\s*\[|$)/u,
  )?.[1];
  return packageSection?.match(/^\s*name\s*=\s*["']([^"']+)["']/mu)?.[1];
}

function dependencyFromEntry(key: string, value: string): Dependency {
  const normalizedKey = key.replace(/\.workspace$/u, "");
  const packageOverride = value.match(/\bpackage\s*=\s*["']([^"']+)["']/u)?.[1];
  const featureList = value.match(/\bfeatures\s*=\s*\[([^\]]*)\]/u)?.[1] ?? "";
  const features = new Set(
    [...featureList.matchAll(/["']([^"']+)["']/gu)].map((match) => match[1]),
  );

  return {
    name: packageOverride ?? normalizedKey,
    features,
    workspace:
      /\bworkspace\s*=\s*true\b/u.test(value) || /\.workspace\s*=/u.test(key),
  };
}

function parseDependencies(manifest: string) {
  const dependencies = new Map<string, Dependency>();
  const lines = manifest.split(/\r?\n/u);
  let section = "";
  let dependencyTable: string | undefined;

  for (let index = 0; index < lines.length; index += 1) {
    const line = stripTomlComment(lines[index]).trim();
    const sectionMatch = line.match(/^\[([^\]]+)\]$/u);
    if (sectionMatch) {
      section = sectionMatch[1];
      dependencyTable = section.match(
        /(?:^|\.)(?:dependencies|dev-dependencies|build-dependencies)\.([A-Za-z0-9_-]+)$/u,
      )?.[1];
      if (dependencyTable && !dependencies.has(dependencyTable)) {
        dependencies.set(
          dependencyTable,
          dependencyFromEntry(dependencyTable, ""),
        );
      }
      continue;
    }

    if (dependencyTable) {
      if (line.startsWith("package")) {
        const current = dependencies.get(dependencyTable);
        if (current) {
          dependencies.set(
            dependencyTable,
            dependencyFromEntry(dependencyTable, line),
          );
        }
      }
      continue;
    }

    if (
      !/(?:^|\.)(?:dependencies|dev-dependencies|build-dependencies)$/u.test(
        section,
      )
    ) {
      continue;
    }

    const entry = line.match(/^([A-Za-z0-9_-]+(?:\.workspace)?)\s*=\s*(.*)$/u);
    if (!entry) {
      continue;
    }

    let value = entry[2];
    let balance =
      (value.match(/[\[{]/gu)?.length ?? 0) -
      (value.match(/[\]}]/gu)?.length ?? 0);
    while (balance > 0 && index + 1 < lines.length) {
      index += 1;
      const continuation = stripTomlComment(lines[index]).trim();
      value += ` ${continuation}`;
      balance +=
        (continuation.match(/[\[{]/gu)?.length ?? 0) -
        (continuation.match(/[\]}]/gu)?.length ?? 0);
    }

    const key = entry[1].replace(/\.workspace$/u, "");
    dependencies.set(key, dependencyFromEntry(entry[1], value));
  }

  return [...dependencies.values()];
}

function mergeWorkspaceFeatures(
  dependencies: Dependency[],
  workspace: Map<string, Dependency>,
) {
  return dependencies.map((dependency) => {
    if (!dependency.workspace) {
      return dependency;
    }

    const inherited = workspace.get(dependency.name);
    return {
      ...dependency,
      features: new Set([
        ...(inherited?.features ?? []),
        ...dependency.features,
      ]),
    };
  });
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
    await readFile(join(crateRoot, "build.rs"));
    files.push(join(crateRoot, "build.rs"));
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
      throw error;
    }
  }

  return files.sort();
}

async function manifestPaths(root: string) {
  const rootManifest = join(root, "Cargo.toml");
  const paths: string[] = [];
  let rootContents = "";

  try {
    rootContents = await readFile(rootManifest, "utf8");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
      throw error;
    }
  }

  if (packageName(rootContents)) {
    paths.push(rootManifest);
    return { paths, rootContents };
  }

  const rustRoot = join(root, "rust");
  let entries: Dirent[];
  try {
    entries = await readdir(rustRoot, { withFileTypes: true });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") {
      return { paths, rootContents };
    }
    throw error;
  }

  for (const entry of entries) {
    if (entry.isDirectory()) {
      const manifest = join(rustRoot, entry.name, "Cargo.toml");
      try {
        await readFile(manifest);
        paths.push(manifest);
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
          throw error;
        }
      }
    }
  }

  return { paths: paths.sort(), rootContents };
}

export async function checkWorkspace(
  root = process.cwd(),
): Promise<CheckResult> {
  const workspaceRoot = resolve(root);
  const { paths, rootContents } = await manifestPaths(workspaceRoot);
  const workspaceDependencies = new Map(
    parseDependencies(rootContents).map((dependency) => [
      dependency.name,
      dependency,
    ]),
  );
  const violations: string[] = [];

  if (paths.length === 0) {
    violations.push(`${workspaceRoot}: no Rust crate manifests found`);
  }

  for (const manifestPath of paths) {
    const manifest = await readFile(manifestPath, "utf8");
    const name = packageName(manifest) ?? basename(resolve(manifestPath, ".."));
    const className = crateClasses.get(name);
    if (!className) {
      violations.push(
        `${name}: unknown capability class; add an approved matrix entry before use`,
      );
      continue;
    }

    const capabilities = capabilityClasses[className];
    const dependencies = mergeWorkspaceFeatures(
      parseDependencies(manifest),
      workspaceDependencies,
    );

    for (const dependency of dependencies) {
      if (!capabilities.network && networkDependencies.has(dependency.name)) {
        violations.push(
          `${name} (${className}): dependency ${dependency.name} grants network capability`,
        );
      }
      if (!capabilities.process && processDependencies.has(dependency.name)) {
        violations.push(
          `${name} (${className}): dependency ${dependency.name} grants process capability`,
        );
      }
      if (
        !capabilities.process &&
        dependency.name === "tokio" &&
        dependency.features.has("process")
      ) {
        violations.push(
          `${name} (${className}): tokio process feature grants process capability`,
        );
      }
    }

    for (const sourcePath of await rustSourceFiles(
      resolve(manifestPath, ".."),
    )) {
      const source = stripRustCommentsAndStrings(
        await readFile(sourcePath, "utf8"),
      );
      for (const [capability, checks] of Object.entries(sourceCapabilities)) {
        if (capabilities[capability as keyof CapabilityClass]) {
          continue;
        }
        for (const check of checks) {
          if (check.pattern.test(source)) {
            violations.push(
              `${name} (${className}): process API ${check.label} is forbidden in ${sourcePath}`.replace(
                "process API",
                `${capability} API`,
              ),
            );
          }
        }
      }
    }
  }

  violations.sort();
  return {
    ok: violations.length === 0,
    checkedCrates: paths.length,
    violations,
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
