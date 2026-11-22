use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use compresso_store::{OperationRecord, SessionId, Store, StoreError};
use tokio::sync::{Mutex, OnceCell, OwnedMutexGuard};

const TRANSPORT: &str = "stdio";
const UNKNOWN_CLIENT: &str = "unknown";
/// Failed tool calls remembered per server process for `compresso_report`
/// (E17-48, D113): tool name and error code only, never arguments or text.
pub(crate) const MAX_RECENT_FAILURES: usize = 20;

/// Persistence boundary used by [`SessionRuntime`].
///
/// Production delegates to [`Store`]. Tests can inject a deterministic writer
/// failure without changing filesystem permissions or process-global state.
pub(crate) trait OperationWriter: Send + Sync {
    fn write(&self, operation: &OperationRecord) -> Result<(), StoreError>;
}

impl OperationWriter for Store {
    fn write(&self, operation: &OperationRecord) -> Result<(), StoreError> {
        self.record_operation(operation)
    }
}

/// Content-free warning boundary for fail-open ledger diagnostics.
pub(crate) trait DiagnosticSink: Send + Sync {
    fn warn(&self, message: &str);
}

/// Production sink: one content-free line on standard error.
pub(crate) struct StderrDiagnosticSink;

impl DiagnosticSink for StderrDiagnosticSink {
    fn warn(&self, message: &str) {
        compresso_core::report_diagnostic(format_args!("{message}"));
    }
}

/// Error returned by the asynchronous session lifecycle.
#[derive(Debug)]
pub(crate) enum SessionError {
    Closing,
    Store(StoreError),
    Task(tokio::task::JoinError),
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closing => formatter.write_str("session is closing"),
            Self::Store(error) => write!(formatter, "session store operation failed: {error}"),
            Self::Task(error) => write!(formatter, "session store task failed: {error}"),
        }
    }
}

impl Error for SessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Closing => None,
            Self::Store(error) => Some(error),
            Self::Task(error) => Some(error),
        }
    }
}

struct SessionRuntimeInner {
    store: Store,
    session_id: OnceCell<SessionId>,
    closed: OnceCell<()>,
    closing: AtomicBool,
    call_gate: Arc<Mutex<()>>,
    operation_writer: Arc<dyn OperationWriter>,
    diagnostics: Arc<dyn DiagnosticSink>,
    /// Newest last; bounded by [`MAX_RECENT_FAILURES`]; memory only.
    recent_failures: std::sync::Mutex<std::collections::VecDeque<(String, &'static str)>>,
}

/// Per-connection MCP session lifecycle and serialization boundary.
///
/// Clones share one session id, one close state, and one fair Tokio mutex. A
/// caller must retain the guard returned by [`Self::acquire_call`] for the
/// complete handler and ledger-recording path. Session creation and closure are
/// blocking Store operations and therefore always execute on Tokio's blocking
/// pool. Ledger recording is synchronous because handlers already run inside
/// that blocking boundary; its failure is reported once per failed attempt and
/// never changes the tool result.
#[derive(Clone)]
pub(crate) struct SessionRuntime {
    inner: Arc<SessionRuntimeInner>,
}

impl SessionRuntime {
    /// Creates the production runtime backed by `store` and stderr warnings.
    pub(crate) fn new(store: Store) -> Self {
        let operation_writer: Arc<dyn OperationWriter> = Arc::new(store.clone());
        Self::with_components(store, operation_writer, Arc::new(StderrDiagnosticSink))
    }

    /// Creates a runtime with injectable operation and diagnostic boundaries.
    pub(crate) fn with_components(
        store: Store,
        operation_writer: Arc<dyn OperationWriter>,
        diagnostics: Arc<dyn DiagnosticSink>,
    ) -> Self {
        Self {
            inner: Arc::new(SessionRuntimeInner {
                store,
                session_id: OnceCell::new(),
                closed: OnceCell::new(),
                closing: AtomicBool::new(false),
                call_gate: Arc::new(Mutex::new(())),
                operation_writer,
                diagnostics,
                recent_failures: std::sync::Mutex::new(std::collections::VecDeque::new()),
            }),
        }
    }

    /// Starts exactly one stdio session and returns its shared id.
    ///
    /// Concurrent or repeated callers receive the first successfully-created
    /// session. Empty or whitespace-only client names use the documented
    /// `"unknown"` fallback.
    pub(crate) async fn start(&self, client: &str) -> Result<SessionId, SessionError> {
        let _lifecycle_guard = Arc::clone(&self.inner.call_gate).lock_owned().await;
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(SessionError::Closing);
        }
        let client = if client.trim().is_empty() {
            UNKNOWN_CLIENT.to_owned()
        } else {
            client.to_owned()
        };
        let store = self.inner.store.clone();
        let session_id = self
            .inner
            .session_id
            .get_or_try_init(|| async move {
                tokio::task::spawn_blocking(move || store.start_session(&client, TRANSPORT))
                    .await
                    .map_err(SessionError::Task)?
                    .map_err(SessionError::Store)
            })
            .await?;
        Ok(session_id.clone())
    }

