# ADR-0038: One opt-in, read-only MongoDB tool beside the SQL tool

## Status

Proposed 2026-11-23 (E17-52, D117).

**Owner-decided (2026-11-23):** MongoDB support as an immediate follow-up
to the SQL tool (ADR-0036); its own tool, `compresso_mongo`, not an engine of
`compresso_sql`; and two licences added so the official driver can be
linked, `CDLA-Permissive-2.0` and `CC0-1.0`.

**Everything else here was chosen by the implementing session and is
awaiting owner confirmation** (listed in D117), including the
capability-check amendment in decision 11.

**Live status.** The CI job `live-mongo` added by this change has **not yet
run**: the branch has not been pushed. The nine live tests in
`rust/compresso-sql/tests/live_mongo.rs` passed on the build machine on
2026-11-23 against throwaway local containers the session started and
removed: MongoDB 8.0.38 with access control on, and MongoDB 8.0.38 with TLS
(a certificate for `localhost` signed by a private root made for the run)
and access control off. Statements below not covered by one of those tests
are reasoned, not observed.

## Context

ADR-0036 made `compresso-sql` the only crate that opens database
connections and gave each SQL engine two controls: a statement gate and the
database's own read-only session. MongoDB has no read-only session: a
connection can do whatever its account may do, and the query language can
write (`$out`, `$merge`) or run JavaScript on the server (`$where`,
`$function`, `$accumulator`, `mapReduce`). On a live 8.0.38 server (root
account, through the driver directly) `$where`, `$function`, and
`$accumulator` ran JavaScript and `$out` wrote a collection
(`test_live_mongo_the_refused_operators_do_run_javascript_and_write_here`).

So for MongoDB **the argument allow-list and the account check are the
control, not defence in depth**. Either alone is meant to stop a write: the
allow-list never sends a writing or JavaScript key, and the account check
refuses any account that could write. If both failed, nothing else would
stop it.

## Decision

1. **Same crate, own tool.** The driver lives in `compresso-sql` as the
   module `mongo` (the crate is not renamed). One crate stays the only
   network path, so the capability check and the `AGENTS.md` exception
   still name one crate, and the tool reuses `cells.rs` (cuts, escaping of
   hidden characters) and `redact.rs` (URL scrubbing) without making them
   public. The MCP tool is `compresso_mongo`, with its own request, outcome,
   and error codes; it does not reshape `SqlRequest` or the `Connector`
   trait.
2. **Opt-in, fixed at startup.** `compresso mcp serve --mongo-env NAME`
   (`COMPRESSO_MONGO_[A-Z0-9_]+`) reads that one variable once;
   `integrations install claude-code --mongo-env NAME` writes only the name
   (after any `--sql-env NAME`, before the recall source). No `.env`
   reading, no discovery, no URL as a tool argument. Full profile only
   (`--tools lean --mongo-env` is refused), never the exploration agent;
   an unadvertised call answers `tool_unavailable`. With the flag off every
   surface and golden is byte-identical; with it on, the guarantee sentence
   names `compresso_mongo` (and `compresso_sql` when both are on).
