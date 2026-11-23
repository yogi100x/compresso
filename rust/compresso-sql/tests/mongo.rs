//! `compresso_mongo` without a server (E17-52, D117): every refusal of the
//! allow-list happens before any connection, configuration problems never
//! echo the URL, and a stalled peer cannot hold a call past its limit.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use compresso_sql::SqlError;
use compresso_sql::mongo::{MongoAction, MongoOutcome, MongoRequest, MongoService, MongoSettings};
use serde_json::{Value, json};

const ENV: &str = "COMPRESSO_MONGO_TEST";
const PASSWORD: &str = concat!("c4n4ry", "-Pa55");

/// A loopback listener that counts the connections it accepts and holds
/// each one open without answering.
struct Counter {
    port: u16,
    accepted: Arc<AtomicUsize>,
}

impl Counter {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("address").port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&accepted);
        std::thread::spawn(move || {
            let mut held: Vec<TcpStream> = Vec::new();
            for stream in listener.incoming().flatten() {
                seen.fetch_add(1, Ordering::SeqCst);
                let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
                let mut buffer = [0_u8; 512];
                let mut stream = stream;
                let _ = stream.read(&mut buffer);
                held.push(stream);
            }
        });
        Self { port, accepted }
    }

    fn url(&self) -> String {
        format!("mongodb://reader:{PASSWORD}@127.0.0.1:{}/probe", self.port)
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

fn service(url: &str) -> MongoService {
    MongoService::new(&MongoSettings::with_value(ENV, Some(url)))
        .with_connect_timeout(Duration::from_millis(500))
}

fn request(action: MongoAction) -> MongoRequest {
    MongoRequest {
        action,
        max_rows: 10,
        timeout: Duration::from_millis(500),
    }
}

fn aggregate(pipeline: Value) -> MongoRequest {
    request(MongoAction::Aggregate {
        collection: "items".to_owned(),
        pipeline,
    })
}

fn find(filter: Value) -> MongoRequest {
    request(MongoAction::Find {
        collection: "items".to_owned(),
        filter,
        projection: None,
        sort: None,
        skip: 0,
    })
}

fn error(result: Result<MongoOutcome, SqlError>) -> SqlError {
    match result {
        Ok(outcome) => panic!("expected an error, got {outcome:?}"),
        Err(error) => {
            assert!(!error.message().contains(PASSWORD), "{error}");
            error
        }
    }
}

#[test]
fn test_every_refusal_happens_before_any_connection() {
    let counter = Counter::start();
    let service = service(&counter.url());
    let refused = [
        aggregate(json!([{"$out": "copy"}])),
        aggregate(json!([{"$match": {}}, {"$merge": {"into": "copy"}}])),
        aggregate(json!([{"$facet": {"a": [{"$out": "copy"}]}}])),
        aggregate(json!([{"$lookup": {"from": "b", "as": "c", "pipeline": [{"$out": "x"}]}}])),
        aggregate(
            json!([{"$lookup": {"from": {"db": "admin", "coll": "system.users"}, "as": "u", "pipeline": []}}]),
        ),
        aggregate(json!([{"$currentOp": {}}])),
        aggregate(json!([{"$listSessions": {}}])),
        aggregate(json!([{"$collStats": {"count": {}}}])),
        aggregate(json!([{"$documents": [{"a": 1}]}])),
        aggregate(json!([{"$changeStream": {}}])),
        aggregate(json!([{"$unionWith": "other"}])),
        aggregate(json!([{"$graphLookup": {"from": "a"}}])),
        aggregate(json!([{"$group": {"_id": null, "x": {"$accumulator": {}}}}])),
        aggregate(
            json!([{"$addFields": {"x": {"$function": {"body": "f", "args": [], "lang": "js"}}}}]),
        ),
        find(json!({"$where": "sleep(1000)"})),
        find(json!({"$expr": {"$function": {"body": "f", "args": [], "lang": "js"}}})),
        find(json!({"a": {"$code": "function(){}"}})),
        find(json!({"$comment": "x"})),
        request(MongoAction::Find {
            collection: "system.users".to_owned(),
            filter: json!({}),
            projection: None,
            sort: None,
            skip: 0,
        }),
        request(MongoAction::Distinct {
            collection: "items".to_owned(),
            field: "$where".to_owned(),
            filter: json!({}),
        }),
    ];
    for request in &refused {
        let error = error(service.execute(request));
        assert_eq!(error.code(), "mongo_refused", "{request:?}: {error}");
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(counter.accepted(), 0, "a refused call opened a connection");

    // Control: an allowed call does reach the listener, so the count above
    // means something.
    let error = error(service.execute(&find(json!({"a": 1}))));
    assert!(
        matches!(error.code(), "mongo_connection" | "mongo_timeout"),
        "{error}"
    );
    assert!(counter.accepted() > 0, "the allowed call never connected");
}

#[test]
fn test_unusable_values_are_not_configured_and_never_echoed() {
    for value in [
        None,
        Some(format!(
            "mongodb+srv://reader:{PASSWORD}@cluster.example.com/probe"
        )),
        Some(format!(
            "mongodb://reader:{PASSWORD}@a.example.com,b.example.com/probe"
        )),
        Some(format!(
            "mongodb://reader:{PASSWORD}@db.example.com/probe?tls=false"
        )),
        Some(format!(
            "mongodb://reader:{PASSWORD}@db.example.com/probe?tlsInsecure=true"
        )),
        Some(format!("mongodb://reader:{PASSWORD}@db.example.com/admin")),
        Some(format!("postgres://reader:{PASSWORD}@db.example.com/probe")),
    ] {
        let service = MongoService::new(&MongoSettings::with_value(ENV, value.as_deref()));
        assert!(!service.is_configured());
        let problem = service.configuration_problem().expect("problem").to_owned();
        assert!(
            !problem.contains(PASSWORD) && !problem.contains("example.com"),
            "{problem}"
        );
        let error = error(service.execute(&find(json!({}))));
        assert_eq!(error.code(), "mongo_not_configured");
        assert!(error.message().contains("--mongo-env"), "{error}");
        assert!(!format!("{service:?}").contains(PASSWORD));
    }
    let bad_name = MongoService::new(&MongoSettings::with_value(
        "COMPRESSO_SQL_X",
        Some("mongodb://reader:pw@localhost/probe"),
    ));
    assert!(
        bad_name
            .configuration_problem()
            .unwrap()
            .contains("COMPRESSO_MONGO_")
    );
    let settings = MongoSettings::with_value(
        ENV,
        Some(&format!("mongodb://reader:{PASSWORD}@localhost/probe")),
    );
    assert!(!format!("{settings:?}").contains(PASSWORD));
    let function_host = MongoService::new(&settings.with_environment(&["K_SERVICE"]));
    assert!(
        function_host
            .configuration_problem()
            .unwrap()
            .contains("K_SERVICE")
    );
}

#[test]
fn test_out_of_range_arguments_are_invalid_before_connecting() {
    let counter = Counter::start();
    let service = service(&counter.url());
    for request in [
        MongoRequest {
            max_rows: 0,
            ..find(json!({}))
        },
        MongoRequest {
            max_rows: 1_001,
            ..find(json!({}))
        },
        MongoRequest {
            timeout: Duration::ZERO,
            ..find(json!({}))
        },
        MongoRequest {
            timeout: Duration::from_millis(30_001),
            ..find(json!({}))
        },
        request(MongoAction::Find {
            collection: "items".to_owned(),
            filter: json!({}),
            projection: None,
            sort: None,
            skip: 1_000_001,
        }),
    ] {
        let error = error(service.execute(&request));
        assert_eq!(error.code(), "invalid_arguments", "{error}");
    }
    assert_eq!(counter.accepted(), 0);
}

#[test]
fn test_a_stalled_peer_cannot_hold_a_call_past_its_limit() {
    let counter = Counter::start();
    let service = service(&counter.url());
    let request = find(json!({}));
    let limit = service.call_limit(request.timeout);
    let started = Instant::now();
    let error = error(service.execute(&request));
    assert!(
        started.elapsed() <= limit + Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert!(
        matches!(error.code(), "mongo_connection" | "mongo_timeout"),
        "{error}"
    );
    assert!(!error.message().contains("127.0.0.1"), "{error}");
}

#[test]
fn test_a_call_from_inside_an_async_context_does_not_panic() {
    let counter = Counter::start();
    let service = service(&counter.url());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let error = runtime.block_on(async { error(service.execute(&find(json!({})))) });
    assert!(
        matches!(error.code(), "mongo_connection" | "mongo_timeout"),
        "{error}"
    );
}
