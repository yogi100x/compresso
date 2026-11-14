# Benchmark campaigns

`compresso-eval` can drive an external task suite (for example a
SWE-bench Verified or TerminalBench style suite) as a resumable campaign. The
campaign runner lives in `rust/compresso-eval/src/campaign/` and is exposed by
the `compresso-eval-campaign` developer binary. It is not part of the shipped
`compresso` command tree.

## Running a campaign

```sh
cargo run -p compresso-eval --bin compresso-eval-campaign -- \
  run --suite rust/compresso-eval/tests/data/campaign-smoke \
  --checkpoint /tmp/campaign.json --concurrency 2 --json
```

| Command | Purpose |
|---------|---------|
| `compresso-eval-campaign run` | Run every non-terminal task and print the report. |
| `compresso-eval-campaign status` | Print the report for an existing checkpoint; runs nothing. |

`run` accepts `--suite`, `--checkpoint`, `--adapter`, `--resume`,
`--concurrency`, `--max-attempts`, `--task-timeout-ms`, `--allow-external`, and
`--json`. `status` accepts `--checkpoint` and `--json`. Both exit `0` only when
every task is terminal, `1` on an error or an incomplete campaign, and `2` on a
usage error.

A fresh `run` refuses to start when the checkpoint file already exists, and
`--resume` refuses to start when it does not. This keeps an accidental rerun
from discarding hours of completed work.

## Task state machine

```text
pending -> running -> passed | failed | exhausted
              |
              +-----> pending   (retryable failure, attempts remaining)
```

- `passed` and `failed` are verdicts from the suite's own grader. They are
  final and never retried.
- An adapter error, a timeout, an adapter panic, or a process interruption is
  a retryable failure. The task returns to `pending` until `--max-attempts`
  attempts have started, then becomes `exhausted`.
- The attempt counter is incremented and persisted before the attempt starts.
  A task found `running` when a campaign is resumed therefore has its
  interrupted attempt counted, so a task that crashes the process every time
  is exhausted instead of looping forever.

## Checkpoint

The checkpoint is one JSON document with `schema_version`, the suite identity
(`id`, `version`, `mode`), and one record per task in identifier order. It is
rewritten after every state transition by writing a sibling temporary file,
syncing it, and renaming it over the checkpoint, so a reader observes either
the previous or the new checkpoint in full.

Resuming validates the schema version, the suite identifier and version, and
the exact task identifier set. A mismatch is refused; nothing is silently
upgraded or merged. Concurrency, attempt budget, and timeout are execution
policy rather than identity and may differ between the original run and the
resume. A lowered attempt budget is applied on resume: a task waiting for a
retry it is no longer entitled to becomes `exhausted` without running.

The CLI installs no signal handler. Interrupting the process is safe because
every transition is already durable; the next `--resume` reclaims whatever was
`running`.

Only one process may own a checkpoint at a time. The runner does not lock the
file.

## Timeouts and concurrency

`--concurrency` bounds the number of attempts in flight. Each attempt runs on
its own thread and the runner waits at most `--task-timeout-ms` for it. On
timeout the runner raises the attempt's cancellation flag and stops waiting. A
thread cannot be killed, so an adapter must poll `TaskContext::is_cancelled`
and release what it holds; an adapter that ignores cancellation leaks its
thread until it returns.

## Report

The report has the same top-level shape as the fixture evaluation report
(`schema_version`, `fixtures`, `aggregate`, `recall`) plus one `campaign` key.

- Each task is one `fixtures` row with `kind` `campaign`, identifier
  `<suite>/<task>`, `before_tokens` and `after_tokens` taken from the task
  measurement, and `passed` true only for a resolved task. A task that did not
  pass carries a `campaign_task` failure naming its state.
- `aggregate` is computed over those rows with the same median and nearest-rank
  P95 as compression fixtures. Tasks without a measurement are not eligible
  for the reduction median.
- `recall` is empty because external suites carry no recall probes. The
  release gates in `docs/plan/gates.toml` require probes and are therefore not
  applied to campaign reports.
- `campaign` carries the suite identity, per-state counts, `resolved_rate`
  (passed tasks over all tasks), attempt totals, and the per-task state.

Rows are emitted in task identifier order, so the report is independent of
scheduling and of how many times the campaign was resumed. Only timing fields
vary between runs.

## Adapters

A suite is reached only through the `SuiteAdapter` trait:

```rust
pub trait SuiteAdapter: Send + Sync + 'static {
    fn descriptor(&self) -> SuiteDescriptor;
    fn tasks(&self) -> Result<Vec<TaskSpec>, AdapterError>;
    fn run_task(
        &self,
        task: &TaskSpec,
        context: &TaskContext,
    ) -> Result<TaskMeasurement, AdapterError>;
}
```

`FixtureSuiteAdapter` is the only adapter built into `compresso-eval`. It
reads a `suite.json` manifest whose tasks script their verdict, token counts,
transient failures, and delay. It performs no network or process I/O and is
what the test suite exercises.

### Plugging in a real suite

`compresso-eval` is a `bounded-local` crate: its library and binaries may not
spawn processes or open network connections. A real suite needs both (a
dataset download and a container per task), so its adapter does not belong in
this crate.

1. Create the adapter in a crate whose capability class permits the access it
   needs, and register that crate in `scripts/check-crate-capabilities.ts` and
   the capability matrix in `docs/plan/01-stack-and-workspace.md`.
2. Implement `SuiteAdapter`. Return a stable `SuiteDescriptor` (pin the dataset
   revision in `version`) and stable task identifiers. In `run_task`, run the
   task with and without Compresso, grade it with the suite's own harness, and
   return a `TaskMeasurement`. Map infrastructure problems to `AdapterError`;
   an unresolved task is a measurement with `resolved` false, not an error.
   Stop the container when `TaskContext::is_cancelled` becomes true.
3. Give that crate a thin binary that constructs the adapter and calls
   `CampaignRunner::new(adapter, policy, checkpoint).run(start)`, then prints
   `CampaignReport::from_checkpoint`.

The names `swe-bench-verified` and `terminal-bench` are reserved values of
`--adapter`. Selecting one without `--allow-external` fails with
`external_adapter_not_allowed`. With the flag it fails with
`external_adapter_unavailable`, because no such adapter is built into this
crate. The flag exists so that a network-capable adapter can never be reached
by default once one is wired in; it does not enable anything today.
