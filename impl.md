## Immediate implementation sequence

  ### 1. Complete E1 foundation

  Implement E1-01 through E1-06 in dependency order:

  - Scaffold Rust and TypeScript workspaces.
  - Pin toolchains and dependencies.
  - Create crate/package boundaries.
  - Configure formatting, linting, security, and CI.
  - Establish schema compilation and contract checks.
  - Create fixture structure.
  - Produce the checksummed local installer artifact.

  Commit each ticket independently after its acceptance criteria pass.

  ### 2. Build the vertical slice in parallel

  After E1:

  E2: Compression engine
  E3: Atlas indexing and storage

  E2 delivers:

  - ContextGraph
  - Token accounting
  - Safe compression strategies
  - Protected-content invariants
  - Deterministic traces
  - Fail-open behavior

  E3 delivers:

  - SQLite store
  - Repository scanner
  - TS/JS, Python, and Rust parsers
  - Incremental indexing
  - Search, outline, unfold, and impact queries

  ### 3. Expose the product

  E4 CLI
    → E5 MCP server
    → E6 Claude Code/Codex/Cursor integrations

  This creates the first usable Compresso workflow:

  install
  → index repository
  → connect coding client
  → search/read/compress through MCP

  ### 4. Build quality gates

  Implement E7 alongside the E2/E3 tail:

  - Controlled fixture corpus
  - Compression benchmarks
  - Recall probes
  - Safety checks
  - Latency gates
  - Passthrough comparisons
  - Release-blocking evaluation

  ### 5. Release the local vertical slice

  The slice is complete when a user can:

  compresso doctor
  compresso index .
  compresso integrations install codex --dry-run
  compresso integrations install codex

  Then Codex can use Compresso’s MCP tools without cloud access.

  Do not start E8–E16 or licensing infrastructure yet. The next concrete action is implementing E1-01.