3. **No raw commands.** The tool builds every command itself from typed
   arguments: `find`, `aggregate`, `count` (the driver's `countDocuments`),
   `distinct`, `collections` (`listCollections` with
   `authorizedCollections`), and `describe` (`$sample` of 100 documents,
   field paths three levels deep with the BSON types seen, and
   `listIndexes`). There is no action that forwards a command document.
4. **The allow-list** (`mongo/gate.rs`), checked before any connection
   (a counting listener sees zero connections for every refusal:
   `tests/mongo.rs`). JSON is converted to BSON by hand; there is no
   extended-JSON parser, so `{"$code": ...}` and friends are just refused
   keys. Accepted `$` keys:
   - *Filters* (and `$match`): `$all $and $elemMatch $eq $exists $expr
     $gt $gte $in $lt $lte $mod $ne $nin $nor $not $options $or $regex
     $size $type`, plus the expression operators inside `$expr`.
   - *Find projections*: inclusion or exclusion (0, 1, true, false),
     `$slice` with integer arguments, `$elemMatch` with a checked filter;
     nothing else (the server evaluates a find projection as an
     expression; see the position table). *Sorts*: 1 or -1 only; several fields only as
     an array of one-field objects (`[{"a": -1}, {"b": 1}]`), so their order
     never depends on how a JSON object keeps key order. Key order inside
     an embedded-document equality value follows the JSON parser (insertion
     order in the shipped binary, whose `serde_json` keeps it) - a known
     limit.
   - *Stages*: `$addFields $bucket $bucketAuto $count $facet $group $limit
     $lookup $match $project $replaceRoot $replaceWith $sample $set $skip
     $sort $sortByCount $unset $unwind`. `$facet` may not be nested;
     `$lookup` only with `from` a plain collection name (the object form
     can name another database), nested pipelines checked with the same
     list.
   - *Expression operators*: arithmetic (`$abs $add $ceil $divide $exp
     $floor $ln $log $log10 $mod $multiply $pow $round $sqrt $subtract
     $trunc`), comparison and boolean, `$cond $ifNull $switch`, strings
     (`$concat $indexOfCP $ltrim $rtrim $trim $split $strLenCP $substrCP
     $toLower $toUpper $regexMatch $strcasecmp`), conversions, arrays
     (`$arrayElemAt $concatArrays $filter $first $last $in $isArray $map
     $reduce $reverseArray $size $slice $indexOfArray $arrayToObject
     $objectToArray`), sets, dates, `$mergeObjects $literal $let`; the
     exact list is `EXPRESSION_OPERATORS`.
   - *Accumulators*: `$addToSet $avg $count $first $last $max $min $push
     $stdDevPop $stdDevSamp $sum`.
   - *Literals*: `{"$oid": "<24 hex>"}` and `{"$date": "<RFC 3339>" |
     <ms>}` become an object id and a date; `$literal` content is checked
     as plain data (no `$` key inside it).
   Refused by name: `$where $function $accumulator` (JavaScript), `$out
   $merge` (writes), and `$changeStream $changeStreamSplitLargeEvent
   $collStats $currentOp $documents $graphLookup $indexStats
   $listLocalSessions $listSampledQueries $listSearchIndexes $listSessions
   $planCacheStats $querySettings $queryStats $shardedDataDistribution
   $unionWith`; every other `$` key is refused as off the list, at every
   depth. Field names may not start with `$` or hold NUL; `system.*`
   collections are refused. Bounds: depth 24, 10,000 values, 50 stages
   (nested included), 100,000 characters per argument, `skip` at most
   1,000,000. The finished BSON is walked once more and any JavaScript,
   symbol, or database-pointer value, or `$` key off every list, is
   refused. *Same-database `$lookup` is allowed* because the account check
   already limits the account to reading the configured database, so a
   join reads nothing `find` could not; its cost (a join is quadratic) is
   bounded by the same time limit. `$graphLookup` (recursive) and
   `$unionWith` are refused to keep the list small.
5. **The account check** (`mongo/account.rs`), on every connection before
   the action, fail closed: `connectionStatus` with `showPrivileges` must
   show exactly one authenticated user; every role with a built-in name
   must be `read` on the configured database (any other built-in role on
   any database refuses; custom roles are judged by their privileges);
   every privilege's resource must be `{db: <configured>, collection:
   <any>}` (never cluster, `anyResource`, an empty or other database); every
   action must be one MongoDB 8.0.38 reported for a `read` user:
   `changeStream collStats dbHash dbStats find killCursors listCollections
   listIndexes listSearchIndexes planCacheRead`. Then `hostInfo` on `admin`
   (read-only, needs a cluster action no accepted account holds) must fail
   with `Unauthorized`: a server without access control still lets a client
   authenticate and reports the user's privileges but enforces none, and
   only this probe catches it (seen live: the TLS test server, which runs
   without access control, is refused this way). Refusals are
   `mongo_account_refused` naming the rule, never a user, role, database,
   or action.
6. **URL and transport** (`mongo/target.rs`). Only `mongodb://`, exactly one
   host (a comma, encoded or not, refuses), a user and a password, and one
   database that is not `admin`, `local`, or `config`. URL options by
   allow-list, case-insensitive, each at most once (`tls` and `ssl` count as
   one): `authMechanism` (SCRAM-SHA-256 or SCRAM-SHA-1), `authSource`,
   `directConnection` (true only), `tls`/`ssl`, `tlsCAFile` (an absolute
   path to a PEM certificate file, checked as for SQL). Refused:
   `mongodb+srv://` (the driver would read hosts from SRV records and
   options from a TXT record; the driver is also built without its DNS
   resolver, so SRV cannot work), several hosts and `replicaSet`
   (discovery contacts hosts the URL does not name), local sockets, and
   every other option (`tlsInsecure`, `tlsAllowInvalid*`, `readPreference`,
   `compressors`, `loadBalanced`, `proxyHost`, ...). The URL is never given
   to the driver's parser: the client options are set field by field with
   one host, a direct connection, a pool of one, no retries, polling
   monitoring, read concern `local`, and read preference `primaryPreferred`
   (so a direct connection to a secondary still reads). A loopback host may
   use plain text unless the URL asks for TLS; any other host always gets
   TLS with certificate and host-name verification (rustls has no option to
   skip either; the tool sets none), against the driver's bundled
   `webpki-roots` or, with `tlsCAFile`, only that file (the driver replaces
   the bundled roots). Unlike the SQL tool, the operating system's store is
   not used. Seen live: reached as `127.0.0.1`, a server whose certificate
   names only `localhost` is refused.
