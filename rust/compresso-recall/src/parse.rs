//! Shape probes for verified transcript line formats.
//!
//! A line is only turned into searchable text when it matches the exact shape
//! below. Anything else is reported as ignored or unrecognized, never guessed.
//!
//! * Claude Code (`~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`): one
//!   JSON object per line with `type` of `user` or `assistant`, a `message`
//!   object whose `content` is a string or an array of typed parts, and
//!   optional `cwd`, `sessionId`, and `timestamp` strings.
//! * Codex (`~/.codex/sessions/**/rollout-*.jsonl`): one JSON object per line
//!   with `timestamp`, `type`, and `payload`. `session_meta` carries the
//!   session `id` and `cwd`; `response_item` with a `message` payload carries
//!   `role` and `input_text`/`output_text` content parts.

use serde_json::{Map, Value};

use crate::sources::RecallSource;

/// Searchable text recovered from one transcript line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TranscriptEvent {
    pub(crate) role: &'static str,
    pub(crate) text: String,
    pub(crate) timestamp: Option<String>,
}

/// Session metadata carried by a line, when present.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LineContext {
    pub(crate) cwd: Option<String>,
    pub(crate) session_id: Option<String>,
}

/// Outcome of probing one transcript line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ParsedLine {
    /// A recognized line. `event` is `None` for metadata-only lines.
    Recognized {
        context: LineContext,
        event: Option<TranscriptEvent>,
    },
    /// The line is not valid JSON or does not match the verified shape.
    Unrecognized,
}

/// Probes one line of a transcript written by `source`.
pub(crate) fn parse_line(source: RecallSource, line: &str) -> ParsedLine {
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(line) else {
        return ParsedLine::Unrecognized;
    };
    match source {
        RecallSource::ClaudeCode => parse_claude_code(&object),
        RecallSource::Codex => parse_codex(&object),
        RecallSource::Cursor => ParsedLine::Unrecognized,
    }
}

fn string_field(object: &Map<String, Value>, name: &str) -> Option<String> {
    object
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn parse_claude_code(object: &Map<String, Value>) -> ParsedLine {
    let Some(kind) = object.get("type").and_then(Value::as_str) else {
        return ParsedLine::Unrecognized;
    };
    let context = LineContext {
        cwd: string_field(object, "cwd"),
        session_id: string_field(object, "sessionId"),
    };
    let role = match kind {
        "user" => "user",
        "assistant" => "assistant",
        _ => {
            return ParsedLine::Recognized {
                context,
                event: None,
            };
        }
    };
    let Some(content) = object
        .get("message")
        .and_then(Value::as_object)
        .and_then(|message| message.get("content"))
    else {
        return ParsedLine::Unrecognized;
    };
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => join_parts(parts.iter().filter_map(claude_code_part)),
        _ => return ParsedLine::Unrecognized,
    };
    ParsedLine::Recognized {
        context,
        event: event(role, text, string_field(object, "timestamp")),
    }
}

/// Extracts prose and shell commands; tool results are never searched.
fn claude_code_part(part: &Value) -> Option<&str> {
    let part = part.as_object()?;
    match part.get("type").and_then(Value::as_str)? {
        "text" => part.get("text").and_then(Value::as_str),
        "tool_use" => part
            .get("input")
            .and_then(Value::as_object)
            .and_then(|input| input.get("command"))
            .and_then(Value::as_str),
        _ => None,
    }
}

fn parse_codex(object: &Map<String, Value>) -> ParsedLine {
    let (Some(kind), Some(payload)) = (
        object.get("type").and_then(Value::as_str),
        object.get("payload").and_then(Value::as_object),
    ) else {
        return ParsedLine::Unrecognized;
    };
    match kind {
        "session_meta" => ParsedLine::Recognized {
            context: LineContext {
                cwd: string_field(payload, "cwd"),
                session_id: string_field(payload, "id"),
            },
            event: None,
        },
        "response_item" => {
            let role = match (
                payload.get("type").and_then(Value::as_str),
                payload.get("role").and_then(Value::as_str),
            ) {
                (Some("message"), Some("user")) => "user",
                (Some("message"), Some("assistant")) => "assistant",
                _ => {
                    return ParsedLine::Recognized {
                        context: LineContext::default(),
                        event: None,
                    };
                }
            };
            let Some(parts) = payload.get("content").and_then(Value::as_array) else {
                return ParsedLine::Unrecognized;
            };
            let text = join_parts(parts.iter().filter_map(codex_part));
            ParsedLine::Recognized {
                context: LineContext::default(),
                event: event(role, text, string_field(object, "timestamp")),
            }
        }
        _ => ParsedLine::Recognized {
            context: LineContext::default(),
            event: None,
        },
    }
}

fn codex_part(part: &Value) -> Option<&str> {
    let part = part.as_object()?;
    match part.get("type").and_then(Value::as_str)? {
        "input_text" | "output_text" => part.get("text").and_then(Value::as_str),
        _ => None,
    }
}

