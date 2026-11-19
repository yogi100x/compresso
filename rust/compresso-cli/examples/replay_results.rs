//! Replays recorded Compresso tool calls against a built `compresso` binary
//! and reports how many characters each tool's results put into context
//! (E17-33, D98).
//!
//! ```text
//! cargo run -p compresso-cli --example replay_results -- \
//!     --bin target/release/compresso --root <repo> --calls <file> \
//!     [--tools lean|full] [--results text|json] [--only compresso_read,compresso_grep] \
//!     [--dump <dir>]
//! ```
//!
//! `--calls` is either a JSON array of `{"tool": ..., "arguments": {...}}`
//! objects or a client stream-JSON transcript, one event per line, whose
//! `tool_use` blocks name Compresso tools (any `mcp__<server>__` prefix is
//! dropped). The binary runs as `compresso mcp serve --root <repo>` with the
//! caller's environment, so set `HOME` to a scratch directory whose store
//! already indexes the repository. Comparing two binaries on the same calls
//! and repository state gives the before and after of a result-size change.
//! The summary is one JSON document on stdout; `--dump` also writes each
//! result to `<dir>/<index>-<tool>.txt`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// One recorded call.
#[derive(Clone, Debug, PartialEq)]
struct Call {
    tool: String,
    arguments: Value,
}

/// Per-tool totals of the replayed results.
#[derive(Clone, Debug, Default, PartialEq)]
struct ToolTotals {
    calls: usize,
    errors: usize,
    chars: usize,
}

/// The Compresso calls recorded in `text`: a JSON array of
/// `{tool, arguments}` or a stream-JSON transcript.
fn parse_calls(text: &str) -> Result<Vec<Call>, String> {
    if text.trim_start().starts_with('[') {
        let values: Vec<Value> =
            serde_json::from_str(text).map_err(|error| format!("calls array: {error}"))?;
        return values
            .into_iter()
            .map(|value| {
                let tool = value["tool"]
                    .as_str()
                    .ok_or("every call needs a string 'tool'")?;
                Ok(Call {
                    tool: tool.to_owned(),
                    arguments: value.get("arguments").cloned().unwrap_or_else(|| json!({})),
                })
            })
            .collect();
    }
    let mut calls = Vec::new();
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(blocks) = event["message"]["content"].as_array() else {
            continue;
        };
        for block in blocks {
            if block["type"] != "tool_use" {
                continue;
            }
            let name = block["name"].as_str().unwrap_or_default();
            let tool = name.rsplit("__").next().unwrap_or(name);
            if tool.starts_with("compresso_") {
                calls.push(Call {
                    tool: tool.to_owned(),
                    arguments: block.get("input").cloned().unwrap_or_else(|| json!({})),
                });
            }
        }
    }
    Ok(calls)
}

/// Adds one result to the per-tool totals.
fn record(totals: &mut BTreeMap<String, ToolTotals>, tool: &str, text: &str, is_error: bool) {
    let entry = totals.entry(tool.to_owned()).or_default();
    entry.calls += 1;
    entry.errors += usize::from(is_error);
    entry.chars += text.chars().count();
}

