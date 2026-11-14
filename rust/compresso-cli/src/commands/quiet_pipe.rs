//! Quiet handling of a reader that closed the CLI's standard output.
//!
//! `compresso ... | head` must behave like a standard Unix filter: once the
//! reader is gone the command stops without a message. The output writer
//! records the closed pipe in a shared flag, and the error writer drops
//! everything written after that point, because a failure that follows the
//! closed pipe is a consequence of it and not a fault worth reporting.
//! Errors delivered before the pipe closed pass through untouched.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use compresso_core::is_closed_pipe;

/// Standard-output writer that records when its reader has gone away.
pub(super) struct ObservedOutput<'a, W> {
    inner: W,
    reader_gone: &'a AtomicBool,
}

impl<'a, W: Write> ObservedOutput<'a, W> {
    pub(super) const fn new(inner: W, reader_gone: &'a AtomicBool) -> Self {
        Self { inner, reader_gone }
    }

    fn observe<T>(&self, result: io::Result<T>) -> io::Result<T> {
        if let Err(error) = &result
            && is_closed_pipe(error)
        {
            self.reader_gone.store(true, Ordering::Relaxed);
        }
        result
    }
}

impl<W: Write> Write for ObservedOutput<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let result = self.inner.write(buffer);
        self.observe(result)
    }

    fn flush(&mut self) -> io::Result<()> {
        let result = self.inner.flush();
        self.observe(result)
    }
}

/// Error writer that goes silent once standard output has lost its reader.
pub(super) struct GatedErrors<'a, W> {
    inner: W,
    reader_gone: &'a AtomicBool,
    delivered: bool,
}

impl<'a, W: Write> GatedErrors<'a, W> {
    pub(super) const fn new(inner: W, reader_gone: &'a AtomicBool) -> Self {
        Self {
            inner,
            reader_gone,
            delivered: false,
        }
    }

    /// Whether a report line reached the error stream before the reader left.
    pub(super) const fn delivered(&self) -> bool {
        self.delivered
    }
}

impl<W: Write> Write for GatedErrors<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.reader_gone.load(Ordering::Relaxed) {
            return Ok(buffer.len());
        }
        let written = self.inner.write(buffer)?;
        // Reports are whole lines. A confirmation prompt ends without a
        // newline and is not a report, so it must not turn the quiet exit
        // into a failure.
        self.delivered |= buffer[..written].contains(&b'\n');
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.reader_gone.load(Ordering::Relaxed) {
            return Ok(());
        }
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingWriter(io::ErrorKind);

    impl Write for FailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(self.0))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_errors_written_before_the_reader_left_are_delivered() {
        let reader_gone = AtomicBool::new(false);
        let mut sink = Vec::new();
        let mut errors = GatedErrors::new(&mut sink, &reader_gone);

        writeln!(errors, "error[invalid_arguments]: bad").expect("error is written");

        assert!(errors.delivered());
        assert_eq!(sink, b"error[invalid_arguments]: bad\n");
    }

    #[test]
    fn test_confirmation_prompt_is_shown_but_is_not_a_delivered_report() {
        let reader_gone = AtomicBool::new(false);
        let mut sink = Vec::new();
        let mut errors = GatedErrors::new(&mut sink, &reader_gone);

        write!(errors, "Apply? [y/N] ").expect("prompt is written");

        assert!(!errors.delivered());
        assert_eq!(sink, b"Apply? [y/N] ");
    }

    #[test]
    fn test_errors_written_after_the_reader_left_are_dropped() {
        let reader_gone = AtomicBool::new(false);
        let mut output =
            ObservedOutput::new(FailingWriter(io::ErrorKind::BrokenPipe), &reader_gone);
        let mut sink = Vec::new();
        let mut errors = GatedErrors::new(&mut sink, &reader_gone);

        assert!(output.write_all(b"result").is_err());
        writeln!(errors, "error[internal]: write failed").expect("dropped quietly");

        assert!(!errors.delivered());
        assert!(sink.is_empty());
    }

    #[test]
    fn test_other_output_failures_do_not_silence_errors() {
        let reader_gone = AtomicBool::new(false);
        let mut output =
            ObservedOutput::new(FailingWriter(io::ErrorKind::StorageFull), &reader_gone);
        let mut sink = Vec::new();
        let mut errors = GatedErrors::new(&mut sink, &reader_gone);

        assert!(output.write_all(b"result").is_err());
        writeln!(errors, "error[internal]: write failed").expect("error is written");

        assert!(errors.delivered());
        assert!(!sink.is_empty());
    }
}
