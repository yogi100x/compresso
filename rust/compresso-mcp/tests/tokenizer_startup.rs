use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use compresso_core::{
    Config, Engine, TokenProvider, TokenizerInitialization, counter_for, initialize_tokenizers,
};
use compresso_mcp::{McpDeps, serve_io_with_tokenizer_startup};
use compresso_store::Store;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;

const IO_TIMEOUT: Duration = Duration::from_secs(3);

#[tokio::test]
async fn test_tokenizer_failure_keeps_passthrough_mcp_alive() {
    let temp = TempDir::new().expect("temporary directory should be created");
    let config = Config::default();
    let deps = McpDeps {
        store: Store::open(&temp.path().join("compresso.db")).expect("store should open"),
        engine: Engine::new(
            config.compression.mode,
            counter_for(TokenProvider::Heuristic),
        ),
        config,
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let startup_calls = Arc::clone(&calls);
    let (client, server) = tokio::io::duplex(16 * 1024);
    let (server_read, server_write) = tokio::io::split(server);
    let server = tokio::spawn(serve_io_with_tokenizer_startup(
        deps,
        server_read,
        server_write,
        move || {
            startup_calls.fetch_add(1, Ordering::SeqCst);
            TokenizerInitialization {
                initialized: Vec::new(),
                fallback: vec![TokenProvider::OpenAiO200k],
            }
        },
    ));
    let (client_read, mut client_write) = tokio::io::split(client);
    let mut client_read = BufReader::new(client_read);

    send(
        &mut client_write,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "startup-test", "version": "1.0.0" }
            }
        }),
    )
    .await;
    let response = receive(&mut client_read).await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(response["result"]["serverInfo"]["name"], "compresso");

    client_write
        .shutdown()
        .await
        .expect("client input should shut down");
    drop(client_write);
    timeout(IO_TIMEOUT, server)
        .await
        .expect("server should exit after EOF")
        .expect("server task should not panic")
        .expect("server should exit cleanly");
}

#[test]
fn test_warm_tokenizer_startup_p95_under_two_hundred_ms() {
    let providers = [
        TokenProvider::OpenAiO200k,
        TokenProvider::AnthropicCalibrated,
    ];
    let initial = initialize_tokenizers(&providers);
    assert!(initial.all_ready());

    let mut samples = (0..100)
        .map(|_| {
            let started = Instant::now();
            let report = initialize_tokenizers(&providers);
            assert!(report.all_ready());
            started.elapsed()
        })
        .collect::<Vec<_>>();
    samples.sort_unstable();

    assert!(
        samples[94] < Duration::from_millis(200),
        "warm tokenizer startup P95 was {:?}",
        samples[94]
    );
}

async fn send(writer: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>, message: &Value) {
    let mut frame = serde_json::to_vec(message).expect("request should serialize");
    frame.push(b'\n');
    timeout(IO_TIMEOUT, writer.write_all(&frame))
        .await
        .expect("request write should not time out")
        .expect("request should be written");
}

async fn receive(reader: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>) -> Value {
    let mut line = String::new();
    let bytes = timeout(IO_TIMEOUT, reader.read_line(&mut line))
        .await
        .expect("response read should not time out")
        .expect("response should be readable");
    assert!(bytes > 0, "server closed before sending a response");
    serde_json::from_str(&line).expect("response should be valid JSON-RPC")
}
