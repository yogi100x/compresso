//! Closed-pipe classification and panic-free standard-stream reporting.
//!
//! A reader that goes away (a client that exits early, `| head`, a closed
//! console) surfaces as `EPIPE` on Unix and as `ERROR_BROKEN_PIPE`,
//! `ERROR_NO_DATA`, or `ERROR_PIPE_NOT_CONNECTED` on Windows. None of these
//! may panic or abort a Compresso process: writers classify them into
//! [`StreamError::Closed`]. The MCP server and `compresso-eval` report that
//! typed error; the `compresso` CLI uses it to stop quietly, like a standard
//! Unix filter whose reader went away.

use std::error::Error as StdError;
use std::fmt;
use std::io::{self, Write};

use thiserror::Error;

/// Windows `ERROR_BROKEN_PIPE`: the pipe has been ended.
const ERROR_BROKEN_PIPE: i32 = 109;
/// Windows `ERROR_NO_DATA`: the pipe is being closed.
const ERROR_NO_DATA: i32 = 232;
/// Windows `ERROR_PIPE_NOT_CONNECTED`: no process is on the other end.
const ERROR_PIPE_NOT_CONNECTED: i32 = 233;
/// Unix `EPIPE` on Linux, macOS, and the BSDs.
const EPIPE: i32 = 32;

/// A failed write to a standard stream or child pipe.
#[derive(Debug, Error)]
pub enum StreamError {
    /// The reading side closed the stream before the write completed.
    #[error("{stream} was closed by its reader before the write completed")]
    Closed {
        /// Human-readable stream name, such as `standard output`.
        stream: &'static str,
        /// The operating-system error that reported the closed pipe.
        #[source]
        source: io::Error,
    },
    /// The write failed for a reason other than a closed reader.
    #[error("writing to {stream} failed")]
    Io {
        /// Human-readable stream name, such as `standard output`.
        stream: &'static str,
        /// The underlying I/O failure.
        #[source]
        source: io::Error,
    },
}

impl StreamError {
    /// Classifies a write failure on `stream`.
    pub fn classify(stream: &'static str, source: io::Error) -> Self {
        if is_closed_pipe(&source) {
            Self::Closed { stream, source }
        } else {
            Self::Io { stream, source }
        }
    }

    /// Returns whether the reader closed the stream.
    pub const fn is_closed(&self) -> bool {
        matches!(self, Self::Closed { .. })
    }

    /// Stable machine-readable error code.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Closed { .. } => "output_closed",
            Self::Io { .. } => "output_failed",
        }
    }
}

