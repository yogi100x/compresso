#!/usr/bin/env node

// Documentation drift linter.
//
// Compares what the documentation says about the product surface with what
// the code actually exposes. The surface inventory is produced by the build
// itself (`cargo run -p compresso-cli --example surface_inventory`), so the
// linter never guesses at commands, flags, tools, or configuration keys by
// reading Rust source.
//
// Scope, deliberately distinct from the plan linter planned in E16-16:
// - Current-state documents (configured `docs.include`) are checked in full.
// - Plan documents are forward-looking and are not scanned for prose. The only
//   plan claim treated as current state is the `Files:` line of a ticket that
//   is checked off as done.
// - Plan-internal consistency (ticket IDs, dependencies, rollups, templates,
//   and the files of tickets that are still open) belongs to the plan linter.

import type { Dirent } from "node:fs";
import { readFile, readdir, stat } from "node:fs/promises";
import { basename, dirname, join, posix, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

export const REPORT_SCHEMA_VERSION = 1;
const INVENTORY_SCHEMA_VERSION = 1;
const CONFIG_SCHEMA_VERSION = 1;

export type FindingKind =
  | "unknown_cli_command"
  | "unknown_cli_flag"
  | "unknown_mcp_tool"
  | "unknown_config_key"
  | "missing_path"
  | "missing_ticket_file"
  | "undocumented_cli_command"
  | "undocumented_mcp_tool"
  | "stale_allowlist_entry";

const findingKinds = new Set<string>([
  "unknown_cli_command",
  "unknown_cli_flag",
  "unknown_mcp_tool",
  "unknown_config_key",
  "missing_path",
  "missing_ticket_file",
  "undocumented_cli_command",
  "undocumented_mcp_tool",
]);

export type Finding = {
  kind: FindingKind;
  /** Repository-relative document, or "" for surface-wide findings. */
  doc: string;
  /** One-based line, or 0 for surface-wide findings. */
  line: number;
  reference: string;
  message: string;
};

export type AllowedFinding = Finding & { reason: string };

export type InventoryCommand = {
  path: string[];
  hidden: boolean;
  has_subcommands: boolean;
  flags: string[];
};

export type Inventory = {
  schema_version: number;
  binaries: { name: string; commands: InventoryCommand[] }[];
  mcp_tools: string[];
  config_keys: string[];
};

export type AllowEntry = {
  kind: string;
  reference: string;
  doc?: string;
  reason: string;
};

export type DriftConfig = {
  schema_version: number;
  docs: { include: string[]; exclude: string[] };
  /** Plan documents whose completed tickets' `Files:` lines are checked. */
  completed_tickets: string[];
  /** Contract documents that document surface but are not themselves linted. */
  mention_only_docs: string[];
  /** Top-level entries under which a slash-separated code span is a path. */
  path_roots: string[];
  /** Binaries whose every visible command must be documented. */
  require_documented_binaries: string[];
  /** Maps `cargo run -p <package>` to the binary it runs. */
  cargo_packages: Record<string, string>;
  allow: AllowEntry[];
};

export type DriftReport = {
  schema_version: number;
  passed: boolean;
  docs_scanned: number;
  counts: Record<string, number>;
  findings: Finding[];
  allowlisted: AllowedFinding[];
};

export class DriftInputError extends Error {}

const skippedDirectories = new Set([
  ".git",
  ".claude",
  "node_modules",
  "target",
  ".turbo",
]);
const shellFenceLanguages = new Set([
  "",
  "sh",
  "bash",
  "zsh",
  "shell",
  "console",
  "text",
]);
const shellSeparators = new Set(["|", "||", "&&", ";", ">", ">>", "<", "&"]);
const fileExtensions = new Set([
  "rs",
  "ts",
  "tsx",
  "js",
  "mjs",
  "json",
  "toml",
  "md",
  "yml",
  "yaml",
  "sh",
  "rb",
  "py",
  "db",
  "sqlite",
  "lock",
  "txt",
  "html",
  "css",
  "exe",
]);

function toPosix(path: string) {
  return path.split("\\").join("/");
}

function globToRegExp(glob: string) {
  let pattern = "";
  for (let index = 0; index < glob.length; index += 1) {
    const char = glob[index];
    if (char === "*") {
      if (glob[index + 1] === "*") {
        if (glob[index + 2] === "/") {
          pattern += "(?:.*/)?";
          index += 2;
        } else {
          pattern += ".*";
          index += 1;
        }
      } else {
        pattern += "[^/]*";
      }
    } else if (char === "?") {
      pattern += "[^/]";
    } else {
      pattern += char.replace(/[.+^${}()|[\]\\]/gu, "\\$&");
    }
  }
  return new RegExp(`^${pattern}$`, "u");
}

async function listFiles(root: string) {
  const files: string[] = [];
  async function walk(directory: string) {
    let entries: Dirent[];
    try {
      entries = await readdir(directory, { withFileTypes: true });
    } catch {
      return;
    }
    entries.sort((left, right) => (left.name < right.name ? -1 : 1));
    for (const entry of entries) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        if (!skippedDirectories.has(entry.name)) {
          await walk(path);
        }
      } else if (entry.isFile()) {
        files.push(toPosix(relative(root, path)));
      }
    }
  }
  await walk(root);
  return files;
}

