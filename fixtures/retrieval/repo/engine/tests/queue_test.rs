use std::time::Duration;

use engine::payload::{MAX_BODY_BYTES, ensure_fits};
use engine::queue::JobQueue;
use engine::retry::retry_backoff;

#[test]
fn test_enqueue_rejects_payload_too_large() {
    let mut queue = JobQueue::new(8);
    let body = vec![0_u8; MAX_BODY_BYTES + 1];
    assert!(ensure_fits(&body).is_err());
    assert!(queue.enqueue("email", body).is_err());
}

#[test]
fn test_retry_backoff_doubles_and_caps() {
    assert_eq!(retry_backoff(0), Duration::from_millis(200));
    assert_eq!(retry_backoff(1), Duration::from_millis(400));
    assert_eq!(retry_backoff(30), Duration::from_millis(60_000));
}

#[test]
fn test_queue_limit_returns_error_when_full() {
    let mut queue = JobQueue::new(1);
    queue.enqueue("email", vec![1]).expect("first job fits");
    assert!(queue.enqueue("email", vec![2]).is_err());
}

#[test]
fn test_dequeue_returns_jobs_in_order() {
    let mut queue = JobQueue::new(4);
    let first = queue.enqueue("email", vec![1]).expect("job fits");
    queue.enqueue("report", vec![2]).expect("job fits");
    assert_eq!(queue.dequeue_next().map(|job| job.id), Some(first));
}