7. **Environment.** The driver reads no variable that changes where or how
   it connects (no DNS resolver, no OS certificate loader, bundled roots).
   It does read `AWS_EXECUTION_ENV`, `AWS_LAMBDA_RUNTIME_API`, `VERCEL`,
   `FUNCTIONS_WORKER_RUNTIME`, `K_SERVICE`, and `FUNCTION_NAME` to detect a
   function-as-a-service host, and then sends values such as the region and
   memory size to the server in its handshake. Like `PG*` for Postgres, any
   of them set (as the driver tests them) makes the target unusable, names
   only.
8. **Bounds and time** (shared with the SQL tool where it fits):
   `max_rows` 100 default, 1,000 at most, sent to the server as a limit one
   past `max_rows`; `max_tokens` 2,000 default, 300-8,000; `timeout_ms` 5,000
   default, 100-30,000, sent as `maxTimeMS` on `find`, `aggregate`,
   `countDocuments`, `distinct`, and the describe sample;
   `allowDiskUse: false`. Text cut at 1,000 characters, names at 64, at most
   100 fields or array items per level, nesting shown to 8 levels, binary as
   its size, JavaScript values as a marker, hidden characters escaped as in
   `cells.rs`; at most 256 KiB of results before the token fit. One call is
   bounded end to end: server selection and connect (10 s), the account
   check (5 s), the action (`timeout_ms` plus 2 s), and shutdown (2 s), on
   a runtime driven from a thread of its own (as E17-50, so a call from
   inside a Tokio runtime cannot panic). Seen live: an aggregation with a
   300 ms limit stopped with `mongo_timeout` after about 0.45 s.
9. **Secrets.** Errors carry fixed codes (`mongo_not_configured`,
   `mongo_refused`, `mongo_account_refused`, `mongo_connection`,
   `mongo_timeout`, `mongo_permission`, `mongo_error`, `invalid_arguments`)
   and scrub the whole URL and every part (user, password, host, database,
   `authSource`, `tlsCAFile`), any `scheme://` text, and `password=`
   values. The report shows `mongo on` or `mongo not configured`, never the
   variable's name.
