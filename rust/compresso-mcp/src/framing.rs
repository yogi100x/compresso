//! Bounded newline-delimited framing for the stdio transport.
//!
//! A client controls how long a line is, so the reader never buffers more
//! than the configured limit: once a line exceeds it, the buffered prefix is
//! released and the rest of the line is discarded as it streams past.

use std::fmt;
use std::io;

use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

/// Largest incoming JSON-RPC message, in bytes before the line terminator.
pub const MAX_INCOMING_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// Size of each read from the client.
const READ_CHUNK_BYTES: usize = 64 * 1024;
/// Buffer capacity kept between messages; larger buffers are released.
const RETAINED_BUFFER_BYTES: usize = 64 * 1024;
/// Leading bytes of a rejected message searched for its request id.
const ID_SCAN_BYTES: usize = 4 * 1024;
/// Longest string request id echoed back in a rejection.
const MAX_RECOVERED_ID_BYTES: usize = 128;

/// One line taken from the client.
#[derive(Debug, PartialEq)]
pub(crate) enum Frame {
    /// A complete line within the limit; read it with
    /// [`BoundedLineReader::message`].
    Message,
    /// A line over the limit, already discarded.
    Oversized {
        /// The request id, when the start of the line identified it safely.
        id: Option<Value>,
        /// Whether the line ended with a newline; `false` means the input
        /// ended inside the oversized line.
        terminated: bool,
    },
    /// The input ended between lines.
    End,
}

enum State {
    Buffering,
    Discarding { id: Option<Value> },
}

/// Reads newline-delimited frames while holding at most `limit` bytes.
///
/// All progress is kept on the reader, so dropping a pending
/// [`Self::next_frame`] future loses no input.
pub(crate) struct BoundedLineReader<R> {
    reader: BufReader<R>,
    limit: usize,
    buffer: Vec<u8>,
    state: State,
    /// The previous frame was returned and its bytes can be released.
    handed_off: bool,
    #[cfg(test)]
    peak_capacity: usize,
}

impl<R: AsyncRead + Unpin> BoundedLineReader<R> {
    pub(crate) fn new(reader: R, limit: usize) -> Self {
        Self {
            reader: BufReader::with_capacity(READ_CHUNK_BYTES, reader),
            limit,
            buffer: Vec::new(),
            state: State::Buffering,
            handed_off: false,
            #[cfg(test)]
            peak_capacity: 0,
        }
    }

    /// The configured per-message limit in bytes.
    pub(crate) const fn limit(&self) -> usize {
        self.limit
    }

    /// The line behind the last [`Frame::Message`], without its terminator.
    pub(crate) fn message(&self) -> &[u8] {
        let mut message = self.buffer.as_slice();
        while let [rest @ .., b'\r'] = message {
            message = rest;
        }
        message
    }

    /// Largest buffer capacity this reader has held.
    #[cfg(test)]
    pub(crate) const fn peak_buffer_capacity(&self) -> usize {
        self.peak_capacity
    }

    /// Reads up to the next newline or the end of input.
    pub(crate) async fn next_frame(&mut self) -> io::Result<Frame> {
        if self.handed_off {
            self.handed_off = false;
            self.buffer.clear();
            self.buffer.shrink_to(RETAINED_BUFFER_BYTES);
        }
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                return Ok(self.hand_off(false));
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let content = &available[..newline.unwrap_or(available.len())];
            let consumed = newline.map_or(available.len(), |index| index + 1);
            if matches!(self.state, State::Buffering) {
                if content.len() > self.limit - self.buffer.len() {
                    let id = recover_request_id(&self.buffer, content);
                    self.buffer = Vec::new();
                    self.state = State::Discarding { id };
                } else {
                    append_within(&mut self.buffer, content, self.limit);
                    #[cfg(test)]
                    {
                        self.peak_capacity = self.peak_capacity.max(self.buffer.capacity());
                    }
                }
            }
            self.reader.consume(consumed);
            if newline.is_some() {
                return Ok(self.hand_off(true));
            }
        }
    }

    fn hand_off(&mut self, terminated: bool) -> Frame {
        self.handed_off = true;
        match std::mem::replace(&mut self.state, State::Buffering) {
            State::Discarding { id } => Frame::Oversized { id, terminated },
            State::Buffering if !terminated && self.buffer.is_empty() => Frame::End,
            State::Buffering => Frame::Message,
        }
    }
}

