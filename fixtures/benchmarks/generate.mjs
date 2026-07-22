#!/usr/bin/env node

import { createHash } from "node:crypto";
import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const fixturesRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const benchmarkRoot = join(fixturesRoot, "benchmarks");

async function write(relativePath, content) {
  const path = join(fixturesRoot, relativePath);
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, content, "utf8");
}

function graphNode(id, kind, content, priority, protectedNode, recency) {
  return {
    id,
    kind,
    content,
    source: { type: "synthetic" },
    token_count: 0,
    priority,
    protected: protectedNode,
    recency,
  };
}

function json(value) {
  return `${JSON.stringify(value, null, 2)}\n`;
}

async function generateStackTraceDupes() {
  const nodeTrace = [
    "ERROR Unhandled promise rejection: FetchError: request to https://api.internal.acme.dev/v2/sessions failed, reason: connect ECONNREFUSED 10.0.12.44:443",
    "    at ClientRequest.<anonymous> (/srv/app/node_modules/node-fetch/lib/index.js:1501:11)",
    "    at ClientRequest.emit (node:events:517:28)",
    "    at TLSSocket.socketErrorListener (node:_http_client:501:9)",
    "    at TLSSocket.emit (node:events:517:28)",
    "    at emitErrorNT (node:internal/streams/destroy:151:8)",
    "    at emitErrorCloseNT (node:internal/streams/destroy:116:3)",
    "    at process.processTicksAndRejections (node:internal/process/task_queues:82:21)",
  ];
  const javaTrace = [
    "ERROR [pool-3-thread-7] c.a.billing.InvoiceWorker - Failed to process invoice batch",
    'java.lang.NullPointerException: Cannot invoke "com.acme.billing.Customer.getRegion()" because "customer" is null',
    "    at com.acme.billing.InvoiceWorker.applyTax(InvoiceWorker.java:214)",
    "    at com.acme.billing.InvoiceWorker.process(InvoiceWorker.java:167)",
    "    at com.acme.billing.InvoiceWorker.run(InvoiceWorker.java:88)",
    "    at java.base/java.util.concurrent.ThreadPoolExecutor.runWorker(ThreadPoolExecutor.java:1136)",
    "    at java.base/java.util.concurrent.ThreadPoolExecutor$Worker.run(ThreadPoolExecutor.java:635)",
    "    at java.base/java.lang.Thread.run(Thread.java:840)",
  ];
  const lines = [];
  for (let index = 0; index < 6; index += 1) {
    const second = String(14 + index * 5).padStart(2, "0");
    lines.push(
      `2026-07-21T11:02:${second}.512Z ${nodeTrace[0]}`,
      ...nodeTrace.slice(1),
    );
  }
  for (let index = 0; index < 5; index += 1) {
    const second = String(2 + index * 5).padStart(2, "0");
    lines.push(
      `2026-07-21 11:03:${second}.117 ${javaTrace[0]}`,
      ...javaTrace.slice(1),
    );
  }
  await write("logs/stack-trace-dupes/service.log", `${lines.join("\n")}\n`);
  await write(
    "logs/stack-trace-dupes/fixture.toml",
    `name = "stack-trace-dupes"
category = "logs"
description = "Repeated Node and Java stack traces whose timestamps and worker names vary while root causes remain stable."
eligible = true
mode = "safe"

[budget]
max_tokens = 1500
max_latency_ms = 200

[[input.files]]
path = "service.log"
kind = "tool_response"
node_id = "log-1"
protected = false
priority = 0.4
source_tool = "shell"

[expectations]
min_reduction_pct = 60.0
expect_fallback = false
must_preserve = [
  "FetchError: request to https://api.internal.acme.dev/v2/sessions failed, reason: connect ECONNREFUSED 10.0.12.44:443",
  "java.lang.NullPointerException: Cannot invoke \\"com.acme.billing.Customer.getRegion()\\" because \\"customer\\" is null",
  "at com.acme.billing.InvoiceWorker.applyTax(InvoiceWorker.java:214)",
]

[[recall_probes]]
question = "Where did the Java exception originate?"
answer_substring = "InvoiceWorker.applyTax(InvoiceWorker.java:214)"
protected = true
`,
  );
}