10. **No ledger row** (D113's reason), as for `compresso_sql`.
11. **Capability-check amendment (awaiting owner confirmation).** The
    driver turns on Tokio's `process` feature in its own manifest,
    unconditionally; its only use of it (spawning the encryption helper) is
    compiled only with its `in-use-encryption` feature, which is off. The
    check counted the feature as process capability for `compresso-sql`,
    `compresso-mcp`, and `compresso-cli`. It no longer does for those three
    when, in the member's own resolved tree, every enabler of
    `tokio feature "process"` is the `mongodb` package and `mongodb` is
    built without `in-use-encryption`. Any other enabler (including a Tokio
    feature such as `full`), the encryption feature, or the driver in any
    other crate still fails (four new negative controls), and the source
    scan still forbids process APIs in every workspace crate. No feature
    selection can avoid this: Cargo features are additive.
12. **Dependencies.** `mongodb` 3.10.2 with `default-features = false` and
    `compat-3-3-0`, `bson-3`, `rustls-tls` (rustls on the ring provider,
    the same provider `sqlx` uses); no DNS resolver, compression, OpenSSL,
    encryption, or AWS/OIDC features. `bson` 3.1.0 directly. Only ring is
    linked (`cargo tree -i aws-lc-rs` finds nothing), and the driver builds
    its TLS configuration with an explicit provider, so the process-default
    provider the SQL path relies on is unchanged. A verified-TLS Postgres read
    passes beside the driver (`test_live_postgres_verified_tls_with_a_private_root_reads`,
    build machine; added to the `live-sql` job). `deny.toml` allows
    `CDLA-Permissive-2.0` for `webpki-roots` and `CC0-1.0` for `tiny-keccak`
    only (per-crate exceptions, no global allow).

## What a model can still do (per action; reasoned unless stated)

- **find, count, distinct**: read every document and field of every
  non-`system.` collection and view in the configured database that the
  account can read. With the built-in `read` role that is the whole
  database. Run load until the time limit (an unindexed filter scans the
  collection; a `$regex` can be slow on purpose).
- **aggregate**: the same reads, plus joins across collections of the same
  database (`$lookup`), grouping, `$sample`, and expression work, each
  stage held to MongoDB's 100 MB in-memory limit (`allowDiskUse: false`),
  until the time limit. Seen live: a quadratic self-join is stopped at its
  limit.
- **collections, describe**: the names and kinds of the collections the
  account may list; field names and types from 100 random documents; index
  names and keys.
- **Time-series collections**: readable (see the confirmation round).
- **Every call**: makes the server log a `connectionStatus`, an
  `Unauthorized` `hostInfo` on `admin` (an audit log records both), and,
  for a call that reads collections, a names-only `listCollections`. Calls
  run one at a time per server, so a slow call delays every other tool until
  it ends; an unreachable server holds a call for about 10 s.
- **Views are refused** (`mongo_view_refused`), directly and as a `$lookup`
  source: reading one runs a pipeline stored on the server that the
  allow-list never sees (it could use `$function` or name collections the
  allow-list refuses). `collections` still lists them, typed `view`.
- **Cannot**: send a writing or JavaScript stage or operator (refused
  before connecting; seen live for `$out`, `$merge`, and `$function`, with
  the server's aggregate counter unchanged, and for `$where`), use an account that can
  write or reach another database (seen live: `readWrite`, `dbOwner`,
  `read` plus `clusterMonitor`, `read` on two databases, and a custom role
  with `insert` are refused; a custom role with only `find` passes), use a
  server without access control (seen live), or reach any host but the one
  configured.

## Least-privilege account (the operator's part)

```javascript
// mongosh, as an administrator, in the data database
use app
db.createUser({ user: "mongo_reader", pwd: passwordPrompt(),
                roles: [{ role: "read", db: "app" }] })
```

Then `COMPRESSO_MONGO_APP=mongodb://mongo_reader:...@db.example.com/app`
(TLS is required for any host but loopback; add `tlsCAFile=/abs/ca.pem` for
a private root). The server must run with access control on. The `read`
role exposes the whole database: keep secrets the model should not see in
another database.

## Review round (2026-11-23)

An independent review of this change found, and this round fixed:

- **describe memory** (must-fix): the field summary stored every path of
  every sampled document (100 documents of 60,000 keys took 1.74 GB). It now
  tracks at most 800 paths (four times the 200 shown) and counts the rest in
  `fields_omitted`, and reads at most 8 MiB of sample. (This round's
  statement that `distinct` was bounded was false: it decoded the whole
  reply; see the confirmation round.)
- **Error text**: the driver's rendering of an error embeds the reply
  document as hex, past the scrub. Errors are now fixed text plus the
  server's numeric code, its code name (letters and digits only), and its
  message (scrubbed, then cut to 600 characters); every message, SQL and MongoDB alike, has
  hidden characters escaped as result cells are (a failed cast quotes the
  stored value). The SQL path had the same unescaped-message problem
  (server messages quoting stored values) and is fixed by the same change in
  `redact.rs`; it never rendered reply documents.
- **Stored `$` keys**: results are decoded from raw BSON without any
  extended-JSON reading (a stored `$oid` key no longer breaks `find`). Output
  form: a stored field name that starts with `$` or `\` is shown with a
  leading `\`, so an object key starting with `$` is always the tool's own
  marker (`$oid`, `$date`, `$timestamp`, `$regex`/`$options`, `$omitted`),
  never stored data.
- **System variables**: `$$NAME` is allowed only for `ROOT`, `CURRENT`,
  `REMOVE`, and `NOW`; any other upper-case name is refused (`$$USER_ROLES`
  returned role and database names the refusals withhold; `$$CLUSTER_TIME`
  and `$$SEARCH_META` report on the server). Pipeline-defined variables
  (lower-case in MongoDB) are unaffected.
- **Multi-field sorts** only as an ordered array of one-field objects.
- **Capability check**: the exemption now also requires the `mongodb`
  package to come from crates.io at exactly 3.10.2 (a path, git, or
  `[patch]` crate of that name, or another version, fails). The source scan
  now refuses glob imports and macro-built paths under `std`, `tokio`,
  `async_std`, and `smol`, `include!` of anything outside the crate's
  scanned sources, and `<name>::process::<member>` paths. The claim that
  this closed the `std::process` forms was false (see the confirmation
  round).
- **Wording**: the guarantee sentence says the tool reads only the
  configured database and sends one probe command on `admin` that must be
  refused.

## Confirmation round (2026-11-23)

A second review found the following, all fixed in this round:

- **Memory per document.** Byte caps held, but each admitted document was
  decoded whole and then rendered: one 15 MB document through `find` peaked
  at 825 MB, a pipeline that builds large documents from small ones at
  610 MB, and `distinct` on one 15 MB value at 795 MB (`distinct` had no
  budget at all). Now replies are read as raw bytes and rendered straight
  from them (`raw.rs`): no `Document` or `Bson` tree of a result document is
  built, each document is rendered within what the result can still hold
  (cut and marked `$truncated`), and a document over 1 MiB is a size marker
  (`{"$document": {"bytes": N}}`). Servers are asked for batches of 16
  documents. `distinct` runs as `$match`, `$unwind`, `$group`, `$limit`
  through the same raw path (unordered, as `distinct` is; the `$group` is
  held to the server's 100 MB limit; where it differs from the server's
  `distinct`, see the second confirmation round). Measured peak RSS of one call in a release build (macOS arm64,
  MongoDB 8.0.38): one 15 MB document through `find` 26 MB, `distinct` on
  one 15 MB value 26 MB, a pipeline generating 11 MB documents from three
  small ones 22 MB, a `$push` of 300,000 values 25 MB, 60,000-key documents
  through `find` 40 MB and `describe` 26 MB (before: 825, 795, 610, n/a,
  403, 133 MB). What remains: the driver holds one server reply while it is
  read (at most 16 MiB of documents; a 48 MiB message at the protocol
  limit).
- **Typed values forged through `distinct`**: it decoded through the
  driver's extended-JSON reading, so a stored `{"$oid": ...}` came back as a
  real object id. It now uses the raw renderer; every non-JSON type is a
  `$`-keyed marker object (`$oid`, `$date`, `$timestamp`, `$regex` with
  `$options`, `$numberDecimal`, `$numberDouble`, `$binary`, `$code`,
  `$symbol`, `$dbPointer`, `$undefined`, `$minKey`, `$maxKey`), and stored
  `$` keys at any depth and in arrays are escaped with a leading `\`.
- **Invalid UTF-8**: a key or string that is not valid UTF-8 is shown
  lossily (U+FFFD) instead of failing the whole call. (The driver refuses to
  insert such a key, so keys are covered by unit tests; strings also live.)
- **System variables only in expressions**: `$$name` is checked only where
  the server reads an expression (stage bodies other than `$match`, `$expr`,
  `$lookup` `let`); a value compared in a filter or a `$literal` is data.
  In an expression a literal string starting with `$` must be wrapped in
  `$literal`, and the refusal says so.
- **Scrub before cut**: server messages are scrubbed of URL parts before the
  length cut, so no prefix of a secret can survive a cut.
- **Time-series collections** are allowed: the server unpacks their buckets
  itself with no pipeline anyone stored, so they are not views in the sense
  that matters here; `collections` lists them as `timeseries`.
- **Capability check**: the lexical scan was still evadable (a `'"'`
  character literal hid what followed; nested use groups; a leading `::`;
  `extern crate` aliases used in a group; a macro given both names;
  `#[path]` modules, extra `[[bin]]` paths, and build scripts outside the
  scanned folders), and it refused any local module named `process`. The
  round then added, for `compresso-sql`, `compresso-mcp`, and
  `compresso-cli`, Clippy's `disallowed_types` and `disallowed_methods` in a
  per-crate `clippy.toml` (std and Tokio process types and `Command::new`;
  only Tokio's in `compresso-cli`, whose sanctioned `std::process` files
  stay), enforced by CI's `-D warnings`; in a scratch crate those source
  forms failed Clippy. That was later found bypassable and is reframed in
  the second confirmation round. The script checks the entries, refuses attributes or manifest
  settings that lower those lints, `#[path]` modules, build scripts, and
  targets outside the scanned folders. The lexical scan stays as a
  secondary check (character literals, nested groups, leading `::`,
  aliases, and the false positive fixed). Residual risk, stated in the
  script header: Clippy does not lint code expanded from another crate's
  macro; a `CLIPPY_CONF_DIR` or `RUSTFLAGS` override in CI would weaken it;
  the lint-level check is lexical; and for every other crate the lexical
  scan remains the only source control, which a dependency re-export under
  another name or a macro assembling the path some other way can evade.

## Second confirmation round (2026-11-23)

- **Find projections were an expression hole** (must-fix): the server
  evaluates a find projection as an aggregation expression, and the gate
  treated it as data, so `{"a": {"b": "$$USER_ROLES"}}` returned the
  account's roles. Projections now accept only the forms in decision 4; any
  string, array, or other object is refused. The final walk over every
  built command checks system variables again, by position.
- **Output budget**: it counted raw bytes; escaping can multiply them by
  six. It now charges the bytes actually written (names and strings as
  JSON), and a document larger than what is left is cut and marked
  `$truncated`, not dropped. Rows after a full result are not read; the
  result says `max_bytes` (their number is not known). This was true of
  the library only: the MCP token fit then dropped whole documents from the
  end, so a large document could still come back as zero rows. The third
  confirmation round fixed the token fit (both tools): rows are dropped from
  the end, the first row that does not fit is cut (strings shortened, the
  rest marked `$truncated`; SQL cells cut evenly so columns stay aligned)
  and kept, and `rows_omitted` counts rows left out for the budget, so a
  result with rows never comes back empty. The SQL tool had the same
  drop-to-zero behaviour with one huge row and is fixed the same way. Dates outside years
  1 to 9999 show raw milliseconds (`{"$date": <ms>}`) instead of a clamped
  date.
- **Escaping is injective**: backslashes in stored names and strings are
  doubled before hidden characters are written as `\uXXXX`, so a hidden
  character and the same text stored literally never render alike, and a
  single leading `\` only marks a stored name that starts with `$`.
- **`distinct` is not the server's `distinct`**: it runs `$match` (field
  exists), the value as an array, `$unwind`, `$group`, `$limit`. A live test
  compares it with the server's own `distinct` (run only in the test,
  through the root account): equal for scalars, an explicit `null`, a
  subdocument, and an array at the end of the path. It differs where the
  path crosses an array of subdocuments (`b.c` over `[{c: 5}, {c: [6, 7]}]`
  gives 5 and `[6, 7]`; the server's gives 5, 6, 7). (An earlier version
  of this record said `undefined` became `null`; on 8.0.38 the server's
  `distinct`, `$group`, and the tool all keep them apart.) The tool
  description calls it
  distinct values of a field; these differences are its contract.
- **Memory, stated as measured**: peak RSS per call in a release build was
  22 to 26 MB for single 11-15 MB documents through `find`, `distinct`, and
  `aggregate`, and 37 to 40 MB for many-key and nested-array documents
  through `find` (60,000-key documents; 200 documents of nested arrays).
- **The process check, stated honestly.** It is a tripwire against
  introducing process spawning by accident or casually, enforced by
  name-resolved lints plus structural checks; it is not proof against a
  contributor deliberately obfuscating code, a dependency's internals, or
  code expanded from another crate's macro, and review remains the control
  for those. Enforcement: a workspace `clippy.toml` disallows the std and
  Tokio process types and `Command::new` in every crate except
  `compresso-agent` and `compresso-updater` (their own `clippy.toml`);
  `allow` attributes for those lints only in `compresso-cli`'s two
  sanctioned files and bounded-local integration tests. The capability
  script parses `clippy.toml`, manifests, and `.cargo/config` as TOML,
  tokenises attributes, and refuses a missing or commented-out entry, a
  crate-level `clippy.toml`, lint-lowering attributes (spaced, `cfg_attr`,
  `expect`, or macro-built), `[lints]` tables that lower the guarded lints,
  cargo `rustflags` or `CLIPPY_*` variables, `unsafe` code
  (`#![forbid(unsafe_code)]` in `compresso-mcp` and `compresso-cli`,
  `#![deny]` in `compresso-sql` with one pinned site), extern blocks and FFI
  attributes, and new direct FFI or process dependencies (`libc` in
  `compresso-atlas` and `windows-sys` in `compresso-integrations` were on
  main and stay allowed). Each evasion the second review found
  (FFI `system`/`posix_spawnp`, spaced or macro-built `allow`, inline and
  dotted `[lints]`, cargo `[env] CLIPPY_CONF_DIR` and `rustflags`, a helper
  in another crate using raw identifiers, a commented-out entry) has a
  fixture the script refuses. Known residual evasions: a dependency that
  spawns or calls FFI on the crate's behalf, or re-exports a process API
  under another name; another crate's macro emitting process or `unsafe`
  code; lint or flag overrides outside the repository (a user-level cargo
  configuration, `RUSTFLAGS` or `CLIPPY_CONF_DIR` set in the CI
  environment); and Clippy not being run.