async function exists(path: string) {
  try {
    await stat(path);
    return true;
  } catch {
    return false;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isStringArray(value: unknown): value is string[] {
  return (
    Array.isArray(value) && value.every((item) => typeof item === "string")
  );
}

export function parseInventory(text: string): Inventory {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch (error) {
    throw new DriftInputError(
      `surface inventory is not JSON: ${(error as Error).message}`,
    );
  }
  if (!isRecord(value) || value.schema_version !== INVENTORY_SCHEMA_VERSION) {
    throw new DriftInputError(
      `surface inventory schema_version must be ${INVENTORY_SCHEMA_VERSION}`,
    );
  }
  const { binaries, mcp_tools: tools, config_keys: keys } = value;
  if (
    !Array.isArray(binaries) ||
    !isStringArray(tools) ||
    !isStringArray(keys)
  ) {
    throw new DriftInputError("surface inventory is missing required arrays");
  }
  for (const binary of binaries) {
    if (
      !isRecord(binary) ||
      typeof binary.name !== "string" ||
      !Array.isArray(binary.commands)
    ) {
      throw new DriftInputError("surface inventory binary entry is malformed");
    }
    for (const command of binary.commands) {
      if (
        !isRecord(command) ||
        !isStringArray(command.path) ||
        command.path.length === 0 ||
        typeof command.hidden !== "boolean" ||
        typeof command.has_subcommands !== "boolean" ||
        !isStringArray(command.flags)
      ) {
        throw new DriftInputError(
          `surface inventory command entry is malformed in ${binary.name}`,
        );
      }
    }
  }
  return value as Inventory;
}

export function parseConfig(text: string): DriftConfig {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch (error) {
    throw new DriftInputError(
      `drift configuration is not JSON: ${(error as Error).message}`,
    );
  }
  if (!isRecord(value) || value.schema_version !== CONFIG_SCHEMA_VERSION) {
    throw new DriftInputError(
      `drift configuration schema_version must be ${CONFIG_SCHEMA_VERSION}`,
    );
  }
  const docs = value.docs;
  if (
    !isRecord(docs) ||
    !isStringArray(docs.include) ||
    !isStringArray(docs.exclude) ||
    !isStringArray(value.completed_tickets) ||
    !isStringArray(value.mention_only_docs) ||
    !isStringArray(value.path_roots) ||
    !isStringArray(value.require_documented_binaries) ||
    !isRecord(value.cargo_packages) ||
    !Object.values(value.cargo_packages).every(
      (binary) => typeof binary === "string",
    ) ||
    !Array.isArray(value.allow)
  ) {
    throw new DriftInputError("drift configuration is missing required fields");
  }
  for (const entry of value.allow) {
    if (
      !isRecord(entry) ||
      typeof entry.kind !== "string" ||
      !findingKinds.has(entry.kind) ||
      typeof entry.reference !== "string" ||
      entry.reference.length === 0 ||
      (entry.doc !== undefined && typeof entry.doc !== "string") ||
      typeof entry.reason !== "string" ||
      entry.reason.trim().length === 0
    ) {
      throw new DriftInputError(
        `allowlist entry needs a known kind, a reference, and a non-empty reason: ${JSON.stringify(entry)}`,
      );
    }
  }
  return value as DriftConfig;
}

type CommandNode = {
  command: InventoryCommand;
  children: Map<string, CommandNode>;
};

type Surface = {
  roots: Map<string, CommandNode>;
  tools: Set<string>;
  configKeys: Set<string>;
  configSections: Set<string>;
  crateIdentifiers: Set<string>;
  cargoPackages: Record<string, string>;
};

function buildSurface(inventory: Inventory, config: DriftConfig): Surface {
  const roots = new Map<string, CommandNode>();
  for (const binary of inventory.binaries) {
    const sorted = [...binary.commands].sort(
      (left, right) => left.path.length - right.path.length,
    );
    for (const command of sorted) {
      if (command.path.length === 1) {
        roots.set(command.path[0], { command, children: new Map() });
        continue;
      }
      let node = roots.get(command.path[0]);
      for (const segment of command.path.slice(1, -1)) {
        node = node?.children.get(segment);
      }
      if (!node) {
        throw new DriftInputError(
          `surface inventory lists ${command.path.join(" ")} without its parent`,
        );
      }
      node.children.set(command.path.at(-1) as string, {
        command,
        children: new Map(),
      });
    }
  }
  return {
    roots,
    tools: new Set(inventory.mcp_tools),
    configKeys: new Set(inventory.config_keys),
    configSections: new Set(
      inventory.config_keys.map((key) => key.split(".")[0]),
    ),
    crateIdentifiers: new Set(),
    cargoPackages: config.cargo_packages,
  };
}

type Context = {
  surface: Surface;
  doc: string;
  findings: Finding[];
  mentionedCommands: Set<string>;
  mentionedTools: Set<string>;
};

function tokenize(text: string) {
  return text.match(/"[^"]*"|'[^']*'|\S+/gu) ?? [];
}

function isWord(token: string) {
  return /^[a-z][a-z0-9-]*$/u.test(token);
}

function binaryNameOf(token: string, surface: Surface) {
  const name = token.split("/").at(-1) as string;
  if (!surface.roots.has(name)) {
    return null;
  }
  // `compresso/` and `rust/compresso/x` are paths, not invocations.
  return token === name || /^[~.$A-Za-z0-9_{}/-]+\/[a-z-]+$/u.test(token)
    ? name
    : null;
}

/** Resolves `cargo run -p <package> [--bin <name>] -- <args>` to a binary. */
function cargoInvocation(tokens: string[], start: number, surface: Surface) {
  if (tokens[start] !== "cargo" || tokens[start + 1] !== "run") {
    return null;
  }
  let binary: string | null = null;
  let packageName: string | null = null;
  for (let index = start + 2; index < tokens.length; index += 1) {
    const token = tokens[index];
    if (shellSeparators.has(token)) {
      return null;
    }
    if (token === "--bin") {
      binary = tokens[index + 1] ?? null;
    } else if (token === "-p" || token === "--package") {
      packageName = tokens[index + 1] ?? null;
    } else if (token === "--") {
      const name =
        binary ??
        (packageName === null
          ? null
          : (surface.cargoPackages[packageName] ?? packageName));
      return name !== null && surface.roots.has(name)
        ? { name, argumentsStart: index + 1 }
        : null;
    }
  }
  return null;
}

function checkInvocation(
  context: Context,
  line: number,
  binary: string,
  tokens: string[],
  start: number,
) {
  let node = context.surface.roots.get(binary) as CommandNode;
  let index = start;
  while (index < tokens.length && isWord(tokens[index])) {
    const child = node.children.get(tokens[index]);
    if (!child) {
      break;
    }
    node = child;
    index += 1;
  }
  const path = node.command.path;
  for (let depth = 1; depth <= path.length; depth += 1) {
    context.mentionedCommands.add(path.slice(0, depth).join(" "));
  }

  // `compresso account login|status|logout` names several subcommands.
  const next = tokens[index];
  const alternatives = next?.split("|") ?? [];
  if (node.command.has_subcommands && alternatives.every(isWord)) {
    let unknown = false;
    for (const name of alternatives) {
      if (node.children.has(name)) {
        context.mentionedCommands.add(`${path.join(" ")} ${name}`);
      } else if (name !== "help") {
        unknown = true;
        context.findings.push({
          kind: "unknown_cli_command",
          doc: context.doc,
          line,
          reference: `${path.join(" ")} ${name}`,
          message: `\`${path.join(" ")}\` has no subcommand \`${name}\``,
        });
      }
    }
    if (unknown || alternatives.length > 1) {
      return;
    }
  }

  const flags = new Set(node.command.flags);
  for (; index < tokens.length; index += 1) {
    const token = tokens[index];
    if (token === "--" || shellSeparators.has(token)) {
      return;
    }
    const match = /^(--[A-Za-z][A-Za-z0-9-]*|-[A-Za-z])(?:=.*)?$/u.exec(token);
    if (match && !flags.has(match[1])) {
      context.findings.push({
        kind: "unknown_cli_flag",
        doc: context.doc,
        line,
        reference: `${path.join(" ")} ${match[1]}`,
        message: `\`${path.join(" ")}\` does not accept \`${match[1]}\``,
      });
    }
  }
}

function checkCommands(context: Context, line: number, text: string) {
  const tokens = tokenize(text);
  for (let index = 0; index < tokens.length; index += 1) {
    const cargo = cargoInvocation(tokens, index, context.surface);
    if (cargo) {
      checkInvocation(context, line, cargo.name, tokens, cargo.argumentsStart);
      return;
    }
    const previous = tokens[index - 1];
    const binary = binaryNameOf(tokens[index], context.surface);
    // `-p compresso-eval` and `--bin compresso` name a build target.
    if (
      binary !== null &&
      previous !== "-p" &&
      previous !== "--package" &&
      previous !== "--bin"
    ) {
      checkInvocation(context, line, binary, tokens, index + 1);
      return;
    }
  }
}

function checkTools(context: Context, line: number, text: string) {
  for (const match of text.matchAll(
    /(?<![A-Za-z0-9_/.-])compresso_[a-z0-9_]+/gu,
  )) {
    const name = match[0];
    const after = text.slice((match.index as number) + name.length);
    if (/^(?:::|\.[a-z]|\/|-|\*)/u.test(after)) {
      continue;
    }
    // A registered tool can share its name with a crate identifier, so the
    // tool check comes first.
    if (context.surface.tools.has(name)) {
      context.mentionedTools.add(name);
    } else if (context.surface.crateIdentifiers.has(name)) {
      continue;
    } else {
      context.findings.push({
        kind: "unknown_mcp_tool",
        doc: context.doc,
        line,
        reference: name,
        message: `\`${name}\` is not a registered MCP tool`,
      });
    }
  }
}

function checkConfigKey(context: Context, line: number, span: string) {
  const match = /^([a-z_]+)\.([a-z0-9_]+)(?:\s*=.*)?$/u.exec(span.trim());
  if (!match || !context.surface.configSections.has(match[1])) {
    return;
  }
  const key = `${match[1]}.${match[2]}`;
  if (context.surface.configKeys.has(key) || fileExtensions.has(match[2])) {
    return;
  }
  context.findings.push({
    kind: "unknown_config_key",
    doc: context.doc,
    line,
    reference: key,
    message: `\`${key}\` is not a configuration key`,
  });
}

type PathChecker = {
  root: string;
  files: string[];
  pathRoots: Set<string>;
};

function hasPlaceholder(path: string) {
  return /[<>{}$…]|\.\.\./u.test(path);
}

async function pathExists(checker: PathChecker, path: string) {
  const normalized = posix.normalize(path).replace(/\/$/u, "");
  if (normalized.startsWith("..")) {
    return true;
  }
  if (normalized.includes("*") || normalized.includes("?")) {
    const pattern = globToRegExp(normalized);
    const directory = globToRegExp(`${normalized}/**`);
    return checker.files.some(
      (file) => pattern.test(file) || directory.test(file),
    );
  }
  return exists(join(checker.root, normalized));
}

async function checkSpanPath(
  context: Context,
  checker: PathChecker,
  line: number,
  span: string,
) {
  const candidate = span
    .trim()
    .replace(/:\d+(?:[-:]\d+)?$/u, "")
    .replace(/[.,;:]$/u, "");
  if (
    !candidate.includes("/") ||
    !/^[A-Za-z0-9_.@*?/-]+$/u.test(candidate) ||
    hasPlaceholder(candidate) ||
    !checker.pathRoots.has(candidate.split("/")[0])
  ) {
    return;
  }
  if (!(await pathExists(checker, candidate))) {
    context.findings.push({
      kind: "missing_path",
      doc: context.doc,
      line,
      reference: candidate,
      message: `\`${candidate}\` does not exist in the repository`,
    });
  }
}

async function checkLinks(
  context: Context,
  checker: PathChecker,
  line: number,
  text: string,
) {
  for (const match of text.matchAll(/\]\(([^)\s]+)(?:\s+"[^"]*")?\)/gu)) {
    const target = match[1].split("#")[0].split("?")[0];
    if (
      target === "" ||
      /^[a-z][a-z0-9+.-]*:/iu.test(target) ||
      hasPlaceholder(target)
    ) {
      continue;
    }
    const resolved = target.startsWith("/")
      ? target.slice(1)
      : posix.join(posix.dirname(context.doc), target);
    if (!(await pathExists(checker, resolved))) {
      context.findings.push({
        kind: "missing_path",
        doc: context.doc,
        line,
        reference: target,
        message: `link target \`${target}\` does not exist`,
      });
    }
  }
}

function codeSpans(line: string) {
  return [...line.matchAll(/(`+)([^`]|[^`].*?[^`])\1(?!`)/gu)].map(
    (match) => match[2],
  );
}

async function scanDocument(
  context: Context,
  checker: PathChecker,
  text: string,
) {
  const lines = text.split(/\r?\n/u);
  let fence: { marker: string; language: string } | null = null;
  let continued = "";
  let continuedLine = 0;

  for (let index = 0; index < lines.length; index += 1) {
    const raw = lines[index];
    const number = index + 1;
    const fenceMatch = /^\s*(`{3,}|~{3,})\s*([A-Za-z0-9_+-]*)/u.exec(raw);
    if (fenceMatch) {
      if (fence === null) {
        fence = {
          marker: fenceMatch[1][0],
          language: fenceMatch[2].toLowerCase(),
        };
      } else if (fenceMatch[1][0] === fence.marker) {
        fence = null;
      }
      continued = "";
      continue;
    }

    if (fence !== null) {
      checkTools(context, number, raw);
      if (!shellFenceLanguages.has(fence.language)) {
        continue;
      }
      // Join backslash continuations so flags stay with their command.
      if (continued === "") {
        continuedLine = number;
      }
      const body = raw.trim().replace(/^[$>]\s+/u, "");
      if (body.endsWith("\\")) {
        continued += `${body.slice(0, -1)} `;
        continue;
      }
      checkCommands(context, continuedLine, continued + body);
      continued = "";
      continue;
    }

    await checkLinks(context, checker, number, raw);
    const spans = codeSpans(raw);
    let prose = raw;
    for (const span of spans) {
      prose = prose.replace(span, " ");
      checkCommands(context, number, span);
      checkTools(context, number, span);
      checkConfigKey(context, number, span);
      await checkSpanPath(context, checker, number, span);
    }
    checkTools(context, number, prose);
  }
}

/** Expands the first `{a,b}` group of a path, recursively. */
function expandBraces(path: string): string[] {
  const match = /\{([^{}]*)\}/u.exec(path);
  if (!match) {
    return [path];
  }
  return match[1]
    .split(",")
    .flatMap((option) =>
      expandBraces(
        path.slice(0, match.index) +
          option.trim() +
          path.slice(match.index + match[0].length),
      ),
    );
}

/** Splits a ticket `Files:` value into candidate paths. */
function ticketPaths(value: string) {
  const items: string[] = [];
  let depth = 0;
  let current = "";
  for (const char of value.replace(/\([^)]*\)/gu, "")) {
    if (char === "{") {
      depth += 1;
    } else if (char === "}") {
      depth -= 1;
    }
    if (char === "," && depth === 0) {
      items.push(current);
      current = "";
    } else {
      current += char;
    }
  }
  items.push(current);
  return items.flatMap((item) => {
    // "tests in rust/x/tests/a.rs" names the path in its last word.
    const word = item.trim().split(/\s+/u).at(-1) ?? "";
    return expandBraces(word.replace(/^`|`$/gu, ""));
  });
}

/** Checks the `Files:` line of every ticket marked `[x]`. */
async function scanCompletedTickets(
  context: Context,
  checker: PathChecker,
  text: string,
) {
  const lines = text.split(/\r?\n/u);
  let ticket: string | null = null;
  for (let index = 0; index < lines.length; index += 1) {
    const heading = /^#{2,4}\s+\[([ xX])\]\s+([A-Za-z0-9-]+):/u.exec(
      lines[index],
    );
    if (heading) {
      ticket = heading[1] === " " ? null : heading[2];
      continue;
    }
    if (/^#{1,4}\s/u.test(lines[index])) {
      ticket = null;
      continue;
    }
    if (ticket === null || !lines[index].startsWith("Files:")) {
      continue;
    }
    for (const path of ticketPaths(lines[index].slice("Files:".length))) {
      // Bare names and crate-relative shorthand (`benches/startup.rs`) are
      // ambiguous, so only repository-rooted paths are checked.
      if (
        hasPlaceholder(path) ||
        !path.includes("/") ||
        !checker.pathRoots.has(path.split("/")[0])
      ) {
        continue;
      }
      if (!(await pathExists(checker, path))) {
        context.findings.push({
          kind: "missing_ticket_file",
          doc: context.doc,
          line: index + 1,
          reference: `${ticket}:${path}`,
          message: `completed ticket ${ticket} lists \`${path}\`, which does not exist`,
        });
      }
    }
    ticket = null;
  }
}