async function generateCargoBuildNoise() {
  const lines = ["$ cargo build --workspace"];
  for (let cycle = 1; cycle <= 12; cycle += 1) {
    lines.push(
      `   Compiling compresso-core v0.1.0 (/workspace/rust/compresso-core) [rebuild ${cycle}]`,
      `   Compiling compresso-atlas v0.1.0 (/workspace/rust/compresso-atlas) [rebuild ${cycle}]`,
      `   Compiling compresso-cli v0.1.0 (/workspace/rust/compresso-cli) [rebuild ${cycle}]`,
      `   Compiling compresso-store v0.1.0 (/workspace/rust/compresso-store) [rebuild ${cycle}]`,
      `   Compiling compresso-eval v0.1.0 (/workspace/rust/compresso-eval) [rebuild ${cycle}]`,
      `   Compiling compresso-mcp v0.1.0 (/workspace/rust/compresso-mcp) [rebuild ${cycle}]`,
      `   Compiling compresso-integrations v0.1.0 (/workspace/rust/compresso-integrations) [rebuild ${cycle}]`,
      `   Compiling compresso-updater v0.1.0 (/workspace/rust/compresso-updater) [rebuild ${cycle}]`,
      "warning: unused variable: `candidate`",
      "  --> rust/compresso-core/src/engine.rs:84:9",
      "   |",
      "84 |     let candidate = registry.estimate(&graph);",
      "   |         ^^^^^^^^^ help: if this is intentional, prefix it with an underscore: `_candidate`",
      "warning: `compresso-core` (lib) generated 1 warning",
      `    Finished dev [unoptimized + debuginfo] target(s) in ${2 + cycle}.0${cycle}s`,
      "",
    );
  }
  lines.push(
    "error[E0599]: no method named `compress` found for struct `Engine` in the current scope",
    "  --> rust/compresso-cli/src/main.rs:42:25",
    "   |",
    "42 |     let result = engine.compress(graph, budget);",
    "   |                         ^^^^^^^^ method not found in `Engine`",
    "   |",
    "   = note: the method is available through the `CompressionEngine` trait",
    'error: could not compile `compresso-cli` (bin "compresso") due to 1 previous error',
  );
  await write("logs/cargo-build-noise/build.log", `${lines.join("\n")}\n`);
  await write(
    "logs/cargo-build-noise/fixture.toml",
    `name = "cargo-build-noise"
category = "logs"
description = "Twelve repetitive Cargo rebuilds followed by one actionable E0599 compiler error."
eligible = true
mode = "safe"

[budget]
max_tokens = 1800
max_latency_ms = 200

[[input.files]]
path = "build.log"
kind = "compiler_output"
node_id = "cargo-build"
protected = false
priority = 0.6
source_tool = "shell"

[expectations]
min_reduction_pct = 40.0
expect_fallback = false
must_preserve = [
  "error[E0599]: no method named \u0060compress\u0060 found for struct \u0060Engine\u0060 in the current scope",
  "42 |     let result = engine.compress(graph, budget);",
  "error: could not compile \u0060compresso-cli\u0060 (bin \\\"compresso\\\") due to 1 previous error",
]

[[recall_probes]]
question = "Which method was missing?"
answer_substring = "no method named \u0060compress\u0060"
protected = true
`,
  );
}

