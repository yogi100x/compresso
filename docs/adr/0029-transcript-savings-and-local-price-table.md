# ADR-0029: Transcript-derived savings and a local price table

## Status

Accepted.

## Context

Savings were reported only from the local operation ledger, and
`modeled_cost_saved` was fixed at zero because no price model was committed.
Two gaps followed. A headless or non-interactive run whose ledger is not
available afterwards (ephemeral home directory, discarded container, a saved
run inspected on another machine) reported zero savings even though the client
transcript recorded every Compresso tool call. And the ledger does not know
which model consumed a result, so savings could not be attributed or priced
per model.

D6 forbids remote telemetry, D58 keeps provider-observed usage separate from
counterfactual savings, and D69 keeps retrieval savings explicitly modeled.

## Decision

**Price table.** Prices live in one checked-in, versioned file,
`rust/compresso-core/data/price-table-v1.json`, validated by
`schemas/price-table/v1.json` and compiled into the binary. It carries
`schema_version`, `table_version`, `effective_date`, a `source` statement, and
per-model input, output, cache-write (5-minute and 1-hour), and cache-read
rates in US dollars per million tokens. Compresso never fetches prices. A
model is matched by exact identifier or listed alias, after removing at most
one trailing `-YYYYMMDD` snapshot date. Any other model is unpriced: its
tokens are reported and its cost is `null` (per-model rows) or contributes
zero with `priced: false` (ledger total). A price is never guessed.

**Rate applied.** Avoided tokens are priced once at the model's base uncached
input rate. Content entering a context window is billed at least that rate on
the first request that carries it; cache-write premiums and later cache
re-reads are not claimed. Ledger token counts use the o200k unit and are not
scaled up to a provider tokenizer. Negative totals cost nothing. The result
keeps the existing `modeled` label and is never presented as
provider-observed cost.

**Transcript source.** `compresso stats --transcript PATH` scans a JSON Lines
client transcript (interactive session log or saved headless stream) in which
assistant messages carry `tool_use` blocks and user messages carry
`tool_result` blocks. Only Compresso tools are considered, matched by exact
tool name or by the final segment of an `mcp__<server>__<tool>` name. From
each successful result only `schema_version`, `operation_id`, and the numeric
accounting fields are read; the model comes from the assistant message that
issued the call. No transcript text is retained, persisted, or transmitted,
and scanning writes nothing to the ledger.

**Untrusted model identifiers.** A model identifier from a transcript or a
caller is kept only when it is 1 to 128 bytes of printable ASCII without
spaces. Anything else (C0 or C1 controls, DEL, escape sequences, invalid
UTF-8, invisible or bidirectional Unicode, oversized values) is discarded at
parse time, counted in `model_ids_rejected`, attributed to the unknown model,
and never priced or echoed. Caller-supplied `model` arguments failing the same
rule are rejected as `invalid_arguments` with a fixed message. As a second
barrier, every transcript- or caller-derived string the CLI renders is passed
through an escaper that turns non-printable characters into visible `\u{..}`
text, so stats output cannot carry terminal control sequences.

**Semantics.** Each distinct `operation_id` counts once. When the local
ledger holds that operation, the ledger row is authoritative and transcript
numbers are ignored. Otherwise:

- `compresso_compress` uses the result's exact before/after counts, and
  claims zero when `fallback_used` is true, matching ledger rules;
- retrieval tools report exact pack tokens and claim zero avoidance, because
  the result payload carries no raw-source baseline (the same rule D69 applies
  to historical rows).

Error results, unparseable or truncated results, unknown schema versions,
malformed operation identifiers, duplicate operations, and calls without a
result contribute nothing and are counted in the report. Transcript savings
are a separate section and are never added to ledger totals.

**Surfaces.** The CLI stats envelope and the MCP stats result stay at
`schema_version` 1 with additive fields: `cost_basis` on both, and
`transcript_savings` on the CLI (`null` without `--transcript`). Both accept
an optional model used to price the ledger total. The MCP tool accepts no
file path, so the server gains no filesystem capability.

## Consequences

Saved headless runs report applied compression savings and per-model cost
without a ledger, and ledger-backed runs gain per-model attribution. Retrieval
avoidance for a ledgerless transcript stays zero until a baseline is exposed
in retrieval results, which would be a separate contract change. Prices age:
the table's `effective_date` is shown with every priced figure and the table
must be updated by a reviewed commit. Only one transcript shape is supported;
other client formats contribute zero rather than a guess.