function compareFindings(left: Finding, right: Finding) {
  return (
    left.doc.localeCompare(right.doc) ||
    left.line - right.line ||
    left.kind.localeCompare(right.kind) ||
    left.reference.localeCompare(right.reference)
  );
}

export async function lintRepository(
  root: string,
  inventory: Inventory,
  config: DriftConfig,
): Promise<DriftReport> {
  const surface = buildSurface(inventory, config);
  const files = await listFiles(root);
  for (const file of files) {
    const match = /^rust\/([^/]+)\/Cargo\.toml$/u.exec(file);
    if (match) {
      surface.crateIdentifiers.add(match[1].replaceAll("-", "_"));
    }
  }
  const checker: PathChecker = {
    root,
    files,
    pathRoots: new Set(config.path_roots),
  };
  const include = config.docs.include.map(globToRegExp);
  const exclude = config.docs.exclude.map(globToRegExp);
  const docs = files.filter(
    (file) =>
      include.some((pattern) => pattern.test(file)) &&
      !exclude.some((pattern) => pattern.test(file)),
  );

  const findings: Finding[] = [];
  const mentionedCommands = new Set<string>();
  const mentionedTools = new Set<string>();
  const contextFor = (doc: string): Context => ({
    surface,
    doc,
    findings,
    mentionedCommands,
    mentionedTools,
  });

  for (const doc of docs) {
    const text = await readFile(join(root, doc), "utf8");
    await scanDocument(contextFor(doc), checker, text);
  }
  for (const doc of config.mention_only_docs) {
    // Contract documents count as documentation of a command or tool, but
    // describe future surface too, so their own references are not findings.
    let text: string;
    try {
      text = await readFile(join(root, doc), "utf8");
    } catch {
      throw new DriftInputError(`mention-only document ${doc} is missing`);
    }
    await scanDocument({ ...contextFor(doc), findings: [] }, checker, text);
  }
  for (const doc of config.completed_tickets) {
    let text: string;
    try {
      text = await readFile(join(root, doc), "utf8");
    } catch {
      throw new DriftInputError(`completed-ticket document ${doc} is missing`);
    }
    await scanCompletedTickets(contextFor(doc), checker, text);
  }

  for (const binary of inventory.binaries) {
    if (!config.require_documented_binaries.includes(binary.name)) {
      continue;
    }
    for (const command of binary.commands) {
      const name = command.path.join(" ");
      if (
        command.path.length > 1 &&
        !command.hidden &&
        !mentionedCommands.has(name)
      ) {
        findings.push({
          kind: "undocumented_cli_command",
          doc: "",
          line: 0,
          reference: name,
          message: `\`${name}\` is not mentioned in any current-state document`,
        });
      }
    }
  }
  for (const tool of inventory.mcp_tools) {
    if (!mentionedTools.has(tool)) {
      findings.push({
        kind: "undocumented_mcp_tool",
        doc: "",
        line: 0,
        reference: tool,
        message: `MCP tool \`${tool}\` is not mentioned in any current-state document`,
      });
    }
  }

  const used = new Set<number>();
  const remaining: Finding[] = [];
  const allowlisted: AllowedFinding[] = [];
  for (const finding of findings) {
    const index = config.allow.findIndex(
      (entry) =>
        entry.kind === finding.kind &&
        entry.reference === finding.reference &&
        (entry.doc === undefined || entry.doc === finding.doc),
    );
    if (index === -1) {
      remaining.push(finding);
    } else {
      used.add(index);
      allowlisted.push({ ...finding, reason: config.allow[index].reason });
    }
  }
  config.allow.forEach((entry, index) => {
    if (!used.has(index)) {
      remaining.push({
        kind: "stale_allowlist_entry",
        doc: entry.doc ?? "",
        line: 0,
        reference: `${entry.kind}:${entry.reference}`,
        message:
          "allowlist entry no longer matches any finding; remove it so the allowlist cannot hide future drift",
      });
    }
  });

  remaining.sort(compareFindings);
  allowlisted.sort(compareFindings);
  const counts: Record<string, number> = {};
  for (const finding of remaining) {
    counts[finding.kind] = (counts[finding.kind] ?? 0) + 1;
  }
  return {
    schema_version: REPORT_SCHEMA_VERSION,
    passed: remaining.length === 0,
    docs_scanned: docs.length,
    counts,
    findings: remaining,
    allowlisted,
  };
}

