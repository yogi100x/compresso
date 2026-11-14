# 09 - Clean-room Advanced Capabilities (E15)

Source research: `compresso-addon-capabilities.md`. Decisions: D49-D50 and ADR-0023/0024. This specification replaces source naming with original Compresso contracts.

## 1. Product boundary

E15 adds `compresso-search`, `compresso-edit`, `compresso-sql`, `compresso-recall`, `compresso-knowledge`, and `compresso-review`. Browser and terminal modules are deferred behind separate sandbox gates. Existing Atlas, E13 model routing, permissions, local API, store, and client adapters are reused. Native client tools remain available in default `assist` mode.

## 2. Clean-room rules

- Public behavior is input, not implementation.
- Do not copy the reference product's source, prompts, copy, identifiers, assets, or obfuscated structures.
- Every algorithm, schema, fixture, and prompt is authored from Compresso requirements.
- The reference product is never downloaded by builds, tests, benchmarks, or runtime code.
- Unknown behavior is not inferred from obfuscated bundles.

## 3. Permission classes

| Class | Examples | Enforcement |
|---|---|---|
| Read | search, importers, schema inspection | approved roots/targets and hard bounds |
| Private read | recall, knowledge | explicit feature and source opt-in; exception (D75): index-free E17-02 recall of a client's own transcripts is enabled when that client's integration is installed, and `--no-recall` opts out |
| Plan | edit plan, SQL mutation preview | no side effects; content-hashed result |
| Write | edit apply, remember, review apply | plan hash, base-version check, approval |
| Destructive | rollback, forget, SQL mutation | elevated approval and audit |
| External action | browser, terminal | isolated sandbox, destination/command policy, approval |

Plans expire, are session/target scoped, and are invalidated by base-version changes. Model output cannot create an `ApprovalGrant`.

## 4. Combined search

`CombinedSearchRequest` contains approved roots, path/glob filters, optional bounded regex, output mode (`paths`, `counts`, `snippets`, `structure`), line context, case/multiline flags, file/match/line/token/byte limits, known content versions, structural mode, importer inclusion, and typed asset policy. Results contain relative canonical paths, hashes, timestamps, exact ranges, bounded snippets or outlines, truncation metadata, token counts, parser confidence, importer edges, and unchanged markers.

Regex compilation and execution have size and time limits. Binary, secret-pattern, dependency, oversized, and escaping-symlink inputs are skipped with typed reasons. PDF extraction and image return run in resource-limited workers and remain disabled until adversarial fixtures pass.

## 5. Transactional edit

`EditPlan` contains transaction id, approved root id, base hashes, ordered operations, validation policy, diff hash, bounded unified diff, permission class, expiry, and rollback feasibility. Operations cover create, exact replace, replace-all, range delete, overwrite, and notebook cells. Fuzzy matching is opt-in; ambiguity fails.

Apply re-resolves paths, compares base hashes, materializes a private temporary tree, runs validation, verifies approval and plan hash, writes same-filesystem temporary files, atomically renames, persists encrypted rollback material, and emits changed hashes. Partial completion requires a separate explicit non-atomic policy. Rollback verifies post-apply hashes before restoring.

## 6. SQL intelligence

Connection profiles store engine, target metadata, read-only default, and keychain secret reference. PostgreSQL, MySQL/MariaDB, and SQLite adapters expose a normalized schema graph. Statements classify as read-only, data mutation, schema mutation, administrative, or unknown.

Read queries require a read-only role where supported, timeout, row/byte/cell/line limits, cancellation, and redaction. Multi-statement input is rejected unless it is a named batch classified per statement. Mutation planning uses a rollback transaction where supported and returns affected-object estimates; application requires elevated approval. Unknown and administrative statements are denied by default.

## 7. Recall and knowledge

Recall is disabled until enabled per source. Sources include Compresso sessions, selected Claude/Codex histories, imported notes, and saved decisions. The pipeline parses events, redacts secrets, chunks, encrypts content, builds lexical and optional local embedding indexes, and ranks by relevance, recency, project, provenance, and outcome confidence. Results are bounded and cite source, revision, and timestamp.

Knowledge scopes are personal, repository, and organization. Items carry provenance, confidence, expiry, source revision, and personalization history. Add, edit, suppress, boost, undo, delete, export, and rebuild are supported. Organization synchronization is optional and client-side encrypted; cloud services never receive plaintext knowledge.

## 8. Review and evaluation

Review collects committed, staged, working-tree, and approved untracked changes relative to a base. Original lenses cover correctness, failure boundaries, data/type integrity, security/privacy, abstraction reuse, maintainability, tests/observability, and migration/documentation safety. Findings have severity, confidence, verified path/line, evidence, optional patch plan, and validation. Applying selected findings uses the normal edit plan/apply boundary.

Historical backtests isolate a pre-review revision, remove write remotes and hosting credentials, hide human comments/fixes, and compare findings against held-out comments and merged changes. Metrics include precision, recall, severity-weighted recall, false-positive burden, cost, latency, and test outcomes.

## 9. Client parity and modes

Claude Code and Codex receive identical core schemas and contract tests. Adapters report MCP, hooks, approval UI, image, resource-link, skills, and status capabilities. Missing hooks are derived from MCP activity or explicit commands, never unsupported private APIs.

Modes are `assist` (coexist, default), `prefer` (explicit preference), and `enforce` (visible organization policy). Installation never selects a default agent or disables native tools.

## 10. IP and security gates

- E14 classifies E15 algorithms, prompts, evaluations, and tuning data.
- Write/SQL/browser/terminal threat models close before enablement.
- No secrets enter plans, traces, provenance, findings, or telemetry.
- Browser workers use fresh profiles, destination allowlists, download quarantine, and no host credentials.
- Terminal workers use command policy, restricted environment, approved cwd, output/time limits, and no implicit interpolation.
- Recall/review payloads are encrypted and excluded from support bundles by default.

## 11. Delivery phases

| Phase | Scope | Exit gate |
|---|---|---|
| E15-A | Combined search and importers | One bounded call replaces discover/search/read; no path escape |
| E15-B | Transactional editing and validation | Atomic multi-file apply and verified rollback; ambiguity fails |
| E15-C | SQL intelligence | Cross-engine schema graph; bounded read-only query by default |
| E15-D | Local recall and knowledge | Opt-in, encrypted, provenance-bearing, exportable and deletable |
| E15-E | Review and evaluation | References verified; held-out quality/cost gates pass |
| E15-F | Client parity and UI | Claude/Codex parity; assist default; controls reversible |
| E15-G | Browser and terminal, deferred | Threat model, sandbox escape tests, explicit owner enablement |

## 12. Definition of done

- Search supports text, source, images, notebooks, and PDFs within typed bounds.
- Edit is previewable, hashed, atomic, validated, and reversible.
- SQL inspection supports PostgreSQL, MySQL/MariaDB, and SQLite; mutation is elevated.
- Recall and knowledge are local by default, inspectable, encrypted, and deletable.
- Review quality is measured on held-out history with test outcomes beside savings.
- Claude Code and Codex pass the same tool and permission contract suite.
- Browser and terminal remain disabled until their separate gates pass.
- All code and fixtures satisfy clean-room provenance.