/// Returns whether `error` reports a pipe whose reader has gone away.
///
/// The check covers the portable [`io::ErrorKind::BrokenPipe`] kind, the raw
/// platform codes, and I/O failures wrapped by a serializer or by
/// [`io::Error::other`], which would otherwise hide the original kind.
pub fn is_closed_pipe(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::BrokenPipe {
        return true;
    }
    if let Some(code) = error.raw_os_error() {
        return is_closed_pipe_code(code, cfg!(windows));
    }
    let mut source = error
        .get_ref()
        .map(|inner| inner as &(dyn StdError + 'static));
    while let Some(inner) = source {
        if let Some(io_error) = inner.downcast_ref::<io::Error>() {
            return is_closed_pipe(io_error);
        }
        if let Some(json_error) = inner.downcast_ref::<serde_json::Error>()
            && json_error.io_error_kind() == Some(io::ErrorKind::BrokenPipe)
        {
            return true;
        }
        source = inner.source();
    }
    false
}

const fn is_closed_pipe_code(code: i32, windows: bool) -> bool {
    if windows {
        matches!(
            code,
            ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED
        )
    } else {
        code == EPIPE
    }
}

/// Writer adapter that remembers the first closed-pipe failure.
///
/// Call sites that only test `is_ok()` or re-wrap errors still fail, and the
/// owner of the guard can report one typed [`StreamError`] at the end through
/// [`ClosedPipeGuard::finish`]. After the reader is gone every further write
/// fails immediately without touching the stream again.
pub struct ClosedPipeGuard<W> {
    inner: W,
    stream: &'static str,
    closed: Option<StreamError>,
}

impl<W: Write> ClosedPipeGuard<W> {
    /// Guards `inner`, naming it `stream` in reported errors.
    pub const fn new(inner: W, stream: &'static str) -> Self {
        Self {
            inner,
            stream,
            closed: None,
        }
    }

    /// Flushes the stream and returns the first closed-pipe or flush failure.
    pub fn finish(mut self) -> Result<(), StreamError> {
        let flushed = self.flush();
        if let Some(closed) = self.closed.take() {
            return Err(closed);
        }
        flushed.map_err(|source| StreamError::classify(self.stream, source))
    }

    fn observe<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        if let Err(error) = &result
            && self.closed.is_none()
            && is_closed_pipe(error)
        {
            let source = match error.raw_os_error() {
                Some(code) => io::Error::from_raw_os_error(code),
                None => io::Error::from(io::ErrorKind::BrokenPipe),
            };
            self.closed = Some(StreamError::Closed {
                stream: self.stream,
                source,
            });
        }
        result
    }
}

impl<W: Write> Write for ClosedPipeGuard<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.closed.is_some() {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        let result = self.inner.write(buffer);
        self.observe(result)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.closed.is_some() {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        let result = self.inner.flush();
        self.observe(result)
    }
}

/// Writes one diagnostic line to `stream`, classifying any failure.
pub fn write_diagnostic_line(
    stream: &mut dyn Write,
    name: &'static str,
    message: fmt::Arguments<'_>,
) -> Result<(), StreamError> {
    writeln!(stream, "{message}")
        .and_then(|()| stream.flush())
        .map_err(|source| StreamError::classify(name, source))
}

/// Reports a diagnostic on standard error without ever panicking.
///
/// `eprintln!` aborts the process when standard error is a closed pipe. A
/// diagnostic that cannot be delivered is dropped instead: there is no other
/// channel left to report it on, and the operation it describes must go on.
pub fn report_diagnostic(message: fmt::Arguments<'_>) {
    let _undeliverable = write_diagnostic_line(&mut io::stderr().lock(), "standard error", message);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Accepts `budget` bytes, then fails every call with `failure`.
    struct FailingWriter {
        budget: usize,
        failure: fn() -> io::Error,
        calls_after_failure: usize,
    }

    impl FailingWriter {
        fn new(budget: usize, failure: fn() -> io::Error) -> Self {
            Self {
                budget,
                failure,
                calls_after_failure: 0,
            }
        }
    }

    impl Write for FailingWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            if self.budget == 0 {
                self.calls_after_failure += 1;
                return Err((self.failure)());
            }
            let written = buffer.len().min(self.budget);
            self.budget -= written;
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn broken_pipe() -> io::Error {
        io::Error::from(io::ErrorKind::BrokenPipe)
    }

    fn disk_full() -> io::Error {
        io::Error::from(io::ErrorKind::StorageFull)
    }

    #[test]
    fn test_platform_closed_pipe_codes_are_classified() {
        for code in [ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED] {
            assert!(is_closed_pipe_code(code, true), "Windows code {code}");
        }
        assert!(is_closed_pipe_code(EPIPE, false));
        // ERROR_NOT_ENOUGH_QUOTA on Windows and ENOSPC on Unix are not pipes.
        assert!(!is_closed_pipe_code(EPIPE, true));
        assert!(!is_closed_pipe_code(28, false));
    }

    #[test]
    fn test_native_closed_pipe_code_is_classified_as_closed() {
        let native = if cfg!(windows) { ERROR_NO_DATA } else { EPIPE };

        let error = StreamError::classify("child input", io::Error::from_raw_os_error(native));

        assert!(error.is_closed());
        assert_eq!(error.code(), "output_closed");
        assert_eq!(
            error.to_string(),
            "child input was closed by its reader before the write completed"
        );
    }

    #[test]
    fn test_wrapped_serializer_failure_keeps_closed_pipe_classification() {
        let mut writer = FailingWriter::new(0, broken_pipe);
        let json_error = serde_json::to_writer(&mut writer, &serde_json::json!({"a": 1}))
            .expect_err("serialization into a closed pipe fails");

        assert!(is_closed_pipe(&io::Error::other(json_error)));
        assert!(is_closed_pipe(&io::Error::other(broken_pipe())));
        assert!(!is_closed_pipe(&io::Error::other("unrelated")));
        assert!(!is_closed_pipe(&disk_full()));
    }

    #[test]
    fn test_guard_reports_typed_closed_error_and_stops_writing() {
        let mut inner = FailingWriter::new(4, broken_pipe);
        let mut guard = ClosedPipeGuard::new(&mut inner, "standard output");

        assert!(guard.write_all(b"abcdefgh").is_err());
        assert!(guard.write_all(b"more").is_err());
        assert!(writeln!(guard, "again").is_err());
        let error = guard.finish().expect_err("closed pipe is reported");

        assert!(matches!(
            error,
            StreamError::Closed {
                stream: "standard output",
                ..
            }
        ));
        assert_eq!(inner.calls_after_failure, 1);
    }

    #[test]
    fn test_guard_passes_healthy_and_non_pipe_outcomes_through() {
        let mut healthy = Vec::new();
        let mut guard = ClosedPipeGuard::new(&mut healthy, "standard output");
        guard.write_all(b"ok\n").expect("healthy write");
        guard.finish().expect("healthy stream finishes");
        assert_eq!(healthy, b"ok\n");

        let mut inner = FailingWriter::new(0, disk_full);
        let mut guard = ClosedPipeGuard::new(&mut inner, "standard output");
        let error = guard.write_all(b"x").expect_err("write fails");
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        guard
            .finish()
            .expect("a non-pipe write failure is left to the caller that saw it");
    }

    #[test]
    fn test_diagnostic_to_closed_stream_returns_typed_error_without_panicking() {
        let mut closed = FailingWriter::new(0, broken_pipe);

        let error = write_diagnostic_line(&mut closed, "standard error", format_args!("warning"))
            .expect_err("closed stream is reported");

        assert!(error.is_closed());
        let mut healthy = Vec::new();
        write_diagnostic_line(&mut healthy, "standard error", format_args!("note {}", 7))
            .expect("healthy diagnostic");
        assert_eq!(healthy, b"note 7\n");
    }
}
