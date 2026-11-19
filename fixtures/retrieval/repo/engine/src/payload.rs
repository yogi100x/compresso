use std::fmt;

/// Largest job body accepted by the queue, in bytes.
pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// Returned when a job body exceeds [`MAX_BODY_BYTES`].
#[derive(Debug, PartialEq, Eq)]
pub struct PayloadTooLarge {
    pub actual_bytes: usize,
    pub allowed_bytes: usize,
}

impl fmt::Display for PayloadTooLarge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "job body is {} bytes, more than the {} allowed",
            self.actual_bytes, self.allowed_bytes
        )
    }
}

/// Rejects a body before it is stored, so an oversized job never reaches a worker.
pub fn ensure_fits(body: &[u8]) -> Result<(), PayloadTooLarge> {
    if body.len() > MAX_BODY_BYTES {
        return Err(PayloadTooLarge {
            actual_bytes: body.len(),
            allowed_bytes: MAX_BODY_BYTES,
        });
    }
    Ok(())
}