fn summary(totals: &BTreeMap<String, ToolTotals>) -> Value {
    let tools = totals
        .iter()
        .map(|(tool, totals)| {
            (
                tool.clone(),
                json!({ "calls": totals.calls, "errors": totals.errors, "chars": totals.chars }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    json!({
        "tools": tools,
        "calls": totals.values().map(|totals| totals.calls).sum::<usize>(),
        "chars": totals.values().map(|totals| totals.chars).sum::<usize>(),
    })
}

struct Options {
    bin: PathBuf,
    root: PathBuf,
    calls: PathBuf,
    tools: String,
    results: Option<String>,
    only: Option<Vec<String>>,
    dump: Option<PathBuf>,
}

fn parse_options(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let (mut bin, mut root, mut calls, mut dump) = (None, None, None, None);
    let (mut tools, mut results, mut only) = ("lean".to_owned(), None, None);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--bin" => bin = Some(PathBuf::from(value()?)),
            "--root" => root = Some(PathBuf::from(value()?)),
            "--calls" => calls = Some(PathBuf::from(value()?)),
            "--tools" => tools = value()?,
            "--results" => results = Some(value()?),
            "--only" => only = Some(value()?.split(',').map(str::to_owned).collect()),
            "--dump" => dump = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Options {
        bin: bin.ok_or("--bin is required")?,
        root: root.ok_or("--root is required")?,
        calls: calls.ok_or("--calls is required")?,
        tools,
        results,
        only,
        dump,
    })
}

fn run(options: &Options) -> Result<Value, String> {
    let text = std::fs::read_to_string(&options.calls)
        .map_err(|error| format!("{}: {error}", options.calls.display()))?;
    let calls = parse_calls(&text)?
        .into_iter()
        .filter(|call| {
            options
                .only
                .as_ref()
                .is_none_or(|only| only.contains(&call.tool))
        })
        .collect::<Vec<_>>();
    let mut command = Command::new(&options.bin);
    command
        .args(["mcp", "serve", "--root"])
        .arg(&options.root)
        .args(["--tools", &options.tools]);
    if let Some(results) = &options.results {
        command.args(["--results", results]);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("{}: {error}", options.bin.display()))?;
    let mut input = child.stdin.take().ok_or("no stdin")?;
    let mut output = BufReader::new(child.stdout.take().ok_or("no stdout")?);
    let mut next_id = 0_u64;
    let mut request = |method: &str, params: Value| -> Result<Value, String> {
        let notification = method.starts_with("notifications/");
        next_id += 1;
        let frame = if notification {
            json!({ "jsonrpc": "2.0", "method": method })
        } else {
            json!({ "jsonrpc": "2.0", "id": next_id, "method": method, "params": params })
        };
        writeln!(input, "{frame}").map_err(|error| error.to_string())?;
        input.flush().map_err(|error| error.to_string())?;
        if notification {
            return Ok(Value::Null);
        }
        loop {
            let mut line = String::new();
            if output
                .read_line(&mut line)
                .map_err(|error| error.to_string())?
                == 0
            {
                return Err("server closed its output".to_owned());
            }
            let reply: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
            if reply["id"] == next_id {
                return Ok(reply);
            }
        }
    };
    request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "compresso-replay", "version": "1.0.0" }
        }),
    )?;
    request("notifications/initialized", Value::Null)?;
    let mut totals = BTreeMap::new();
    for (index, call) in calls.iter().enumerate() {
        let reply = request(
            "tools/call",
            json!({ "name": call.tool, "arguments": call.arguments }),
        )?;
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .map_or_else(|| reply.to_string(), str::to_owned);
        let is_error = reply["result"]["isError"] == true || reply.get("error").is_some();
        record(&mut totals, &call.tool, &text, is_error);
        if let Some(dump) = &options.dump {
            std::fs::create_dir_all(dump).map_err(|error| error.to_string())?;
            std::fs::write(dump.join(format!("{index:03}-{}.txt", call.tool)), &text)
                .map_err(|error| error.to_string())?;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(summary(&totals))
}

fn main() {
    let outcome = parse_options(std::env::args().skip(1)).and_then(|options| run(&options));
    match outcome {
        Ok(summary) => println!("{summary:#}"),
        Err(error) => {
            eprintln!("replay_results: {error}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calls_parse_from_an_array_or_a_stream_transcript() {
        let array =
            r#"[{"tool":"compresso_read","arguments":{"path":"a.rs"}},{"tool":"compresso_stats"}]"#;
        let calls = parse_calls(array).expect("array parses");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].arguments["path"], "a.rs");
        assert_eq!(calls[1].arguments, json!({}));

        let transcript = [
            r#"{"type":"system","subtype":"init"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"x"},{"type":"tool_use","id":"t1","name":"mcp__compresso__compresso_grep","input":{"pattern":"x"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"1 x"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/a"}}]}}"#,
            "not json",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"compresso_read","input":{"path":"b.rs"}}]}}"#,
        ]
        .join("\n");
        let calls = parse_calls(&transcript).expect("transcript parses");
        assert_eq!(
            calls,
            vec![
                Call {
                    tool: "compresso_grep".to_owned(),
                    arguments: json!({ "pattern": "x" })
                },
                Call {
                    tool: "compresso_read".to_owned(),
                    arguments: json!({ "path": "b.rs" })
                },
            ]
        );
        assert!(parse_calls(r#"[{"arguments":{}}]"#).is_err());
    }

    #[test]
    fn test_summary_counts_characters_per_tool() {
        let mut totals = BTreeMap::new();
        record(&mut totals, "compresso_read", "ab\ncd\n", false);
        record(&mut totals, "compresso_read", "é", false);
        record(&mut totals, "compresso_grep", "[invalid_arguments] x", true);
        let summary = summary(&totals);
        assert_eq!(summary["tools"]["compresso_read"]["chars"], 7);
        assert_eq!(summary["tools"]["compresso_read"]["calls"], 2);
        assert_eq!(summary["tools"]["compresso_grep"]["errors"], 1);
        assert_eq!(summary["calls"], 3);
        assert_eq!(summary["chars"], 28);
    }

    #[test]
    fn test_options_require_the_binary_root_and_calls() {
        let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let options = parse_options(
            args(&[
                "--bin",
                "b",
                "--root",
                "r",
                "--calls",
                "c",
                "--only",
                "compresso_read,compresso_grep",
            ])
            .into_iter(),
        )
        .expect("valid");
        assert_eq!(options.tools, "lean");
        assert_eq!(options.only.expect("only").len(), 2);
        assert!(parse_options(args(&["--bin", "b"]).into_iter()).is_err());
        assert!(parse_options(args(&["--bogus"]).into_iter()).is_err());
    }
}