## Positions: data or expression

A string beginning with `$` or `$$` means a field path or a variable only
where the server reads an aggregation expression; elsewhere it is data or
is not accepted at all. The gate's default is expression (system variables
checked, only `ROOT`, `CURRENT`, `REMOVE`, `NOW`, and lower-case pipeline
variables allowed) unless a row below proves data. Every row marked *data*
has a live test showing `"$$USER_ROLES"` comes back as the literal string
(`test_live_mongo_data_positions_return_variables_as_literal_strings`). A
filter takes query operators only; expression operators are accepted inside
`$expr` alone. No component of a field path may start with `$`.

| Position | Server reads `$`/`$$` strings as | Gate |
|---|---|---|
| `collection` (every action) | a collection name | 1-120 characters, no `$`, not `system.*` |
| `max_rows`, `max_tokens`, `timeout_ms` | (tool arguments, never sent as strings) | integers in range |
| find filter, field value | data | data |
| find filter, `$in`/`$nin`/`$all`/`$eq`/... arguments | data | data |
| find filter, `$elemMatch` | data (a query) | data |
| find filter, `$expr` | expression | expression |
| find projection | expression | only 0, 1, true, false, integer `$slice`; strings refused |
| find projection, `$elemMatch` | data (a query) | data |
| find sort | keys only | 1 or -1 only |
| find skip | a number | a number |
| find hint, collation | (not sent) | not accepted |
| count filter | data | data |
| distinct field | a field path (the tool builds `"$field"`) | field path, no component starting with `$` |
| distinct filter | data | data |
| `$match` | data (a query) | data |
| `$match` `$expr` | expression | expression |
| `$project` | expression | expression |
| `$addFields` / `$set` | expression | expression |
| `$unset` | field names | field paths, no component starting with `$` |
| `$group` `_id` and accumulators | expression | expression |
| `$sort` | keys only | 1 or -1 only |
| `$limit`, `$skip` | numbers | numbers |
| `$sample` `size` | number | number |
| `$count` | an output name | expression (conservative) |
| `$sortByCount` | expression | expression |
| `$unwind` `path` and options | a field path, flags | expression (conservative) |
| `$replaceRoot` `newRoot`, `$replaceWith` | expression | expression |
| `$bucket` `groupBy`, `output` | expression | expression |
| `$bucket` `boundaries`, `default` | expressions that must fold to constants | expression (system variables checked; anything else that is not constant the server refuses) |
| `$bucketAuto` `groupBy`, `output` | expression | expression |
| `$bucketAuto` `buckets`, `granularity` | a number, a name | expression (conservative) |
| `$lookup` `from` | a collection name | collection name, no `$` |
| `$lookup` `localField`, `foreignField`, `as` | field paths | field paths, no component starting with `$` |
| `$lookup` `let` | expression | expression |
| `$lookup` `pipeline`, `$facet` pipelines | stages | per stage, as above |
| `$facet` output names | field names | field names, no `$` at the start |
| `$literal` content (any expression) | data | data |

