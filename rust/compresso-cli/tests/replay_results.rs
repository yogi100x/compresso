//! E17-33 (D98): replays recorded Compresso tool calls against the built
//! `compresso` binary and reports how many characters each tool's results
//! put into context.
//!
//! The recorded replay is an ignored test driven by the environment:
//!
//! ```text
//! HOME=<scratch home whose store indexes REPO> \
//! COMPRESSO_REPLAY_BIN=<compresso binary> COMPRESSO_REPLAY_ROOT=<REPO> \
//! COMPRESSO_REPLAY_CALLS=<file> [COMPRESSO_REPLAY_TOOLS=lean|full] \
//! [COMPRESSO_REPLAY_RESULTS=text|json] \
//! [COMPRESSO_REPLAY_ONLY=compresso_read,compresso_grep] [COMPRESSO_REPLAY_DUMP=<dir>] \
//! [COMPRESSO_REPLAY_BASELINE=<dir> COMPRESSO_REPLAY_NEEDLES=<file>] \
//! cargo test -p compresso-cli --test replay_results -- --ignored --nocapture
//! ```
//!
//! The calls file is a JSON array of `{"tool": ..., "arguments": {...}}` or a
//! client stream-JSON transcript, one event per line, whose `tool_use` blocks
//! name Compresso tools (any `mcp__<server>__` prefix is dropped). Running two
//! binaries on the same calls and repository state gives the before and after
//! of a result-size change. The summary is one JSON document on stdout.
//!
//! E17-34 (D99): with `COMPRESSO_REPLAY_BASELINE=<dump dir of an earlier
//! replay of the same calls>` and `COMPRESSO_REPLAY_NEEDLES=<file>`, the
//! summary also reports which needle lines the baseline results showed and
//! the new results show, only name by line (`tests:`/`above:` in a grep file
//! header), or lost. The needles file is
//! `{"facts": ["text", ...], "sites": [{"file": "path", "line": 12}, ...]}`:
//! a shown line is a needle when it contains a fact or is a site.
// Integration tests of a bounded-local crate may start processes (the
// capability matrix); the workspace clippy.toml disallows them elsewhere.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use compresso_core::suppress_console_window;
use serde_json::{Value, json};

/// Points a spawned binary at an isolated home on every platform (the
/// store lives under `HOME`/XDG on Unix and `APPDATA`/`LOCALAPPDATA` on
/// Windows), as the other CLI process tests do.
fn isolate(command: &mut Command, home: &Path) {
    command
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("APPDATA", home.join("appdata"))
        .env("LOCALAPPDATA", home.join("localappdata"))
        .env_remove("COMPRESSO_LAUNCHER_PATH");
}

/// Keeps a spawned binary from opening a console window on Windows.
fn without_console(command: &mut Command) {
    let applied = suppress_console_window(|flags| {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(flags);
        }
        #[cfg(not(windows))]
        let _ = (flags, &command);
    });
    assert_eq!(applied, cfg!(windows));
}

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

/// The repository lines a grep or read result shows, as
/// `(path, line) -> text`, and the lines a grep file header names without
/// listing them (E17-34).
#[derive(Debug, Default, PartialEq)]
struct Shown {
    lines: BTreeMap<(String, u32), String>,
    named: BTreeSet<(String, u32)>,
}

fn leading_number(line: &str) -> Option<(u32, &str)> {
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let number = line.get(..digits)?.parse().ok()?;
    Some((number, &line[digits..]))
}