async function generateSupersededDecisions() {
  const graph = {
    schema_version: 1,
    nodes: [
      graphNode(
        "asst-before",
        "assistant_response",
        "I will compare three storage layouts before recording the final decision.",
        0.4,
        false,
        1784628000,
      ),
      graphNode(
        "decision-1",
        "decision",
        "Draft decision: store indexes as one JSON file per repository.",
        0.2,
        false,
        1784628060,
      ),
      graphNode(
        "decision-2",
        "decision",
        "Draft decision: use an embedded key-value store with one global writer.",
        0.3,
        false,
        1784628120,
      ),
      graphNode(
        "decision-3",
        "decision",
        "Draft decision: use SQLite but rebuild every index on startup.",
        0.4,
        false,
        1784628180,
      ),
      graphNode(
        "decision-final",
        "decision",
        "Final decision: use SQLite in WAL mode with versioned migrations and per-file index updates.",
        0.9,
        false,
        1784628240,
      ),
      graphNode(
        "asst-after",
        "assistant_response",
        "The final SQLite WAL decision is now authoritative for implementation.",
        0.8,
        false,
        1784628300,
      ),
    ],
    edges: [
      {
        from: "decision-final",
        to: "decision-1",
        kind: "supersedes",
        weight: 1.0,
      },
      {
        from: "decision-final",
        to: "decision-2",
        kind: "supersedes",
        weight: 1.0,
      },
      {
        from: "decision-final",
        to: "decision-3",
        kind: "supersedes",
        weight: 1.0,
      },
      {
        from: "asst-after",
        to: "decision-final",
        kind: "references",
        weight: 0.8,
      },
    ],
  };
  await write("conversations/superseded-decisions/graph.json", json(graph));
  await write(
    "conversations/superseded-decisions/fixture.toml",
    `name = "superseded-decisions"
category = "conversations"
description = "Three draft architecture decisions superseded by one authoritative SQLite WAL decision."
eligible = true
mode = "safe"

[budget]
max_tokens = 500
max_latency_ms = 200

[input]
graph = "graph.json"

[expectations]
min_reduction_pct = 30.0
expect_fallback = false
must_preserve = [
  "Final decision: use SQLite in WAL mode with versioned migrations and per-file index updates.",
]

[[recall_probes]]
question = "Which storage architecture is authoritative?"
answer_substring = "SQLite in WAL mode with versioned migrations"
protected = true
`,
  );
}

async function generateLongSession() {
  const nodes = [
    graphNode(
      "sys-1",
      "system_instruction",
      "You are a coding assistant. Preserve the active request, tool contracts, and verified failures.",
      1.0,
      true,
      1784630000,
    ),
  ];
  const edges = [];
  const detail =
    "The repository contains deterministic Rust services, TypeScript clients, and Python maintenance scripts. " +
    "Review evidence carefully, keep exact error locations, avoid speculative edits, and report each validation command. ";
  for (let turn = 1; turn <= 20; turn += 1) {
    const userId = `user-${turn}`;
    const assistantId = `assistant-${turn}`;
    nodes.push(
      graphNode(
        userId,
        "user_instruction",
        `Historical request ${turn}: investigate subsystem ${turn} and summarize the relevant implementation constraints. ${detail.repeat(8)}`,
        turn === 20 ? 1.0 : 0.25 + turn / 100,
        true,
        1784630000 + turn * 120,
      ),
      graphNode(
        assistantId,
        "assistant_response",
        `Historical response ${turn}: analysis completed for subsystem ${turn}; tests and boundaries were recorded. ${detail.repeat(8)}${
          turn === 20
            ? " ACTIVE RESULT: the failing boundary is parser_version drift in src/indexer.rs:214."
            : ""
        }`,
        turn === 20 ? 0.95 : 0.2 + turn / 100,
        false,
        1784630060 + turn * 120,
      ),
    );
    edges.push({
      from: assistantId,
      to: userId,
      kind: "responds_to",
      weight: 1.0,
    });
  }
  nodes.push(
    graphNode(
      "req-current",
      "tool_request",
      '{"tool":"read_file","arguments":{"path":"rust/compresso-atlas/src/indexer.rs","line":214}}',
      1.0,
      true,
      1784632600,
    ),
  );
  const graph = { schema_version: 1, nodes, edges };
  await write("conversations/long-session-prune/graph.json", json(graph));
  await write(
    "conversations/long-session-prune/fixture.toml",
    `name = "long-session-prune"
category = "conversations"
description = "A forty-turn coding transcript whose active request, latest result, and in-flight tool request outrank stale history."
eligible = true
mode = "safe"

[budget]
max_tokens = 6000
max_latency_ms = 200

[input]
graph = "graph.json"

[expectations]
min_reduction_pct = 25.0
expect_fallback = false
protected_node_ids = ["sys-1", "user-20", "req-current"]
must_preserve = [
  "ACTIVE RESULT: the failing boundary is parser_version drift in src/indexer.rs:214.",
]

[[recall_probes]]
question = "Where is the active parser-version failure?"
answer_substring = "src/indexer.rs:214"
protected = true
`,
  );
}