37 positions: 8 data, 15 expression (3 of them conservative, where the
server reads a name or a number), and 14 that take only numbers, keys,
names, or the fixed projection forms.

## Known gaps

- The CI job has not run; the live tests have run only on the build machine
  against MongoDB 8.0.38. Other versions, sharded clusters (`mongos`),
  managed cloud variants, and Windows clients have not been tried.
- `READ_ACTIONS` is the 8.0.38 list. A server version whose `read` role
  grants a new action is refused (fail closed) until the list is updated.
- `listCollections` and `listIndexes` get no `maxTimeMS` from the driver;
  they are bounded by the client deadline only. When the client deadline
  passes, the call is abandoned and the client shut down; the server's own
  `maxTimeMS` stops the command, but no `killOp` is sent.
- No `mongodb+srv://`, replica-set discovery, sockets, client certificates,
  or non-SCRAM authentication. A proposal for SRV (resolve SRV and TXT in
  the tool, apply the TXT options through the same allow-list, pin the
  resolved hosts, require TLS) is left for the owner.
- The driver's handshake sends its name, version, the operating system, and
  the architecture, and labels a Docker or Kubernetes host.

## Consequences

- `Cargo.lock` gains 45 packages and changes none; two of them
  (`mongocrypt`, `mongocrypt-sys`) are listed by Cargo for an optional
  feature and are never built (`cargo tree --target all -i mongocrypt-sys`
  is empty). The macOS arm64 release binary grows from 39,634,416 to
  46,450,288 bytes (+17.2%) after the confirmation round.
- The full surface grows only when the flag is set: the tool definition is
  591 o200k tokens (budget 600); default surfaces and goldens are
  unchanged.
- The allow-list refuses some valid MongoDB (geospatial and text queries,
  `$setWindowFields`, `$densify`, `$graphLookup`, `$unionWith`, window and
  trigonometric operators, aggregation-expression projections in `find`, which are refused);
  every refusal names the key and says to rewrite with listed operators.