/// Appends `content`, growing geometrically but never past `limit`.
///
/// The caller guarantees the result fits within `limit`; `Vec`'s own growth
/// policy could otherwise reserve nearly twice that.
fn append_within(buffer: &mut Vec<u8>, content: &[u8], limit: usize) {
    let needed = buffer.len() + content.len();
    if needed > buffer.capacity() {
        let target = needed
            .max(buffer.capacity().saturating_mul(2))
            .min(limit.max(needed));
        buffer.reserve_exact(target - buffer.len());
    }
    buffer.extend_from_slice(content);
}

/// Recovers the request id from the start of a rejected message.
///
/// Only a top-level integer or short string `id` that is followed by another
/// complete member (or the end of the object) within the first
/// [`ID_SCAN_BYTES`] is trusted. A nested, repeated, structured, or possibly
/// truncated id yields `None`, so a rejection never names the wrong request.
fn recover_request_id(buffered: &[u8], next: &[u8]) -> Option<Value> {
    let mut window = Vec::with_capacity(ID_SCAN_BYTES);
    window.extend(buffered.iter().take(ID_SCAN_BYTES));
    window.extend(next.iter().take(ID_SCAN_BYTES - window.len()));

    let mut slot = IdSlot::Missing;
    let mut deserializer = serde_json::Deserializer::from_slice(&window);
    // The window is a prefix of an oversized message, so parsing is expected
    // to fail at the cut; whatever was confirmed before that point stands.
    let _truncated = TopLevelId(&mut slot).deserialize(&mut deserializer);
    match slot {
        IdSlot::Found(id) => Some(id),
        IdSlot::Missing | IdSlot::Rejected => None,
    }
}

enum IdSlot {
    Missing,
    Found(Value),
    Rejected,
}

impl IdSlot {
    fn confirm(&mut self, id: Value) {
        *self = match self {
            Self::Missing => Self::Found(id),
            Self::Found(_) | Self::Rejected => Self::Rejected,
        };
    }
}

struct TopLevelId<'a>(&'a mut IdSlot);

impl<'de> DeserializeSeed<'de> for TopLevelId<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for TopLevelId<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON-RPC message object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut candidate = None;
        loop {
            // An id counts only once the member after it has started
            // cleanly, which proves the id token itself was not cut short.
            let key = map.next_key::<String>()?;
            if let Some(id) = candidate.take() {
                self.0.confirm(id);
            }
            let Some(key) = key else {
                return Ok(());
            };
            if key == "id" {
                let id = map.next_value::<Value>()?;
                if is_echoable_id(&id) {
                    candidate = Some(id);
                } else {
                    *self.0 = IdSlot::Rejected;
                }
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
    }
}