async function generateOutlineLargeFile() {
  const lines = [
    "export interface SessionSnapshot {",
    "  sessionId: string;",
    "  observedTokens: number;",
    "  appliedTokens: number;",
    "}",
    "",
    "export function summarizeSession(snapshot: SessionSnapshot): string {",
    "  const saved = snapshot.observedTokens - snapshot.appliedTokens;",
    "  return `${snapshot.sessionId}: ${saved} tokens saved`;",
    "}",
    "",
  ];
  let metric = 1;
  while (lines.length < 900) {
    lines.push(
      `export const sessionMetric${String(metric).padStart(4, "0")}: number = ${metric};`,
    );
    metric += 1;
  }
  if (lines.length !== 900) {
    throw new Error("outline-large-file must contain exactly 900 lines");
  }
  await write("code/outline-large-file/input.ts", `${lines.join("\n")}\n`);
  await write(
    "code/outline-large-file/fixture.toml",
    `name = "outline-large-file"
category = "code"
description = "A 900-line TypeScript source file whose exported API must survive source outlining."
eligible = true
mode = "safe"

[budget]
max_tokens = 1500
max_latency_ms = 200

[[input.files]]
path = "input.ts"
kind = "source_file"
node_id = "large-source"
protected = false
priority = 0.5

[expectations]
min_reduction_pct = 70.0
expect_fallback = false
must_preserve = [
  "export interface SessionSnapshot {",
  "export function summarizeSession(snapshot: SessionSnapshot): string {",
]

[[recall_probes]]
question = "Which function summarizes a session?"
answer_substring = "summarizeSession(snapshot: SessionSnapshot)"
protected = true
`,
  );
}

async function generateCompilerOutput() {
  const lines = [
    "error[E0308]: mismatched types",
    "  --> src/session.rs:74:31",
    "   |",
    "74 |     let timeout: Duration = config.timeout_ms;",
    "   |                  --------   ^^^^^^^^^^^^^^^^^ expected `Duration`, found `u64`",
    "   |                  |",
    "   |                  expected due to this",
    "   |",
    "help: try wrapping the expression in `Duration::from_millis`",
    "   |",
    "74 |     let timeout: Duration = Duration::from_millis(config.timeout_ms);",
    "   |                             +++++++++++++++++++++                 +",
    "",
  ];
  for (let index = 1; index <= 30; index += 1) {
    lines.push(
      `warning: unused variable: \u0060temporary_${index}\u0060`,
      `  --> src/generated/worker_${index}.rs:18:9`,
      "   |",
      `18 |     let temporary_${index} = queue.peek();`,
      `   |         ^^^^^^^^^^^^^ help: prefix it with an underscore: \u0060_temporary_${index}\u0060`,
      "",
    );
  }
  lines.push("error: aborting due to 1 previous error; 30 warnings emitted");
  await write(
    "debugging/compiler-output-rust/input.txt",
    `${lines.join("\n")}\n`,
  );
  await write(
    "debugging/compiler-output-rust/fixture.toml",
    `name = "compiler-output-rust"
category = "debugging"
description = "One actionable Rust type error followed by thirty repetitive generated-code warnings."
eligible = true
mode = "safe"

[budget]
max_tokens = 1600
max_latency_ms = 200

[[input.files]]
path = "input.txt"
kind = "compiler_output"
node_id = "rustc-output"
protected = false
priority = 0.8
source_tool = "shell"

[expectations]
min_reduction_pct = 40.0
expect_fallback = false
must_preserve = [
  "error[E0308]: mismatched types",
  "74 |     let timeout: Duration = config.timeout_ms;",
  "expected \u0060Duration\u0060, found \u0060u64\u0060",
  "Duration::from_millis(config.timeout_ms)",
]

[[recall_probes]]
question = "Which conversion fixes the type mismatch?"
answer_substring = "Duration::from_millis(config.timeout_ms)"
protected = true
`,
  );
}

