import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { type TestContext, test } from "node:test";

import {
  type DriftConfig,
  DriftInputError,
  type Inventory,
  lintRepository,
  main,
  parseConfig,
  parseInventory,
} from "./doc-drift-lint.ts";

function command(path: string, flags: string[] = [], hidden = false) {
  return { path: path.split(" "), hidden, has_subcommands: false, flags };
}

function group(path: string) {
  return { ...command(path), has_subcommands: true };
}

const inventory: Inventory = {
  schema_version: 1,
  binaries: [
    {
      name: "compresso",
      commands: [
        { ...group("compresso"), flags: ["--help", "--version", "-V", "-h"] },
        command("compresso doctor", ["--help", "--json", "-h"]),
        command("compresso read", ["--json", "--max-tokens", "--root"]),
        group("compresso config"),
        command("compresso config explain", ["--json"]),
        group("compresso account"),
        command("compresso account login", ["--json"]),
        command("compresso account logout", ["--json"]),
        command("compresso secret", [], true),
      ],
    },
    {
      name: "compresso-eval",
      commands: [
        group("compresso-eval"),
        command("compresso-eval run", ["--gates", "--json"]),
      ],
    },
  ],
  mcp_tools: ["compresso_read", "compresso_search"],
  config_keys: ["compression.mode", "index.enabled", "runtime.log"],
};

const everything = [
  "`compresso doctor`, `compresso read`, `compresso config explain`,",
  "`compresso account login|logout`, `compresso_read`, `compresso_search`.",
  "",
].join("\n");

function config(overrides: Partial<DriftConfig> = {}): DriftConfig {
  return {
    schema_version: 1,
    docs: { include: ["README.md", "docs/**/*.md"], exclude: ["docs/plan/**"] },
    completed_tickets: [],
    mention_only_docs: [],
    path_roots: ["docs", "rust", "scripts"],
    require_documented_binaries: ["compresso"],
    cargo_packages: { "compresso-cli": "compresso" },
    allow: [],
    ...overrides,
  };
}