fn is_echoable_id(id: &Value) -> bool {
    match id {
        Value::Number(number) => number.is_i64() || number.is_u64(),
        Value::String(text) => text.len() <= MAX_RECOVERED_ID_BYTES,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::AsyncWriteExt;

    use super::*;

    fn reader(input: &[u8], limit: usize) -> BoundedLineReader<&[u8]> {
        BoundedLineReader::new(input, limit)
    }

    fn id_of(prefix: &str) -> Option<Value> {
        recover_request_id(prefix.as_bytes(), b"")
    }

    #[tokio::test]
    async fn test_line_at_the_limit_is_a_message() {
        let mut reader = reader(b"12345678\nnext\n", 8);

        assert_eq!(reader.next_frame().await.unwrap(), Frame::Message);
        assert_eq!(reader.message(), b"12345678");
        assert_eq!(reader.next_frame().await.unwrap(), Frame::Message);
        assert_eq!(reader.message(), b"next");
        assert_eq!(reader.next_frame().await.unwrap(), Frame::End);
        assert!(reader.peak_buffer_capacity() <= 8);
    }

    #[tokio::test]
    async fn test_line_over_the_limit_is_discarded_and_the_next_line_is_read() {
        let mut reader = reader(b"123456789\nnext\n", 8);

        assert_eq!(
            reader.next_frame().await.unwrap(),
            Frame::Oversized {
                id: None,
                terminated: true
            }
        );
        assert_eq!(reader.next_frame().await.unwrap(), Frame::Message);
        assert_eq!(reader.message(), b"next");
    }

    #[tokio::test]
    async fn test_oversized_line_at_end_of_input_is_unterminated() {
        let mut reader = reader(b"ok\n123456789", 8);

        assert_eq!(reader.next_frame().await.unwrap(), Frame::Message);
        assert_eq!(
            reader.next_frame().await.unwrap(),
            Frame::Oversized {
                id: None,
                terminated: false
            }
        );
        assert_eq!(reader.next_frame().await.unwrap(), Frame::End);
    }

    #[tokio::test]
    async fn test_final_line_without_newline_is_a_message() {
        let mut reader = reader(b"tail\r", 8);

        assert_eq!(reader.next_frame().await.unwrap(), Frame::Message);
        assert_eq!(reader.message(), b"tail");
        assert_eq!(reader.next_frame().await.unwrap(), Frame::End);
    }

    #[tokio::test]
    async fn test_dropped_read_keeps_the_partial_line() {
        let (mut client, server) = tokio::io::duplex(64);
        let mut reader = BoundedLineReader::new(server, 64);
        client.write_all(b"{\"par").await.unwrap();

        tokio::select! {
            biased;
            _ = reader.next_frame() => panic!("an incomplete line is not a frame"),
            () = tokio::task::yield_now() => {}
        }
        client.write_all(b"tial\":1}\n").await.unwrap();

        assert_eq!(reader.next_frame().await.unwrap(), Frame::Message);
        assert_eq!(reader.message(), b"{\"partial\":1}");
    }

    #[tokio::test]
    async fn test_large_buffer_is_released_after_hand_off() {
        let mut input = vec![b' '; 4 * RETAINED_BUFFER_BYTES];
        input.extend_from_slice(b"\nnext\n");
        let mut reader = reader(&input, 8 * RETAINED_BUFFER_BYTES);

        assert_eq!(reader.next_frame().await.unwrap(), Frame::Message);
        assert_eq!(reader.next_frame().await.unwrap(), Frame::Message);
        assert!(reader.buffer.capacity() <= RETAINED_BUFFER_BYTES);
    }

    #[test]
    fn test_growth_never_reserves_past_the_limit() {
        let mut buffer = Vec::new();
        for _ in 0..10 {
            append_within(&mut buffer, &[0; 7], 70);
            assert!(buffer.capacity() <= 70);
        }
        assert_eq!(buffer.len(), 70);
    }

    #[test]
    fn test_top_level_id_is_recovered_once_the_next_member_starts() {
        assert_eq!(
            id_of(r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"text":"aaa"#),
            Some(json!(7))
        );
        assert_eq!(
            id_of(r#"{"id":"req-\"1\"","method":"x"#),
            Some(json!("req-\"1\""))
        );
        assert_eq!(id_of(r#"{"id":-3}"#), Some(json!(-3)));
    }

    #[test]
    fn test_possibly_truncated_id_is_not_recovered() {
        assert_eq!(id_of(r#"{"jsonrpc":"2.0","id":12"#), None);
        assert_eq!(id_of(r#"{"jsonrpc":"2.0","id":12,"#), None);
        assert_eq!(id_of(r#"{"jsonrpc":"2.0","id":12,"meth"#), None);
        assert_eq!(id_of(r#"{"jsonrpc":"2.0","id":"abc"#), None);
    }

    #[test]
    fn test_nested_late_or_unusual_ids_are_not_recovered() {
        assert_eq!(id_of(r#"{"params":{"id":5,"x":1},"method":"x","#), None);
        assert_eq!(id_of(r#"{"params":"\"id\":5,","method":"x","#), None);
        assert_eq!(id_of(r#"[{"id":5,"method":"x"}"#), None);
        assert_eq!(id_of(r#"{"id":1.5,"method":"x","#), None);
        assert_eq!(id_of(r#"{"id":[1],"method":"x","#), None);
        assert_eq!(id_of(r#"{"id":null,"method":"x","#), None);
        assert_eq!(id_of(r#"{"id":1,"id":2,"method":"x","#), None);
        let long = format!(
            r#"{{"id":"{}","method":"x","#,
            "i".repeat(MAX_RECOVERED_ID_BYTES + 1)
        );
        assert_eq!(id_of(&long), None);
        let late = format!(
            r#"{{"params":"{}","id":5,"method":"x","#,
            "p".repeat(ID_SCAN_BYTES)
        );
        assert_eq!(id_of(&late), None);
    }

    #[test]
    fn test_id_scan_spans_the_buffered_prefix_and_the_next_chunk() {
        assert_eq!(
            recover_request_id(br#"{"jsonrpc":"2.0","i"#, br#"d":9,"method":"x","#),
            Some(json!(9))
        );
    }
}