fn shown(tool: &str, text: &str) -> Shown {
    let mut shown = Shown::default();
    let mut lines = text.lines().peekable();
    let mut file: Option<String> = None;
    while let Some(line) = lines.next() {
        if tool == "compresso_read" {
            // `@@ path:START-END of TOTAL` and then exactly those lines.
            let Some(header) = line.strip_prefix("@@ ") else {
                continue;
            };
            let Some((path, span)) = header.rsplit_once(':') else {
                continue;
            };
            let span = span.split(' ').next().unwrap_or_default();
            let Some((start, end)) = span.split_once('-') else {
                continue;
            };
            let (Ok(start), Ok(end)) = (start.parse::<u32>(), end.parse::<u32>()) else {
                continue;
            };
            for number in start..=end {
                let Some(content) = lines.next() else { break };
                shown
                    .lines
                    .insert((path.to_owned(), number), content.to_owned());
            }
            continue;
        }
        if line.starts_with("# ") || line.starts_with('[') {
            file = None;
            continue;
        }
        if let Some((number, rest)) = leading_number(line) {
            if let (Some(path), Some(content)) = (&file, rest.get(1..)) {
                shown
                    .lines
                    .insert((path.clone(), number), content.to_owned());
            }
            continue;
        }
        // A file header: `path (N[, stale][; tests: 1, 2][; above: 3])`.
        let Some((path, counts)) = line.rsplit_once(" (") else {
            continue;
        };
        file = Some(path.to_owned());
        for part in counts.trim_end_matches(')').split("; ").skip(1) {
            let Some((_, numbers)) = part.split_once(": ") else {
                continue;
            };
            for number in numbers.split(", ").filter_map(|n| n.parse().ok()) {
                shown.named.insert((path.to_owned(), number));
            }
        }
    }
    shown
}

/// Needle lines the baseline showed, per call, against the new results.
fn compare_needles(
    calls: &[Call],
    baseline: &[String],
    current: &[String],
    facts: &[String],
    sites: &BTreeSet<(String, u32)>,
) -> Value {
    let (mut base_rows, mut kept, mut named) = (0, 0, 0);
    let mut lost = Vec::new();
    let mut base_facts = BTreeSet::new();
    let mut current_facts = BTreeSet::new();
    let mut not_shown_in_call = Vec::new();
    for (index, call) in calls.iter().enumerate() {
        let (Some(before), Some(after)) = (baseline.get(index), current.get(index)) else {
            continue;
        };
        let (before, after_shown) = (shown(&call.tool, before), shown(&call.tool, after));
        for fact in facts {
            let was = before
                .lines
                .values()
                .any(|text| text.contains(fact.as_str()));
            let is = after_shown
                .lines
                .values()
                .any(|text| text.contains(fact.as_str()));
            if was {
                base_facts.insert(fact.clone());
            }
            if is {
                current_facts.insert(fact.clone());
            }
            if was && !is {
                not_shown_in_call.push(json!({ "call": index, "fact": fact }));
            }
        }
        for (key, text) in &before.lines {
            if !(sites.contains(key) || facts.iter().any(|fact| text.contains(fact.as_str()))) {
                continue;
            }
            base_rows += 1;
            if after_shown.lines.contains_key(key) {
                kept += 1;
            } else if after_shown.named.contains(key) {
                named += 1;
            } else {
                lost.push(
                    json!({ "call": index, "tool": call.tool, "file": key.0, "line": key.1 }),
                );
            }
        }
    }
    json!({
        "needle_lines_in_baseline": base_rows,
        "kept": kept,
        "named_not_listed": named,
        "lost": lost,
        "facts_shown_by_baseline_only": base_facts.difference(&current_facts).collect::<Vec<_>>(),
        "facts_not_shown_in_call": not_shown_in_call,
    })
}

struct Replay<'a> {
    bin: &'a Path,
    root: &'a Path,
    /// Isolates the server's home (see [`isolate`]); `None` inherits the
    /// caller's environment.
    home: Option<&'a Path>,
    tools: &'a str,
    results: Option<&'a str>,
    dump: Option<&'a Path>,
}