export function formatReport(report: DriftReport) {
  const lines = report.findings.map((finding) => {
    const location =
      finding.doc === ""
        ? "(surface)"
        : finding.line === 0
          ? finding.doc
          : `${finding.doc}:${finding.line}`;
    return `${location}: ${finding.kind}: ${finding.message}`;
  });
  lines.push(
    `doc-drift-lint: ${report.docs_scanned} documents, ${report.findings.length} findings, ${report.allowlisted.length} allowlisted`,
  );
  return lines.join("\n");
}

type Options = {
  root: string;
  inventory: string;
  config: string;
  json: boolean;
};

const usage =
  "Usage: doc-drift-lint.ts --inventory <surface.json> [--root <repo>] [--config <file>] [--json]";

function parseArguments(argv: string[]): Options | null {
  const scriptDirectory = dirname(fileURLToPath(import.meta.url));
  const options: Options = {
    root: process.cwd(),
    inventory: "",
    config: join(scriptDirectory, "doc-drift-lint.config.json"),
    json: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--help" || argument === "-h") {
      return null;
    }
    if (argument === "--json") {
      options.json = true;
      continue;
    }
    const value = argv[index + 1];
    if (
      (argument === "--root" ||
        argument === "--inventory" ||
        argument === "--config") &&
      value !== undefined
    ) {
      options[argument.slice(2) as "root" | "inventory" | "config"] = value;
      index += 1;
      continue;
    }
    throw new DriftInputError(`unexpected argument ${argument}\n${usage}`);
  }
  if (options.inventory === "") {
    throw new DriftInputError(`--inventory is required\n${usage}`);
  }
  return options;
}