async function repository(t: TestContext, files: Record<string, string>) {
  const root = await mkdtemp(join(tmpdir(), "doc-drift-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  for (const [path, content] of Object.entries(files)) {
    await mkdir(dirname(join(root, path)), { recursive: true });
    await writeFile(join(root, path), content);
  }
  return { root };
}

function summarize(findings: { kind: string; reference: string }[]) {
  return findings.map((finding) => `${finding.kind} ${finding.reference}`);
}

test("test_drift_lint_passes_when_docs_match_surface", async (t) => {
  const repo = await repository(t, {
    "README.md": [
      everything,
      "```sh",
      "$ compresso read src/lib.rs --max-tokens 200 \\",
      "    --json | jq -e '.content'",
      "cargo run -p compresso-eval --release -- run fixtures/ --gates g.toml",
      "cargo run -p compresso-cli -- doctor --json",
      "compresso --version",
      "```",
      "Set `compression.mode = safe` or `runtime.log`; see `index.rs`.",
      "See [conventions](docs/conventions.md#errors) and `docs/*.md`.",
    ].join("\n"),
    "docs/conventions.md": "Back to [the readme](../README.md).\n",
  });

  const report = await lintRepository(repo.root, inventory, config());

  assert.deepEqual(report.findings, []);
  assert.equal(report.passed, true);
  assert.equal(report.docs_scanned, 2);
  assert.equal(report.schema_version, 1);
});

test("test_drift_lint_reports_unknown_cli_command_and_flag", async (t) => {
  const repo = await repository(t, {
    "README.md": [
      everything,
      "Run `compresso doctor --fix` or `compresso repair`.",
      "Use `compresso config set` and `compresso account login|whoami`.",
      "```bash",
      "compresso read a.rs \\",
      "  --budget 10",
      "cargo run -p compresso-eval -- run fixtures/ --strict",
      "~/.local/bin/compresso doctor -x",
      "```",
    ].join("\n"),
  });

  const report = await lintRepository(repo.root, inventory, config());

  assert.equal(report.passed, false);
  assert.deepEqual(summarize(report.findings), [
    "unknown_cli_command compresso repair",
    "unknown_cli_flag compresso doctor --fix",
    "unknown_cli_command compresso account whoami",
    "unknown_cli_command compresso config set",
    "unknown_cli_flag compresso read --budget",
    "unknown_cli_flag compresso-eval run --strict",
    "unknown_cli_flag compresso doctor -x",
  ]);
  assert.deepEqual(
    report.findings.map((finding) => finding.line),
    [4, 4, 5, 5, 7, 9, 10],
    "a continued shell command is reported on its first line",
  );
  assert.equal(report.counts.unknown_cli_flag, 4);
});

test("test_drift_lint_ignores_flags_of_other_programs", async (t) => {
  const repo = await repository(t, {
    "README.md": [
      everything,
      "Run `cargo clippy --workspace --all-targets -- -D warnings`.",
      "Build with `cargo build --release -p compresso-cli`.",
      "The archive is `compresso-0.1.0-aarch64-apple-darwin.tar.gz`.",
      "```sh",
      "compresso doctor --json | jq --raw-output .status",
      "compresso doctor && other --unknown",
      "```",
      "```rust",
      "compresso frobnicate --nope",
      "```",
    ].join("\n"),
  });

  const report = await lintRepository(repo.root, inventory, config());

  assert.deepEqual(report.findings, []);
});

test("test_drift_lint_reports_unknown_mcp_tool_but_not_crate_identifiers", async (t) => {
  const repo = await repository(t, {
    "rust/compresso-core/Cargo.toml": "[package]\n",
    "README.md": [
      everything,
      "Call `compresso_edit` or compresso_teleport in prose.",
      "Rust paths `compresso_core::Config`, `compresso_core`, and",
      "`compresso_eval::run` plus `target/compresso_tool.rs` are not tools.",
      "```json",
      '{"name": "compresso_delete"}',
      "```",
    ].join("\n"),
  });

  const report = await lintRepository(repo.root, inventory, config());

  assert.deepEqual(summarize(report.findings), [
    "unknown_mcp_tool compresso_edit",
    "unknown_mcp_tool compresso_teleport",
    "unknown_mcp_tool compresso_delete",
  ]);
});

test("test_drift_lint_counts_a_tool_that_shares_a_crate_identifier", async (t) => {
  const repo = await repository(t, {
    "rust/compresso-search/Cargo.toml": "[package]\n",
    "README.md": [everything, "Rust path `compresso_search::run` is a crate."].join(
      "\n",
    ),
  });

  const report = await lintRepository(repo.root, inventory, config());

  assert.deepEqual(summarize(report.findings), []);
});

test("test_drift_lint_reports_unknown_config_key_only_in_known_sections", async (t) => {
  const repo = await repository(t, {
    "README.md": [
      everything,
      "`compression.level`, `index.depth = 3`, `index.rs`, `runtime.log`,",
      "`other.section`, and `serde.workspace` are mentioned.",
    ].join("\n"),
  });

  const report = await lintRepository(repo.root, inventory, config());

  assert.deepEqual(summarize(report.findings), [
    "unknown_config_key compression.level",
    "unknown_config_key index.depth",
  ]);
});

test("test_drift_lint_reports_missing_paths_and_links", async (t) => {
  const repo = await repository(t, {
    "rust/compresso-core/src/lib.rs": "",
    "docs/guide.md": [
      everything,
      "`rust/compresso-core/src/lib.rs:12`, `rust/compresso-core/`,",
      "`rust/compresso-core/src/*.rs`, and `rust/<crate>/src/lib.rs` exist.",
      "`rust/compresso-core/src/gone.rs` and `rust/compresso-*/src/*.py` do not.",
      "`target/release/compresso` and `other/place.rs` are outside path roots.",
      "[ok](../rust/compresso-core/src/lib.rs) [web](https://example.com/x)",
      "[anchor](#top) [gone](./missing.md) [root](/docs/absent.md)",
    ].join("\n"),
  });

  const report = await lintRepository(repo.root, inventory, config());

  assert.deepEqual(summarize(report.findings), [
    "missing_path rust/compresso-*/src/*.py",
    "missing_path rust/compresso-core/src/gone.rs",
    "missing_path ./missing.md",
    "missing_path /docs/absent.md",
  ]);
});

test("test_drift_lint_reports_undocumented_commands_and_tools", async (t) => {
  const repo = await repository(t, {
    "README.md": "Only `compresso doctor` and `compresso_read` are here.\n",
    "docs/plan/contract.md": [
      "`compresso account login|logout` and `compresso config explain`.",
      "Future: `compresso teleport --fast` and `compresso_future`.",
    ].join("\n"),
  });

  const strict = await lintRepository(repo.root, inventory, config());
  assert.deepEqual(summarize(strict.findings), [
    "undocumented_cli_command compresso account",
    "undocumented_cli_command compresso account login",
    "undocumented_cli_command compresso account logout",
    "undocumented_cli_command compresso config",
    "undocumented_cli_command compresso config explain",
    "undocumented_cli_command compresso read",
    "undocumented_mcp_tool compresso_search",
  ]);
  assert.ok(
    strict.findings.every(
      (finding) => finding.doc === "" && finding.line === 0,
    ),
  );

  const withContract = await lintRepository(
    repo.root,
    inventory,
    config({ mention_only_docs: ["docs/plan/contract.md"] }),
  );
  assert.deepEqual(
    summarize(withContract.findings),
    [
      "undocumented_cli_command compresso read",
      "undocumented_mcp_tool compresso_search",
    ],
    "a contract document documents surface without being linted itself",
  );
});

test("test_drift_lint_checks_files_of_completed_tickets_only", async (t) => {
  const repo = await repository(t, {
    "README.md": everything,
    "rust/compresso-core/src/graph.rs": "",
    "rust/compresso-core/src/engine.rs": "",
    "docs/plan/tickets.md": [
      "## E1 — Foundation",
      "### [x] E1-01: Done ticket",
      "Owner: R1 · Estimate: 1d · Depends: —",
      "Files: rust/compresso-core/src/{graph,engine,tokens}.rs, rust/compresso-core/tests/ (new), tests in rust/compresso-core/src/gone.rs, benches/startup.rs, sample.tsx, rust/<crate>/x.rs",
      "Description: `compresso teleport` is described here but not linted.",
      "### [ ] E1-02: Open ticket",
      "Files: rust/compresso-future/src/lib.rs, scripts/plan-lint.ts",
    ].join("\n"),
  });

  const report = await lintRepository(
    repo.root,
    inventory,
    config({ completed_tickets: ["docs/plan/tickets.md"] }),
  );

  assert.deepEqual(summarize(report.findings), [
    "missing_ticket_file E1-01:rust/compresso-core/src/gone.rs",
    "missing_ticket_file E1-01:rust/compresso-core/src/tokens.rs",
    "missing_ticket_file E1-01:rust/compresso-core/tests/",
  ]);
  assert.ok(report.findings.every((finding) => finding.line === 4));
});

test("test_drift_lint_allowlist_suppresses_matches_and_flags_stale_entries", async (t) => {
  const repo = await repository(t, {
    "README.md": [everything, "Planned: `compresso teleport`."].join("\n"),
    "docs/other.md": "Also `compresso teleport` here.\n",
  });
  const planned = {
    kind: "unknown_cli_command",
    reference: "compresso teleport",
    reason: "Planned in a future ticket.",
  };

  const scoped = await lintRepository(
    repo.root,
    inventory,
    config({ allow: [{ ...planned, doc: "README.md" }] }),
  );
  assert.deepEqual(summarize(scoped.findings), [
    "unknown_cli_command compresso teleport",
  ]);
  assert.equal(scoped.findings[0].doc, "docs/other.md");
  assert.equal(scoped.allowlisted.length, 1);
  assert.equal(scoped.allowlisted[0].reason, "Planned in a future ticket.");

  const stale = await lintRepository(
    repo.root,
    inventory,
    config({
      allow: [
        planned,
        {
          kind: "unknown_mcp_tool",
          reference: "compresso_shipped",
          reason: "Was planned.",
        },
      ],
    }),
  );
  assert.equal(stale.passed, false);
  assert.deepEqual(summarize(stale.findings), [
    "stale_allowlist_entry unknown_mcp_tool:compresso_shipped",
  ]);
  assert.equal(stale.allowlisted.length, 2);
});

test("test_drift_lint_rejects_malformed_inventory_and_config", () => {
  assert.throws(
    () => parseInventory(JSON.stringify({ ...inventory, schema_version: 2 })),
    DriftInputError,
  );
  assert.throws(() => parseInventory("{"), DriftInputError);
  assert.throws(
    () => parseInventory(JSON.stringify({ schema_version: 1, binaries: [] })),
    DriftInputError,
  );
  assert.equal(
    parseInventory(JSON.stringify(inventory)).binaries.length,
    inventory.binaries.length,
  );

  const allow = (entry: Record<string, unknown>) =>
    JSON.stringify(config({ allow: [entry as never] }));
  for (const entry of [
    { kind: "unknown_cli_command", reference: "compresso x" },
    { kind: "unknown_cli_command", reference: "compresso x", reason: "  " },
    { kind: "typo", reference: "compresso x", reason: "why" },
    { kind: "stale_allowlist_entry", reference: "x", reason: "why" },
  ]) {
    assert.throws(() => parseConfig(allow(entry)), DriftInputError);
  }
  assert.throws(
    () => parseConfig(JSON.stringify({ ...config(), schema_version: 9 })),
    DriftInputError,
  );
});

test("test_drift_lint_repository_config_is_valid", async (t) => {
  const text = await readFile(
    join(import.meta.dirname, "doc-drift-lint.config.json"),
    "utf8",
  );
  const parsed = parseConfig(text);

  assert.ok(parsed.docs.exclude.includes("docs/plan/**"));
  assert.ok(parsed.require_documented_binaries.includes("compresso"));
});

test("test_drift_lint_cli_emits_json_and_exit_codes", async (t) => {
  const repo = await repository(t, {
    "README.md": everything,
    "inventory.json": JSON.stringify(inventory),
    "config.json": JSON.stringify(config()),
    "broken.json": JSON.stringify({ schema_version: 7 }),
  });
  const run = async (...argv: string[]) => {
    const output: string[] = [];
    const errors: string[] = [];
    const code = await main(
      argv,
      (text) => output.push(text),
      (text) => errors.push(text),
    );
    return { code, output: output.join("\n"), errors: errors.join("\n") };
  };
  const base = [
    "--root",
    repo.root,
    "--inventory",
    join(repo.root, "inventory.json"),
    "--config",
    join(repo.root, "config.json"),
  ];

  const clean = await run(...base, "--json");
  assert.equal(clean.code, 0);
  assert.deepEqual(JSON.parse(clean.output), {
    schema_version: 1,
    passed: true,
    docs_scanned: 1,
    counts: {},
    findings: [],
    allowlisted: [],
  });

  await writeFile(join(repo.root, "README.md"), "`compresso nope`\n");
  const drift = await run(...base, "--json");
  assert.equal(drift.code, 1);
  const report = JSON.parse(drift.output);
  assert.equal(report.passed, false);
  assert.deepEqual(report.findings.at(-1), {
    kind: "unknown_cli_command",
    doc: "README.md",
    line: 1,
    reference: "compresso nope",
    message: "`compresso` has no subcommand `nope`",
  });

  const human = await run(...base);
  assert.equal(human.code, 1);
  assert.match(human.output, /^README\.md:1: unknown_cli_command: /mu);
  assert.match(human.output, /\(surface\): undocumented_mcp_tool: /u);

  const badInventory = await run(
    "--root",
    repo.root,
    "--inventory",
    join(repo.root, "broken.json"),
  );
  assert.equal(badInventory.code, 2);
  assert.match(badInventory.errors, /schema_version must be 1/u);
  assert.equal((await run("--root", repo.root)).code, 2);
  assert.equal((await run("--bogus")).code, 2);
  assert.equal((await run("--help")).code, 0);
});
