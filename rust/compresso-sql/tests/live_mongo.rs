//! Live MongoDB checks for `compresso_mongo` (E17-52, D117). Each test
//! returns at once unless its variables are set, so the default run needs no
//! server:
//!
//! - `COMPRESSO_MONGO_TEST_ADMIN_URL`: a root account on a server with
//!   authentication on, used only by these tests (through the driver
//!   directly) to create the accounts and data they need;
//! - `COMPRESSO_MONGO_TEST_HOST`: that server's `host:port` as the tool
//!   should reach it;
//! - optionally `COMPRESSO_MONGO_TEST_TLS_HOST` and
//!   `COMPRESSO_MONGO_TEST_TLS_CA`: the `host:port` of a TLS server with
//!   authentication off whose certificate names only `localhost`, and the
//!   private root certificate that signed it.
//!
//! With `COMPRESSO_SQL_TEST_REQUIRE_LIVE=1` (set in CI) a missing variable
//! fails the test instead of skipping it.

use std::time::{Duration, Instant};

use bson::{Document, doc};
use compresso_sql::SqlError;
use compresso_sql::mongo::{MongoAction, MongoOutcome, MongoRequest, MongoService, MongoSettings};
use mongodb::Client;
use serde_json::{Value, json};

const ENV: &str = "COMPRESSO_MONGO_TEST";
const DATABASE: &str = "compresso_probe";
const OTHER_DATABASE: &str = "compresso_other";
const PASSWORD: &str = concat!("pr0be", "-Pa55");