async function generateAdversarialAssets() {
  await write(
    "adversarial/malformed-json-payload/input.txt",
    '{"request_id":"req-42","items":[{"id":1,"message":"complete"},{"id":2,"message":"truncated tail: KEEP_ME',
  );
  await write(
    "adversarial/malformed-json-payload/fixture.toml",
    `name = "malformed-json-payload"
category = "adversarial"
description = "A tool response containing JSON truncated mid-string; compact_json must leave it byte-exact."
eligible = false
mode = "safe"

[budget]
max_tokens = 500
max_latency_ms = 200

[[input.files]]
path = "input.txt"
kind = "tool_response"
node_id = "malformed-json"
protected = false
priority = 0.6
source_tool = "http"

[expectations]
min_reduction_pct = 0.0
expect_fallback = false
must_preserve = ["truncated tail: KEEP_ME"]
`,
  );

  const fenceBomb = [
    "Analysis started.",
    "```rust",
    "fn one() {}",
    "```typescript",
    "const two = 2;",
    "```python",
    "three = 3",
    "```json",
    '{"four":4}',
    "```text",
    "five",
    "```",
    "first closer",
    "```",
    "second closer",
    "```",
    "third closer leaves two fences open.",
  ].join("\n");
  await write("adversarial/fence-bomb/input.txt", `${fenceBomb}\n`);
  await write(
    "adversarial/fence-bomb/fixture.toml",
    `name = "fence-bomb"
category = "adversarial"
description = "An assistant response with five opening Markdown fences and only three closers."
eligible = false
mode = "safe"

[budget]
max_tokens = 500
max_latency_ms = 200

[[input.files]]
path = "input.txt"
kind = "assistant_response"
node_id = "fence-bomb"
protected = false
priority = 0.5
source_tool = "fixture"

[expectations]
min_reduction_pct = 0.0
expect_fallback = true
must_preserve = ["third closer leaves two fences open."]
`,
  );
}

async function generateOpenAiUnknownFields() {
  const payload = {
    model: "gpt-5-codex",
    messages: [
      {
        role: "system",
        content: "Preserve unknown provider fields byte-identically.",
      },
      { role: "user", content: "Explain the failing boundary test." },
    ],
    reasoning_effort: "low",
    x_vendor_session: "session-opaque-42",
    x_compresso_future: { cache_epoch: 7, preserve_prefix: true },
    stream: false,
  };
  await write(
    "provider-payloads/openai-unknown-fields/input.json",
    json(payload),
  );
  await write(
    "provider-payloads/openai-unknown-fields/fixture.toml",
    `name = "openai-unknown-fields"
category = "provider-payloads"
description = "A Chat Completions request with plausible future and vendor-extension fields that require passthrough."
eligible = false
mode = "passthrough"

[budget]
max_tokens = 1000
max_latency_ms = 200

[[input.files]]
path = "input.json"
kind = "tool_request"
node_id = "openai-request"
protected = true
priority = 1.0
source_tool = "openai"

[expectations]
min_reduction_pct = 0.0
expect_fallback = false
protected_node_ids = ["openai-request"]
must_preserve = [
  "\\\"reasoning_effort\\\": \\\"low\\\"",
  "\\\"x_vendor_session\\\": \\\"session-opaque-42\\\"",
  "\\\"x_compresso_future\\\"",
]
`,
  );
}