/** Runs the command-line entry point and returns the process exit code. */
export async function main(
  argv: string[],
  write: (text: string) => void,
  writeError: (text: string) => void,
) {
  try {
    const options = parseArguments(argv);
    if (options === null) {
      write(usage);
      return 0;
    }
    const read = async (path: string, label: string) => {
      try {
        return await readFile(path, "utf8");
      } catch (error) {
        throw new DriftInputError(
          `failed to read ${label} ${path}: ${(error as Error).message}`,
        );
      }
    };
    const inventory = parseInventory(
      await read(options.inventory, "surface inventory"),
    );
    const config = parseConfig(await read(options.config, "configuration"));
    const report = await lintRepository(
      resolve(options.root),
      inventory,
      config,
    );
    write(options.json ? JSON.stringify(report) : formatReport(report));
    return report.passed ? 0 : 1;
  } catch (error) {
    if (!(error instanceof DriftInputError)) {
      throw error;
    }
    writeError(`${basename(fileURLToPath(import.meta.url))}: ${error.message}`);
    return 2;
  }
}

const invokedPath = process.argv[1]
  ? pathToFileURL(resolve(process.argv[1])).href
  : "";
if (import.meta.url === invokedPath) {
  process.exitCode = await main(
    process.argv.slice(2),
    (text) => console.log(text),
    (text) => console.error(text),
  );
}