fn variable(name: &str) -> Option<String> {
    let value = std::env::var(name).ok().filter(|value| !value.is_empty());
    if value.is_none()
        && std::env::var("COMPRESSO_SQL_TEST_REQUIRE_LIVE").is_ok_and(|value| value == "1")
    {
        panic!("{name} is not set but COMPRESSO_SQL_TEST_REQUIRE_LIVE=1");
    }
    value
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

/// The live server, set up once per test: accounts and data are created if
/// missing, so the tests can run in any order and more than once.
struct Live {
    host: String,
    admin: Client,
    runtime: tokio::runtime::Runtime,
}

impl Live {
    fn start() -> Option<Self> {
        let admin_url = variable("COMPRESSO_MONGO_TEST_ADMIN_URL")?;
        let host = variable("COMPRESSO_MONGO_TEST_HOST")?;
        let runtime = runtime();
        let admin = runtime
            .block_on(Client::with_uri_str(&admin_url))
            .expect("admin client");
        let live = Self {
            host,
            admin,
            runtime,
        };
        live.setup();
        Some(live)
    }

    fn command(
        &self,
        database: &str,
        command: Document,
    ) -> Result<Document, mongodb::error::Error> {
        self.runtime
            .block_on(async { self.admin.database(database).run_command(command).await })
    }

    fn setup(&self) {
        let users: &[(&str, Vec<Document>)] = &[
            ("reader", vec![doc! {"role": "read", "db": DATABASE}]),
            ("writer", vec![doc! {"role": "readWrite", "db": DATABASE}]),
            ("owner", vec![doc! {"role": "dbOwner", "db": DATABASE}]),
            (
                "monitor",
                vec![
                    doc! {"role": "read", "db": DATABASE},
                    doc! {"role": "clusterMonitor", "db": "admin"},
                ],
            ),
            (
                "wide",
                vec![
                    doc! {"role": "read", "db": DATABASE},
                    doc! {"role": "read", "db": OTHER_DATABASE},
                ],
            ),
            ("custom", vec![doc! {"role": "findOnly", "db": DATABASE}]),
            (
                "custominsert",
                vec![doc! {"role": "findAndInsert", "db": DATABASE}],
            ),
        ];
        let _ = self.command(
            DATABASE,
            doc! {"createRole": "findOnly", "privileges": [{"resource": {"db": DATABASE, "collection": ""}, "actions": ["find", "listCollections"]}], "roles": []},
        );
        let _ = self.command(
            DATABASE,
            doc! {"createRole": "findAndInsert", "privileges": [{"resource": {"db": DATABASE, "collection": ""}, "actions": ["find", "insert"]}], "roles": []},
        );
        for (user, roles) in users {
            let _ = self.command(DATABASE, doc! {"dropUser": *user});
            self.command(
                DATABASE,
                doc! {"createUser": *user, "pwd": PASSWORD, "roles": roles.clone()},
            )
            .expect("create user");
        }
        let items = self
            .admin
            .database(DATABASE)
            .collection::<Document>("items");
        let slow = self.admin.database(DATABASE).collection::<Document>("slow");
        self.runtime.block_on(async {
            items.drop().await.expect("drop items");
            let documents = (0..30)
                .map(|n| doc! {"n": n, "kind": if n % 2 == 0 { "even" } else { "odd" }, "nested": {"depth": n}, "note": "hidden\u{202e}text"})
                .collect::<Vec<_>>();
            items.insert_many(documents).await.expect("seed items");
            if slow.estimated_document_count().await.unwrap_or(0) < 5_000 {
                slow.drop().await.expect("drop slow");
                let documents = (0..5_000)
                    .map(|n| doc! {"s": format!("Value-{n}")})
                    .collect::<Vec<_>>();
                slow.insert_many(documents).await.expect("seed slow");
            }
            let _ = self
                .admin
                .database(DATABASE)
                .collection::<Document>("copy")
                .drop()
                .await;
            // Review fixtures: a stored value with hidden characters, stored
            // `$` keys, a view, and a document with many keys.
            let database = self.admin.database(DATABASE);
            for name in ["evil", "dollar", "wide", "items_view"] {
                let _ = database.collection::<Document>(name).drop().await;
            }
            database
                .collection::<Document>("evil")
                .insert_one(doc! {"s": "12\u{202e}34\u{200b}x"})
                .await
                .expect("seed evil");
            database
                .run_command(doc! {"insert": "dollar", "documents": [
                    {"_id": 1, "$oid": "forged"},
                    {"_id": 2, "x": {"$oid": "65a1b2c3d4e5f60718293a4b"}},
                ]})
                .await
                .expect("seed dollar");
            let mut wide = Document::new();
            for key in 0..5_000 {
                wide.insert(format!("k{key}"), key);
            }
            database
                .collection::<Document>("wide")
                .insert_one(wide)
                .await
                .expect("seed wide");
            database
                .run_command(doc! {"create": "items_view", "viewOn": "items", "pipeline": [{"$match": {"kind": "even"}}]})
                .await
                .expect("create view");
        });
    }

    fn url(&self, user: &str) -> String {
        format!("mongodb://{user}:{PASSWORD}@{}/{DATABASE}", self.host)
    }

    fn service(&self, user: &str) -> MongoService {
        let service = MongoService::new(&MongoSettings::with_value(ENV, Some(&self.url(user))));
        if let Some(problem) = service.configuration_problem() {
            panic!("live setup is not usable: {problem}");
        }
        service
    }

    fn collection_exists(&self, name: &str) -> bool {
        self.runtime.block_on(async {
            self.admin
                .database(DATABASE)
                .list_collection_names()
                .await
                .expect("list")
                .iter()
                .any(|existing| existing == name)
        })
    }

    fn aggregate_total(&self) -> i64 {
        let status = self
            .command("admin", doc! {"serverStatus": 1})
            .expect("serverStatus");
        let total = status
            .get_document("metrics")
            .and_then(|metrics| metrics.get_document("commands"))
            .and_then(|commands| commands.get_document("aggregate"))
            .map(|aggregate| aggregate.get("total").cloned());
        match total {
            Ok(Some(bson::Bson::Int64(total))) => total,
            Ok(Some(bson::Bson::Int32(total))) => i64::from(total),
            other => panic!("aggregate counter unreadable: {other:?}"),
        }
    }
}

fn request(action: MongoAction, max_rows: usize, timeout_ms: u64) -> MongoRequest {
    MongoRequest {
        action,
        max_rows,
        timeout: Duration::from_millis(timeout_ms),
    }
}

fn documents(outcome: MongoOutcome) -> (Vec<Value>, bool) {
    match outcome {
        MongoOutcome::Documents(result) => (result.documents, result.truncated),
        other => panic!("unexpected outcome {other:?}"),
    }
}

fn failure(result: Result<MongoOutcome, SqlError>) -> SqlError {
    let error = result.expect_err("expected an error");
    assert!(!error.message().contains(PASSWORD), "{error}");
    error
}

#[test]
fn test_live_mongo_reads_work_for_a_read_account() {
    let Some(live) = Live::start() else { return };
    let service = live.service("reader");
    let collections = service
        .execute(&request(MongoAction::Collections, 100, 5_000))
        .expect("collections");
    let MongoOutcome::Collections(collections) = collections else {
        panic!("collections");
    };
    assert!(
        collections
            .collections
            .iter()
            .any(|entry| entry.name == "items")
    );

    let find = MongoAction::Find {
        collection: "items".to_owned(),
        filter: json!({"kind": "even", "n": {"$gte": 4}}),
        projection: Some(json!({"_id": 0, "n": 1, "note": 1})),
        sort: Some(json!({"n": 1})),
        skip: 1,
    };
    let (rows, truncated) = documents(service.execute(&request(find, 3, 5_000)).expect("find"));
    assert_eq!(rows.len(), 3);
    assert!(truncated);
    assert_eq!(rows[0]["n"], 6);
    assert_eq!(rows[0]["note"], "hidden\\u202Etext");

    let aggregate = MongoAction::Aggregate {
        collection: "items".to_owned(),
        pipeline: json!([
            {"$match": {"n": {"$lt": 10}}},
            {"$group": {"_id": "$kind", "total": {"$sum": "$n"}}},
            {"$sort": {"_id": 1}}
        ]),
    };
    let (rows, _) = documents(
        service
            .execute(&request(aggregate, 10, 5_000))
            .expect("aggregate"),
    );
    assert_eq!(
        rows,
        vec![
            json!({"_id": "even", "total": 20}),
            json!({"_id": "odd", "total": 25})
        ]
    );

    let count = service
        .execute(&request(
            MongoAction::Count {
                collection: "items".to_owned(),
                filter: json!({"kind": "odd"}),
            },
            10,
            5_000,
        ))
        .expect("count");
    assert!(matches!(count, MongoOutcome::Count(result) if result.count == 15));

    let (values, _) = documents(
        service
            .execute(&request(
                MongoAction::Distinct {
                    collection: "items".to_owned(),
                    field: "kind".to_owned(),
                    filter: json!({}),
                },
                10,
                5_000,
            ))
            .expect("distinct"),
    );
    assert_eq!(values, vec![json!("even"), json!("odd")]);

    let describe = service
        .execute(&request(
            MongoAction::Describe {
                collection: "items".to_owned(),
            },
            10,
            5_000,
        ))
        .expect("describe");
    let MongoOutcome::Describe(describe) = describe else {
        panic!("describe");
    };
    assert_eq!(describe.sampled, 30);
    assert!(
        describe
            .fields
            .iter()
            .any(|field| field.path == "nested.depth")
    );
    assert!(describe.indexes.iter().any(|index| index.name == "_id_"));

    // Called from inside a runtime: no panic.
    let inside = runtime()
        .block_on(async { service.execute(&request(MongoAction::Collections, 10, 5_000)) });
    assert!(inside.is_ok(), "{inside:?}");
}

/// Every allowed stage, and a multi-field sort in order, runs for a `read`
/// account: the allow-list sends nothing the server or the role refuses.
#[test]
fn test_live_mongo_every_allowed_stage_runs_for_a_read_account() {
    let Some(live) = Live::start() else { return };
    let service = live.service("reader");
    let stages = [
        json!({"$addFields": {"double": {"$multiply": ["$n", 2]}}}),
        json!({"$bucket": {"groupBy": "$n", "boundaries": [0, 10, 20, 30], "output": {"c": {"$sum": 1}}}}),
        json!({"$bucketAuto": {"groupBy": "$n", "buckets": 3}}),
        json!({"$count": "total"}),
        json!({"$facet": {"evens": [{"$match": {"kind": "even"}}, {"$count": "n"}]}}),
        json!({"$group": {"_id": "$kind", "avg": {"$avg": "$n"}, "all": {"$push": "$n"}}}),
        json!({"$limit": 2}),
        json!({"$lookup": {"from": "items", "localField": "n", "foreignField": "n", "as": "self"}}),
        json!({"$match": {"n": {"$in": [1, 2, 3]}}}),
        json!({"$project": {"_id": 0, "n": 1, "kind": 1}}),
        json!({"$replaceRoot": {"newRoot": "$nested"}}),
        json!({"$replaceWith": {"n": "$n"}}),
        json!({"$sample": {"size": 3}}),
        json!({"$set": {"flag": {"$cond": [{"$gt": ["$n", 5]}, true, false]}}}),
        json!({"$skip": 28}),
        json!({"$sort": [{"kind": 1}, {"n": -1}]}),
        json!({"$sortByCount": "$kind"}),
        json!({"$unset": "note"}),
        json!({"$unwind": "$self"}),
    ];
    assert_eq!(stages.len(), compresso_sql::mongo::gate::STAGES.len());
    for stage in stages {
        let pipeline = if stage.get("$unwind").is_some() {
            json!([{"$lookup": {"from": "items", "localField": "n", "foreignField": "n", "as": "self"}}, stage])
        } else {
            json!([stage])
        };
        let result = service.execute(&request(
            MongoAction::Aggregate {
                collection: "items".to_owned(),
                pipeline: pipeline.clone(),
            },
            50,
            5_000,
        ));
        assert!(result.is_ok(), "{pipeline}: {result:?}");
    }
    let find = MongoAction::Find {
        collection: "items".to_owned(),
        filter: json!({}),
        projection: Some(json!({"_id": 0, "kind": 1, "n": 1})),
        sort: Some(json!([{"kind": 1}, {"n": -1}])),
        skip: 0,
    };
    let (rows, _) = documents(service.execute(&request(find, 2, 5_000)).expect("find"));
    assert_eq!(
        rows,
        vec![
            json!({"n": 28, "kind": "even"}),
            json!({"n": 26, "kind": "even"})
        ]
    );
}

#[test]
fn test_live_mongo_accounts_beyond_read_are_refused() {
    let Some(live) = Live::start() else { return };
    let collections = request(MongoAction::Collections, 10, 5_000);
    for user in ["writer", "owner", "monitor", "wide", "custominsert"] {
        let error = failure(live.service(user).execute(&collections));
        assert_eq!(error.code(), "mongo_account_refused", "{user}: {error}");
        assert!(!error.message().contains(user), "{error}");
        println!("{user}: {}", error.message());
    }
    // A custom role with only read actions on the database passes.
    live.service("custom")
        .execute(&collections)
        .expect("custom read-only role");
}

/// The account check alone stops a writing pipeline: the allow-list is
/// skipped (`test-hooks`), and `$out` still never runs for a `readWrite`
/// account. For a `read` account the server refuses it too.
#[test]
fn test_live_mongo_the_account_check_alone_stops_out_for_a_write_account() {
    let Some(live) = Live::start() else { return };
    let writer = live.service("writer");
    let error = failure(writer.execute_unchecked_pipeline_for_tests(
        "items",
        vec![doc! {"$out": "copy"}],
        Duration::from_secs(5),
    ));
    assert_eq!(error.code(), "mongo_account_refused", "{error}");
    assert!(!live.collection_exists("copy"));

    let reader = live.service("reader");
    let error = failure(reader.execute_unchecked_pipeline_for_tests(
        "items",
        vec![doc! {"$out": "copy"}],
        Duration::from_secs(5),
    ));
    assert_eq!(error.code(), "mongo_permission", "{error}");
    assert!(!live.collection_exists("copy"));
}

#[test]
fn test_live_mongo_out_and_merge_never_reach_the_server() {
    let Some(live) = Live::start() else { return };
    let service = live.service("reader");
    let before = live.aggregate_total();
    for pipeline in [
        json!([{"$out": "copy"}]),
        json!([{"$merge": {"into": "copy"}}]),
        json!([{"$match": {}}, {"$facet": {"a": [{"$out": "copy"}]}}]),
        json!([{"$lookup": {"from": "items", "as": "x", "pipeline": [{"$merge": {"into": "copy"}}]}}]),
        json!([{"$addFields": {"x": {"$function": {"body": "function(){return 1}", "args": [], "lang": "js"}}}}]),
    ] {
        let error = failure(service.execute(&request(
            MongoAction::Aggregate {
                collection: "items".to_owned(),
                pipeline,
            },
            10,
            5_000,
        )));
        assert_eq!(error.code(), "mongo_refused", "{error}");
    }
    let error = failure(service.execute(&request(
        MongoAction::Find {
            collection: "items".to_owned(),
            filter: json!({"$where": "sleep(5000) || true"}),
            projection: None,
            sort: None,
            skip: 0,
        },
        10,
        5_000,
    )));
    assert_eq!(error.code(), "mongo_refused", "{error}");
    assert_eq!(
        live.aggregate_total(),
        before,
        "a refused pipeline reached the server"
    );
    assert!(!live.collection_exists("copy"));
}

/// What the refusals guard against, observed on this server through the
/// root account (never through the tool): `$where`, `$function`, and
/// `$accumulator` run JavaScript, and `$out` and `$merge` write.
#[test]
fn test_live_mongo_the_refused_operators_do_run_javascript_and_write_here() {
    let Some(live) = Live::start() else { return };
    let items = live
        .admin
        .database(DATABASE)
        .collection::<Document>("items");
    live.runtime.block_on(async {
        let mut cursor = items
            .find(doc! {"$where": "this.n === 3"})
            .await
            .expect("$where runs");
        assert!(futures_util::TryStreamExt::try_next(&mut cursor).await.expect("row").is_some());
        let pipeline = vec![
            doc! {"$limit": 1},
            doc! {"$addFields": {"js": {"$function": {"body": "function(){return 41 + 1}", "args": [], "lang": "js"}}}},
            doc! {"$group": {"_id": null, "acc": {"$accumulator": {"init": "function(){return 0}", "accumulate": "function(s){return s+1}", "accumulateArgs": [], "merge": "function(a,b){return a+b}", "lang": "js"}}}},
        ];
        let mut cursor = items.aggregate(pipeline).await.expect("JavaScript runs");
        let row = futures_util::TryStreamExt::try_next(&mut cursor)
            .await
            .expect("row")
            .expect("one row");
        assert_eq!(row.get("acc"), Some(&bson::Bson::Double(1.0)));
        items
            .aggregate(vec![doc! {"$limit": 1}, doc! {"$out": "copy"}])
            .await
            .expect("$out writes");
    });
    assert!(live.collection_exists("copy"));
    live.runtime.block_on(async {
        live.admin
            .database(DATABASE)
            .collection::<Document>("copy")
            .drop()
            .await
            .expect("drop copy");
    });
}

#[test]
fn test_live_mongo_a_slow_command_returns_the_timeout_code() {
    let Some(live) = Live::start() else { return };
    let service = live.service("reader");
    // A self-join on an unindexed, computed key: 5,000 x 5,000 expression
    // evaluations, far past 300 ms.
    let slow = request(
        MongoAction::Aggregate {
            collection: "slow".to_owned(),
            pipeline: json!([
                {"$lookup": {
                    "from": "slow",
                    "let": {"key": "$s"},
                    "pipeline": [{"$match": {"$expr": {"$eq": [{"$toLower": "$s"}, {"$toLower": "$$key"}]}}}],
                    "as": "same"
                }},
                {"$count": "n"}
            ]),
        },
        10,
        300,
    );
    let started = Instant::now();
    let error = failure(service.execute(&slow));
    let elapsed = started.elapsed();
    assert_eq!(error.code(), "mongo_timeout", "{error}");
    assert!(elapsed < service.call_limit(slow.timeout), "{elapsed:?}");
    println!(
        "slow command stopped after {elapsed:?}: {}",
        error.message()
    );
}

#[test]
fn test_live_mongo_tls_checks_the_host_name_against_the_private_root() {
    let Some(host) = variable("COMPRESSO_MONGO_TEST_TLS_HOST") else {
        return;
    };
    let Some(ca) = variable("COMPRESSO_MONGO_TEST_TLS_CA") else {
        return;
    };
    let port = host
        .rsplit_once(':')
        .map_or("27017", |(_, port)| port)
        .to_owned();
    // The certificate names only localhost: reached as 127.0.0.1, TLS must
    // refuse it.
    let wrong =
        format!("mongodb://reader:{PASSWORD}@127.0.0.1:{port}/{DATABASE}?tls=true&tlsCAFile={ca}");
    let service = MongoService::new(&MongoSettings::with_value(ENV, Some(&wrong)));
    let error = failure(service.execute(&request(MongoAction::Collections, 10, 2_000)));
    assert_eq!(error.code(), "mongo_connection", "{error}");
    assert!(!error.message().contains("127.0.0.1"), "{error}");
    println!("wrong host name: {}", error.message());
    // Control: reached as localhost the handshake succeeds. That server runs
    // without access control, where a read user still authenticates and
    // reports only read privileges; the enforcement probe must refuse it.
    let runtime = runtime();
    runtime.block_on(async {
        let admin = Client::with_uri_str(format!(
            "mongodb://localhost:{port}/?tls=true&tlsCAFile={ca}&directConnection=true"
        ))
        .await
        .expect("TLS admin client");
        let database = admin.database(DATABASE);
        let _ = database.run_command(doc! {"dropUser": "reader"}).await;
        database
            .run_command(doc! {"createUser": "reader", "pwd": PASSWORD, "roles": [{"role": "read", "db": DATABASE}]})
            .await
            .expect("create user without access control");
    });
    let right =
        format!("mongodb://reader:{PASSWORD}@localhost:{port}/{DATABASE}?tls=true&tlsCAFile={ca}");
    let service = MongoService::new(&MongoSettings::with_value(ENV, Some(&right)));
    let error = failure(service.execute(&request(MongoAction::Collections, 10, 2_000)));
    println!("right host name: [{}] {}", error.code(), error.message());
    assert_eq!(error.code(), "mongo_account_refused", "{error}");
    assert!(
        error.message().contains("does not enforce access control"),
        "{error}"
    );
}

/// E17-52 review: server error text, stored `$` keys, system variables,
/// views, and a wide sample, against the live server.
#[test]
fn test_live_mongo_review_cases() {
    let Some(live) = Live::start() else { return };
    let service = live.service("reader");

    // A failed cast quotes the stored value: hidden characters escaped, no
    // reply document, no database name.
    let error = failure(service.execute(&request(
        MongoAction::Aggregate {
            collection: "evil".to_owned(),
            pipeline: json!([{"$project": {"x": {"$toInt": "$s"}}}]),
        },
        10,
        5_000,
    )));
    assert_eq!(error.code(), "mongo_error", "{error}");
    let message = error.message();
    println!("cast error: {message}");
    assert!(
        !message.contains('\u{202e}') && !message.contains('\u{200b}'),
        "{message}"
    );
    assert!(
        !message.contains("RawDocument") && !message.contains(DATABASE),
        "{message}"
    );
    assert!(message.contains("code "), "{message}");

    // Stored `$` keys are shown escaped; a real object id is not.
    let (rows, _) = documents(
        service
            .execute(&request(
                MongoAction::Find {
                    collection: "dollar".to_owned(),
                    filter: json!({}),
                    projection: None,
                    sort: Some(json!({"_id": 1})),
                    skip: 0,
                },
                10,
                5_000,
            ))
            .expect("stored $ keys read"),
    );
    assert_eq!(rows[0], json!({"_id": 1, "\\$oid": "forged"}));
    assert_eq!(
        rows[1],
        json!({"_id": 2, "x": {"\\$oid": "65a1b2c3d4e5f60718293a4b"}})
    );

    // $$USER_ROLES never reaches the server.
    let error = failure(service.execute(&request(
        MongoAction::Aggregate {
            collection: "items".to_owned(),
            pipeline: json!([{"$project": {"r": "$$USER_ROLES"}}]),
        },
        10,
        5_000,
    )));
    assert_eq!(error.code(), "mongo_refused", "{error}");

    // Views are listed, but reading one (directly or through $lookup) is
    // refused.
    let MongoOutcome::Collections(collections) = service
        .execute(&request(MongoAction::Collections, 100, 5_000))
        .expect("collections")
    else {
        panic!("collections");
    };
    assert!(
        collections
            .collections
            .iter()
            .any(|entry| entry.name == "items_view" && entry.kind == "view")
    );
    for action in [
        MongoAction::Find {
            collection: "items_view".to_owned(),
            filter: json!({}),
            projection: None,
            sort: None,
            skip: 0,
        },
        MongoAction::Describe {
            collection: "items_view".to_owned(),
        },
        MongoAction::Aggregate {
            collection: "items".to_owned(),
            pipeline: json!([{"$lookup": {"from": "items_view", "as": "v", "pipeline": []}}]),
        },
    ] {
        let error = failure(service.execute(&request(action, 10, 5_000)));
        assert_eq!(error.code(), "mongo_view_refused", "{error}");
    }

    // A wide document: the field list stays bounded.
    let MongoOutcome::Describe(describe) = service
        .execute(&request(
            MongoAction::Describe {
                collection: "wide".to_owned(),
            },
            10,
            5_000,
        ))
        .expect("describe wide")
    else {
        panic!("describe");
    };
    assert_eq!(
        describe.fields.len(),
        compresso_sql::mongo::MAX_DESCRIBED_FIELDS
    );
    assert!(
        describe.fields_omitted >= 4_800,
        "{}",
        describe.fields_omitted
    );
}