fn join_parts<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    parts
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn event(role: &'static str, text: String, timestamp: Option<String>) -> Option<TranscriptEvent> {
    (!text.trim().is_empty()).then_some(TranscriptEvent {
        role,
        text,
        timestamp,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn recognized(parsed: ParsedLine) -> (LineContext, Option<TranscriptEvent>) {
        match parsed {
            ParsedLine::Recognized { context, event } => (context, event),
            ParsedLine::Unrecognized => panic!("line should be recognized"),
        }
    }

    #[test]
    fn test_claude_code_string_and_part_content_are_extracted() {
        let user = json!({
            "type": "user",
            "cwd": "/work/repo",
            "sessionId": "session-1",
            "timestamp": "2026-03-01T10:00:00.000Z",
            "message": { "role": "user", "content": "why does the cache miss?" }
        });
        let assistant = json!({
            "type": "assistant",
            "cwd": "/work/repo",
            "sessionId": "session-1",
            "message": { "role": "assistant", "content": [
                { "type": "text", "text": "Decision: key the cache by content hash." },
                { "type": "tool_use", "name": "Bash", "input": { "command": "cargo test -p cache" } },
                { "type": "tool_result", "content": "never searched output" }
            ] }
        });

        let (context, event) = recognized(parse_line(RecallSource::ClaudeCode, &user.to_string()));
        assert_eq!(context.cwd.as_deref(), Some("/work/repo"));
        assert_eq!(context.session_id.as_deref(), Some("session-1"));
        let event = event.expect("user text should be searchable");
        assert_eq!(event.role, "user");
        assert_eq!(event.text, "why does the cache miss?");
        assert_eq!(event.timestamp.as_deref(), Some("2026-03-01T10:00:00.000Z"));

        let (_, event) = recognized(parse_line(RecallSource::ClaudeCode, &assistant.to_string()));
        let event = event.expect("assistant text should be searchable");
        assert_eq!(
            event.text,
            "Decision: key the cache by content hash.\ncargo test -p cache"
        );
    }

    #[test]
    fn test_claude_code_metadata_and_tool_result_lines_have_no_event() {
        let summary = json!({ "type": "summary", "summary": "x", "leafUuid": "y" });
        let tool_result = json!({
            "type": "user",
            "cwd": "/work/repo",
            "message": { "role": "user", "content": [
                { "type": "tool_result", "content": "raw tool output" }
            ] }
        });

        assert_eq!(
            recognized(parse_line(RecallSource::ClaudeCode, &summary.to_string())).1,
            None
        );
        assert_eq!(
            recognized(parse_line(
                RecallSource::ClaudeCode,
                &tool_result.to_string()
            ))
            .1,
            None
        );
    }

    #[test]
    fn test_codex_session_meta_and_messages_are_extracted() {
        let meta = json!({
            "timestamp": "2026-03-02T09:00:00.000Z",
            "type": "session_meta",
            "payload": { "id": "thread-9", "cwd": "/work/repo", "originator": "codex" }
        });
        let message = json!({
            "timestamp": "2026-03-02T09:00:05.000Z",
            "type": "response_item",
            "payload": { "type": "message", "role": "assistant", "content": [
                { "type": "output_text", "text": "Chose sqlite over a flat file." }
            ] }
        });
        let reasoning = json!({
            "timestamp": "2026-03-02T09:00:06.000Z",
            "type": "response_item",
            "payload": { "type": "reasoning", "summary": [] }
        });
        let developer = json!({
            "timestamp": "2026-03-02T09:00:07.000Z",
            "type": "response_item",
            "payload": { "type": "message", "role": "developer", "content": [
                { "type": "input_text", "text": "injected instructions" }
            ] }
        });

        let (context, event) = recognized(parse_line(RecallSource::Codex, &meta.to_string()));
        assert_eq!(context.cwd.as_deref(), Some("/work/repo"));
        assert_eq!(context.session_id.as_deref(), Some("thread-9"));
        assert_eq!(event, None);

        let (_, event) = recognized(parse_line(RecallSource::Codex, &message.to_string()));
        let event = event.expect("assistant message should be searchable");
        assert_eq!(event.role, "assistant");
        assert_eq!(event.text, "Chose sqlite over a flat file.");

        for line in [reasoning, developer] {
            assert_eq!(
                recognized(parse_line(RecallSource::Codex, &line.to_string())).1,
                None
            );
        }
    }

    #[test]
    fn test_unverified_shapes_are_unrecognized() {
        for line in [
            "not json",
            "[1,2,3]",
            "{\"role\":\"user\",\"text\":\"guess me\"}",
            "{\"type\":\"user\",\"message\":\"flat string message\"}",
        ] {
            assert_eq!(
                parse_line(RecallSource::ClaudeCode, line),
                ParsedLine::Unrecognized,
                "{line}"
            );
        }
        for line in [
            "not json",
            "{\"type\":\"response_item\"}",
            "{\"role\":\"user\",\"content\":\"legacy flat line\"}",
        ] {
            assert_eq!(
                parse_line(RecallSource::Codex, line),
                ParsedLine::Unrecognized,
                "{line}"
            );
        }
        let cursor = json!({ "type": "user", "message": { "content": "hello" } });
        assert_eq!(
            parse_line(RecallSource::Cursor, &cursor.to_string()),
            ParsedLine::Unrecognized
        );
    }
}