    /// Returns the initialized session id, if initialization has completed.
    pub(crate) fn current(&self) -> Option<SessionId> {
        self.inner.session_id.get().cloned()
    }

    /// Remembers that `tool` failed with `code`, keeping the newest
    /// [`MAX_RECENT_FAILURES`] in memory. Nothing is persisted.
    pub(crate) fn record_failure(&self, tool: &str, code: &'static str) {
        if let Ok(mut failures) = self.inner.recent_failures.lock() {
            if failures.len() == MAX_RECENT_FAILURES {
                failures.pop_front();
            }
            failures.push_back((tool.to_owned(), code));
        }
    }

    /// The failures remembered by [`Self::record_failure`], newest first.
    pub(crate) fn recent_failures(&self) -> Vec<(String, &'static str)> {
        self.inner
            .recent_failures
            .lock()
            .map(|failures| failures.iter().rev().cloned().collect())
            .unwrap_or_default()
    }

    /// Acquires the per-session tool-call serialization guard.
    ///
    /// The owned guard is intentionally independent of the runtime borrow so it
    /// can be retained across `spawn_blocking(...).await` by the server handler.
    pub(crate) async fn acquire_call(&self) -> Option<OwnedMutexGuard<()>> {
        let guard = Arc::clone(&self.inner.call_gate).lock_owned().await;
        (!self.inner.closing.load(Ordering::Acquire)).then_some(guard)
    }

    /// Records one operation for the current session without failing the call.
    ///
    /// The runtime overwrites any caller-supplied session id, ensuring every MCP
    /// ledger row is linked to this connection. Compression callers must also
    /// place the same id into the trace before serializing it because Store
    /// validates the trace and operation roots together.
    pub(crate) fn record_operation(&self, mut operation: OperationRecord) -> bool {
        let Some(session_id) = self.current() else {
            self.inner.diagnostics.warn(&format!(
                "compresso MCP ledger write skipped for tool {:?} operation {:?}: session is not initialized",
                operation.tool, operation.id
            ));
            return false;
        };
        operation.session_id = Some(session_id);

        if self.inner.operation_writer.write(&operation).is_err() {
            self.inner.diagnostics.warn(&format!(
                "compresso MCP ledger write failed for tool {:?} operation {:?}; continuing without persistence",
                operation.tool, operation.id
            ));
            return false;
        }
        true
    }

    /// Marks the initialized session ended after transport shutdown.
    ///
    /// EOF before initialization is a clean no-op. Repeated calls after a
    /// successful close do not write a second end timestamp.
    pub(crate) async fn close(&self) -> Result<(), SessionError> {
        let _call_guard = Arc::clone(&self.inner.call_gate).lock_owned().await;
        self.inner.closing.store(true, Ordering::Release);
        let Some(session_id) = self.current() else {
            return Ok(());
        };
        let store = self.inner.store.clone();
        self.inner
            .closed
            .get_or_try_init(|| async move {
                tokio::task::spawn_blocking(move || store.close_session(&session_id))
                    .await
                    .map_err(SessionError::Task)?
                    .map_err(SessionError::Store)
            })
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use compresso_core::{Mode, operation_identity};

    use super::*;

    #[derive(Default)]
    struct CapturingDiagnostics {
        warnings: StdMutex<Vec<String>>,
    }

    impl DiagnosticSink for CapturingDiagnostics {
        fn warn(&self, message: &str) {
            self.warnings
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(message.to_owned());
        }
    }

    #[derive(Default)]
    struct FailingWriter {
        calls: AtomicUsize,
    }

    impl OperationWriter for FailingWriter {
        fn write(&self, _operation: &OperationRecord) -> Result<(), StoreError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(StoreError::InvalidField {
                field: "injected operation writer",
                reason: "deterministic test failure",
            })
        }
    }

    fn retrieval_operation(id: &str, created_at: i64) -> OperationRecord {
        OperationRecord {
            id: id.to_owned(),
            session_id: None,
            tool: "compresso_search".to_owned(),
            mode: Mode::Safe,
            observed_tokens: 17,
            applied_tokens: 17,
            compression_overhead: 0,
            fallback_used: false,
            tokens_served: Some(17),
            retrieval: None,
            trace_json: None,
            created_at,
        }
    }

