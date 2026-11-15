# ADR-0031: Confirmed pull request post through the GitHub CLI

## Status

Accepted 2026-11-15 by the owner (D81, follow-up to E17-09). It adds a second
process capability to a `bounded-local` crate, limited to the `pr_post`
module.

## Context

`compresso decisions` drafts a pull request comment from the content-free
operation ledger. Posting it was a stub: `compresso-cli` may not start child
processes (01-stack §3.1), so `--post --yes` failed closed and printed the
`gh pr comment` pipeline for the user to run. The owner asked for the post to
happen when it is explicitly confirmed.

## Decision

After `compresso decisions --pr NUMBER --post --yes`, and only then, the CLI
runs the GitHub CLI itself.

- **What runs.** Exactly `gh pr comment <NUMBER> --body-file -`, started with
  `std::process::Command`. No shell is involved. The only variable argument
  is the pull request number, which is parsed as a positive integer. The
  draft is written to the child's standard input and never appears in an
  argument.
- **Which `gh`.** The first `gh` (`gh.exe` or `gh.cmd` on Windows) found in an
  absolute `PATH` entry, started by its absolute path. Relative `PATH`
  entries are not searched, so the working directory cannot choose the
  program.
- **Where.** In the current working directory with the caller's environment,
  because `gh` takes the repository from the directory and its credentials
  from its own configuration. Compresso neither reads nor passes a token.
- **Bounded.** `gh` has 60 seconds. If it has not finished it is killed and
  the command reports `post_timed_out`.
- **What comes back.** Only the exit status. `gh`'s output is discarded, so
  nothing it prints can reach Compresso's output or the ledger.
- **Gates that stay.** Preview is still the default; `--post` without `--yes`
  reports `confirmation_required`; `--dry-run` never reaches the poster; the
  posted body is byte-identical to the previewed draft, which is built only
  from the ledger and is secret-redacted with markup, mentions, and issue
  references removed.
- **Enforcement.** `scripts/check-crate-capabilities.ts` allows process APIs
  in `src/commands/pr_post.rs` of `compresso-cli`, beside the existing
  allowance for `src/commands/mcp_probe.rs` (ADR-0028). No process-capable
  dependency is added.

## Consequences

- Compresso can now cause an outbound network request, made by `gh` under the
  user's GitHub identity, and writes to a third-party service. It happens
  only on an explicit, confirmed command and never from the MCP server, a
  hook, or any automatic path. Compresso itself still opens no connection.
  This is not telemetry (D6): the content is the draft the user was shown.
- A posted comment is public to everyone who can read the pull request and
  cannot be recalled by Compresso.
- Failure codes: `gh_not_found`, `post_unavailable` (the program could not be
  started), `post_failed` (it exited unsuccessfully), `post_timed_out`. Each
  states that nothing was posted, except that after a timeout the comment may
  or may not have been delivered before `gh` was stopped.
- A timed-out `gh` is killed, but a process it started itself may outlive it.
- The trust placed in the program named `gh` on the user's `PATH` is the same
  trust the user places in it when running the printed pipeline by hand.
- Tests start only a stand-in script, never the real `gh`, and reach no
  network. The integration test that exercises the production wiring uses
  `--dry-run` so that it cannot post from a developer machine or CI.