/// Runs `calls` through one `compresso mcp serve` process and returns each
/// result's text, in order, plus the summary.
fn replay(options: &Replay<'_>, calls: &[Call]) -> Result<(Vec<String>, Value), String> {
    let mut command = Command::new(options.bin);
    command
        .args(["mcp", "serve", "--root"])
        .arg(options.root)
        .args(["--tools", options.tools]);
    if let Some(results) = options.results {
        command.args(["--results", results]);
    }
    if let Some(home) = options.home {
        isolate(&mut command, home);
    }
    without_console(&mut command);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("{}: {error}", options.bin.display()))?;
    let mut input = child.stdin.take().ok_or("no stdin")?;
    let mut output = BufReader::new(child.stdout.take().ok_or("no stdout")?);
    let mut next_id = 0_u64;
    let mut send = |method: &str, params: Value| -> Result<Value, String> {
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
    send(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "compresso-replay", "version": "1.0.0" }
        }),
    )?;
    send("notifications/initialized", Value::Null)?;
    let mut totals = BTreeMap::new();
    let mut texts = Vec::with_capacity(calls.len());
    for (index, call) in calls.iter().enumerate() {
        let reply = send(
            "tools/call",
            json!({ "name": call.tool, "arguments": call.arguments }),
        )?;
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .map_or_else(|| reply.to_string(), str::to_owned);
        let is_error = reply["result"]["isError"] == true || reply.get("error").is_some();
        record(&mut totals, &call.tool, &text, is_error);
        if let Some(dump) = options.dump {
            fs::create_dir_all(dump).map_err(|error| error.to_string())?;
            fs::write(dump.join(format!("{index:03}-{}.txt", call.tool)), &text)
                .map_err(|error| error.to_string())?;
        }
        texts.push(text);
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok((texts, summary(&totals)))
}

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
fn test_needles_are_kept_named_or_lost_against_a_baseline() {
    let calls = parse_calls(
        r#"[{"tool":"compresso_grep","arguments":{}},{"tool":"compresso_read","arguments":{}}]"#,
    )
    .expect("calls");
    let baseline = vec![
        "# tick — 3 lines in 1 files\nsrc/a.rs (3)\n1 [def tick] fn tick() {}\n7 [tests] use super::tick;\n9- context TICK_MAX\n[note]\n".to_owned(),
        "@@ src/b.rs:4-5 of 9\nconst TICK_MAX: u32 = 3;\nfn other() {}\n".to_owned(),
    ];
    let current = vec![
        "# tick — 3 lines in 1 files\nsrc/a.rs (3; tests: 7; above: 9)\n1 [def tick] fn tick() {}\n".to_owned(),
        "@@ src/b.rs:5-5 of 9\nfn other() {}\n".to_owned(),
    ];
    let shown_now = shown("compresso_grep", &current[0]);
    assert_eq!(
        shown_now.named,
        BTreeSet::from([("src/a.rs".to_owned(), 7), ("src/a.rs".to_owned(), 9)])
    );
    let facts = vec!["TICK_MAX".to_owned(), "fn tick".to_owned()];
    let sites = BTreeSet::from([("src/a.rs".to_owned(), 7)]);
    let report = compare_needles(&calls, &baseline, &current, &facts, &sites);
    assert_eq!(report["needle_lines_in_baseline"], 4);
    assert_eq!(report["kept"], 1);
    assert_eq!(report["named_not_listed"], 2);
    assert_eq!(
        report["lost"],
        json!([{ "call": 1, "tool": "compresso_read", "file": "src/b.rs", "line": 4 }])
    );
    assert_eq!(report["facts_shown_by_baseline_only"], json!(["TICK_MAX"]));
    assert_eq!(
        report["facts_not_shown_in_call"],
        json!([{ "call": 0, "fact": "TICK_MAX" }, { "call": 1, "fact": "TICK_MAX" }])
    );
}