async function generateBenchmarks() {
  const elements = Array.from({ length: 2000 }, (_, index) => ({
    id: index,
    key: `fixture-${String(index).padStart(4, "0")}`,
    enabled: index % 3 !== 0,
    tags: `group-${index % 17},shard-${index % 31}`,
    metrics: {
      observed_tokens: 1000 + index * 13,
      applied_tokens: 700 + index * 7,
      latency_ms: 20 + (index % 180),
    },
  }));
  await write("benchmarks/json-2000-elements/input.json", json(elements));

  const targetBytes = 200 * 1024;
  let source =
    "export interface BenchmarkRow {\n  id: number;\n  value: string;\n}\n\nexport const rows: BenchmarkRow[] = [];\n";
  let index = 0;
  while (true) {
    const line = `rows.push({ id: ${index}, value: "deterministic-source-row-${String(index).padStart(6, "0")}" });\n`;
    if (Buffer.byteLength(source) + Buffer.byteLength(line) > targetBytes - 5) {
      break;
    }
    source += line;
    index += 1;
  }
  const remaining = targetBytes - Buffer.byteLength(source);
  if (remaining < 5) {
    throw new Error("source benchmark padding invariant failed");
  }
  source += `/*${"x".repeat(remaining - 5)}*/\n`;
  if (Buffer.byteLength(source) !== targetBytes) {
    throw new Error("source benchmark must be exactly 200 KiB");
  }
  await write("benchmarks/source-200kb/input.ts", source);
}

async function fixtureFiles(directory, root = directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  entries.sort((left, right) => left.name.localeCompare(right.name));
  const files = [];
  for (const entry of entries) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await fixtureFiles(path, root)));
    } else if (entry.isFile()) {
      files.push(relative(root, path).replaceAll("\\", "/"));
    } else {
      throw new Error(`fixture assets must be regular files: ${path}`);
    }
  }
  return files;
}

async function treePin(path) {
  const directory = join(fixturesRoot, path);
  const files = await fixtureFiles(directory);
  const hash = createHash("sha256");
  let sizeBytes = 0;
  for (const file of files) {
    const content = await readFile(join(directory, file));
    sizeBytes += content.length;
    hash.update(file);
    hash.update(Buffer.from([0]));
    hash.update(String(content.length));
    hash.update(Buffer.from([0]));
    hash.update(content);
    hash.update(Buffer.from([0]));
  }
  return {
    path,
    file_count: files.length,
    size_bytes: sizeBytes,
    sha256: hash.digest("hex"),
  };
}

async function printPins() {
  const paths = [
    "adversarial/fence-bomb",
    "adversarial/malformed-json-payload",
    "adversarial/prompt-injection-repo",
    "code/mini-monorepo",
    "code/outline-large-file",
    "conversations/long-session-prune",
    "conversations/superseded-decisions",
    "conversations/tool-output-dupes",
    "debugging/compiler-output-rust",
    "debugging/test-failure-context",
    "logs/cargo-build-noise",
    "logs/repeated-npm-install",
    "logs/stack-trace-dupes",
    "provider-payloads/anthropic-counts",
    "provider-payloads/openai-unknown-fields",
  ];
  for (const path of paths) {
    process.stdout.write(`${JSON.stringify(await treePin(path))}\n`);
  }
  for (const path of [
    "json-2000-elements/input.json",
    "source-200kb/input.ts",
  ]) {
    const content = await readFile(join(benchmarkRoot, path));
    process.stdout.write(
      `${JSON.stringify({
        path: `benchmarks/${path}`,
        size_bytes: content.length,
        sha256: createHash("sha256").update(content).digest("hex"),
      })}\n`,
    );
  }
}

await generateStackTraceDupes();
await generateCargoBuildNoise();
await generateSupersededDecisions();
await generateLongSession();
await generateOutlineLargeFile();
await generateCompilerOutput();
await generateAdversarialAssets();
await generateOpenAiUnknownFields();
await generateBenchmarks();
await printPins();
