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
run**: the branch has not been pushed. The eight live tests in
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
     `$slice`, `$elemMatch`. *Sorts*: 1 or -1 only; several fields only as
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
- **Every call**: makes the server log a `connectionStatus` and an
  `Unauthorized` `hostInfo` (an audit log records both); reads role names
  through `$$USER_ROLES` if it asks. Calls run one at a time per server, so
  a slow call delays every other tool until it ends.
- **Reading a view runs the view's stored pipeline**, which an
  administrator wrote and the allow-list never sees (a view may use
  `$function`); the account check still refuses any account that could
  write.
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
  46,305,392 bytes (+16.8%).
- The full surface grows only when the flag is set: the tool definition is
  591 o200k tokens (budget 600); default surfaces and goldens are
  unchanged.
- The allow-list refuses some valid MongoDB (geospatial and text queries,
  `$setWindowFields`, `$densify`, `$graphLookup`, `$unionWith`, window and
  trigonometric operators, aggregation-expression projections in `find`);
  every refusal names the key and says to rewrite with listed operators.