/// A synthetic repository with one 300-line file, indexed by the built
/// binary into a temporary home: the text form windows the long read and
/// costs fewer characters than the JSON form of the same calls.
#[test]
fn test_replay_measures_both_forms_against_the_built_binary() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let home = temp.path().join("home");
    let root = temp.path().join("repo");
    fs::create_dir_all(root.join("src")).expect("source directory");
    fs::create_dir_all(&home).expect("home directory");
    let long = (0..300)
        .map(|index| format!("pub fn step_{index:03}(total: &mut u32) {{ *total += {index}; }}\n"))
        .collect::<String>();
    fs::write(root.join("src/long.rs"), long).expect("fixture written");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_compresso"));
    let mut index = Command::new(&bin);
    isolate(&mut index, &home);
    without_console(&mut index);
    let status = index
        .args(["index", "."])
        .current_dir(&root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("index runs");
    assert!(status.success(), "index failed: {status}");
    let calls = parse_calls(
        r#"[{"tool":"compresso_read","arguments":{"path":"src/long.rs","max_tokens":9000}},
            {"tool":"compresso_grep","arguments":{"pattern":"total","max_matches_per_file":4}}]"#,
    )
    .expect("calls");
    let run = |results: &'static str| {
        replay(
            &Replay {
                bin: &bin,
                root: &root,
                home: Some(home.as_path()),
                tools: "lean",
                results: Some(results),
                dump: None,
            },
            &calls,
        )
        .expect("replay runs")
    };
    let (texts, text_summary) = run("text");
    let (_, json_summary) = run("json");
    assert!(
        texts[0].starts_with("@@ src/long.rs:1-80 of 300\n"),
        "{}",
        texts[0]
    );
    assert!(
        texts[0].contains("[lines 81-300 not shown: past the 80-line read window;"),
        "{}",
        texts[0]
    );
    assert_eq!(text_summary["tools"]["compresso_grep"]["errors"], 0);
    let chars = |summary: &Value| summary["chars"].as_u64().expect("chars");
    assert!(
        chars(&text_summary) * 2 < chars(&json_summary),
        "text {text_summary} vs json {json_summary}"
    );
}

/// The recorded replay; see the module documentation.
#[test]
#[ignore = "replays recorded calls named by COMPRESSO_REPLAY_* against a built binary"]
fn replay_recorded_calls_from_the_environment() {
    let variable = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let required = |name: &str| variable(name).unwrap_or_else(|| panic!("set {name}"));
    let (bin, root, calls_path) = (
        required("COMPRESSO_REPLAY_BIN"),
        required("COMPRESSO_REPLAY_ROOT"),
        required("COMPRESSO_REPLAY_CALLS"),
    );
    let tools = std::env::var("COMPRESSO_REPLAY_TOOLS").unwrap_or_else(|_| "lean".to_owned());
    let results = std::env::var("COMPRESSO_REPLAY_RESULTS").ok();
    let only = std::env::var("COMPRESSO_REPLAY_ONLY").ok();
    let dump = variable("COMPRESSO_REPLAY_DUMP");
    let text = fs::read_to_string(&calls_path).expect("calls file readable");
    let calls = parse_calls(&text)
        .expect("calls parse")
        .into_iter()
        .filter(|call| {
            only.as_deref()
                .is_none_or(|only| only.split(',').any(|tool| tool == call.tool))
        })
        .collect::<Vec<_>>();
    let (texts, mut summary) = replay(
        &Replay {
            bin: &bin,
            root: &root,
            home: None,
            tools: &tools,
            results: results.as_deref(),
            dump: dump.as_deref(),
        },
        &calls,
    )
    .expect("replay runs");
    if let (Some(baseline), Some(needles)) = (
        variable("COMPRESSO_REPLAY_BASELINE"),
        variable("COMPRESSO_REPLAY_NEEDLES"),
    ) {
        // The dump an earlier replay of the same calls wrote, in call order.
        let baseline = (0..calls.len())
            .map(|index| {
                fs::read_to_string(baseline.join(format!("{index:03}-{}.txt", calls[index].tool)))
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        let needles: Value =
            serde_json::from_str(&fs::read_to_string(&needles).expect("needles file readable"))
                .expect("needles file is JSON");
        let facts = needles["facts"]
            .as_array()
            .map(|facts| {
                facts
                    .iter()
                    .filter_map(|fact| fact.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let sites = needles["sites"]
            .as_array()
            .map(|sites| {
                sites
                    .iter()
                    .filter_map(|site| {
                        Some((
                            site["file"].as_str()?.to_owned(),
                            u32::try_from(site["line"].as_u64()?).ok()?,
                        ))
                    })
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        summary["needles"] = compare_needles(&calls, &baseline, &texts, &facts, &sites);
    }
    println!("{summary:#}");
}