    #[test]
    fn test_recent_failures_keep_the_newest_codes_only() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let runtime = SessionRuntime::new(store);
        assert!(runtime.recent_failures().is_empty());
        for index in 0..(MAX_RECENT_FAILURES + 5) {
            runtime.record_failure(&format!("tool_{index}"), "invalid_arguments");
        }
        let failures = runtime.recent_failures();
        assert_eq!(failures.len(), MAX_RECENT_FAILURES);
        assert_eq!(
            failures[0].0,
            format!("tool_{}", MAX_RECENT_FAILURES + 4),
            "newest first"
        );
        assert_eq!(failures[MAX_RECENT_FAILURES - 1].0, "tool_5");
    }

    #[tokio::test]
    async fn test_start_is_exactly_once_and_close_is_idempotent() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let runtime = SessionRuntime::new(store.clone());
        let changes_before = store.total_changes().expect("changes should be readable");

        let (first, second) =
            tokio::join!(runtime.start("test-harness"), runtime.start("test-harness"));
        let first = first.expect("first start should succeed");
        let second = second.expect("second start should succeed");
        assert_eq!(first, second);
        assert_eq!(
            runtime
                .start("other")
                .await
                .expect("repeated start should succeed"),
            first
        );
        assert_eq!(runtime.current(), Some(first.clone()));
        assert_eq!(
            store.total_changes().expect("changes should be readable"),
            changes_before + 1
        );

        let session = store
            .session_by_id(&first)
            .expect("session should be readable")
            .expect("session should exist");
        assert_eq!(session.client, "test-harness");
        assert_eq!(session.transport, TRANSPORT);
        assert_eq!(session.ended_at, None);

        runtime.close().await.expect("close should succeed");
        let changes_after_close = store.total_changes().expect("changes should be readable");
        runtime
            .close()
            .await
            .expect("second close should be a no-op");
        assert_eq!(
            store.total_changes().expect("changes should be readable"),
            changes_after_close
        );
        assert!(
            store
                .session_by_id(&first)
                .expect("session should be readable")
                .expect("session should exist")
                .ended_at
                .is_some()
        );
    }

    #[tokio::test]
    async fn test_empty_client_uses_unknown_fallback() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let runtime = SessionRuntime::new(store.clone());

        let session_id = runtime.start("   ").await.expect("start should succeed");
        let session = store
            .session_by_id(&session_id)
            .expect("session should be readable")
            .expect("session should exist");
        assert_eq!(session.client, UNKNOWN_CLIENT);
    }

    #[tokio::test]
    async fn test_call_guard_is_exclusive() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let runtime = SessionRuntime::new(store);

        let first = runtime
            .acquire_call()
            .await
            .expect("open runtime should admit a call");
        assert!(
            Arc::clone(&runtime.inner.call_gate)
                .try_lock_owned()
                .is_err()
        );
        drop(first);
        assert!(
            Arc::clone(&runtime.inner.call_gate)
                .try_lock_owned()
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_close_rejects_delayed_calls_after_waiting_for_active_call() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let runtime = SessionRuntime::new(store);
        runtime
            .start("test-harness")
            .await
            .expect("start should succeed");
        let active = runtime
            .acquire_call()
            .await
            .expect("open runtime should admit a call");
        let closing = runtime.clone();
        let close = tokio::spawn(async move { closing.close().await });

        tokio::task::yield_now().await;
        assert!(!close.is_finished());
        drop(active);
        close
            .await
            .expect("close task should not panic")
            .expect("close should succeed");
        assert!(runtime.acquire_call().await.is_none());
        assert!(matches!(
            runtime.start("late-client").await,
            Err(SessionError::Closing)
        ));
    }

    #[tokio::test]
    async fn test_production_writer_injects_current_session() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let runtime = SessionRuntime::new(store.clone());
        let session_id = runtime
            .start("test-harness")
            .await
            .expect("start should succeed");
        let identity = operation_identity();

        assert!(runtime.record_operation(retrieval_operation(&identity.id, identity.created_at)));
        let operation = store
            .operation_by_id(&identity.id)
            .expect("operation should be readable")
            .expect("operation should exist");
        assert_eq!(operation.session_id, Some(session_id));
    }

    #[tokio::test]
    async fn test_ledger_failure_is_fail_open_and_warns_once() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        let writer = Arc::new(FailingWriter::default());
        let diagnostics = Arc::new(CapturingDiagnostics::default());
        let runtime = SessionRuntime::with_components(store, writer.clone(), diagnostics.clone());
        runtime
            .start("test-harness")
            .await
            .expect("start should succeed");
        let identity = operation_identity();

        assert!(!runtime.record_operation(retrieval_operation(&identity.id, identity.created_at)));
        assert_eq!(writer.calls.load(Ordering::SeqCst), 1);
        let warnings = diagnostics
            .warnings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("compresso_search"));
        assert!(warnings[0].contains(&identity.id));
        assert!(!warnings[0].contains("deterministic test failure"));
    }
}
