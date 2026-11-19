//! `compresso edit`: the operator-run form of the opt-in edit tool (E17-32,
//! D97, ADR-0035). Running the command is the operator's explicit choice, so
//! it needs no enabling flag; it applies the same gates, writes, and report
//! as `compresso_edit` on a server started with `--edits`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use clap::{ArgGroup, Args};
use compresso_atlas::{EditOperation, EditRequest, InsertPosition, apply_edits};
use compresso_core::{Mode, TokenProvider, counter_for, operation_identity};
use compresso_store::{OperationRecord, Store};
use serde_json::{Value, json};

use super::OutputArgs;
use super::retrieval::{RepositoryArgs, RetrievalError, resolve_repository};

/// Largest `--operations` document read.
const MAX_OPERATIONS_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// The CLI prints the whole report; this only bounds a pathological one.
const CLI_REPORT_MAX_TOKENS: usize = 100_000;

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("operation")
        .required(true)
        .args(["rename", "replace", "insert", "operations"])
))]
pub struct EditArgs {
    #[command(flatten)]
    pub repository: RepositoryArgs,
    /// Rename the identifier OLD to NEW at identifier boundaries in the
    /// indexed files (narrow the scope with --path).
    #[arg(long, num_args = 2, value_names = ["OLD", "NEW"])]
    pub rename: Option<Vec<String>>,
    /// Replace the exact text --old with --new in this repository file.
    #[arg(long, value_name = "REPO_PATH")]
    pub replace: Option<String>,
    /// Insert --text before or after the unique --anchor in this file.
    #[arg(long, value_name = "REPO_PATH")]
    pub insert: Option<String>,
    /// A JSON array of operations in the compresso_edit form; `-` reads stdin.
    #[arg(long, value_name = "FILE")]
    pub operations: Option<PathBuf>,
    #[arg(long, requires = "replace")]
    pub old: Option<String>,
    #[arg(long, requires = "replace")]
    pub new: Option<String>,
    #[arg(long, requires = "insert")]
    pub anchor: Option<String>,
    #[arg(long, requires = "insert")]
    pub text: Option<String>,
    /// Insert before the anchor instead of after it.
    #[arg(long, requires = "insert")]
    pub before: bool,
    /// Rename scope: a repository-relative prefix or glob.
    #[arg(long, value_name = "FILTER", requires = "rename")]
    pub path: Option<String>,
    /// Rename even though NEW already occurs as an identifier in scope.
    #[arg(long, requires = "rename")]
    pub allow_existing: bool,
    /// Matches required; nothing is written when the count differs.
    #[arg(long, value_name = "N")]
    pub expected_count: Option<usize>,
    /// Print the report and a unified diff without writing.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub output: OutputArgs,
}

pub(crate) struct Rendered {
    pub json: Value,
    pub human: String,
}

fn invalid(message: impl Into<String>) -> RetrievalError {
    RetrievalError {
        code: "invalid_arguments",
        message: message.into(),
    }
}

fn operations(
    arguments: &EditArgs,
    input: &mut dyn Read,
) -> Result<Vec<EditOperation>, RetrievalError> {
    if let Some([old, new]) = arguments.rename.as_deref() {
        return Ok(vec![EditOperation::Rename {
            old: old.clone(),
            new: new.clone(),
            path: arguments.path.clone(),
            expected_count: arguments.expected_count,
            allow_existing: arguments.allow_existing,
        }]);
    }
    if let Some(path) = &arguments.replace {
        let (Some(old), Some(new)) = (&arguments.old, &arguments.new) else {
            return Err(invalid("--replace requires --old and --new"));
        };
        return Ok(vec![EditOperation::Replace {
            path: path.clone(),
            old: old.clone(),
            new: new.clone(),
            expected_count: arguments.expected_count.unwrap_or(1),
            expected_hash: None,
        }]);
    }
    if let Some(path) = &arguments.insert {
        let (Some(anchor), Some(text)) = (&arguments.anchor, &arguments.text) else {
            return Err(invalid("--insert requires --anchor and --text"));
        };
        if arguments.expected_count.is_some() {
            return Err(invalid(
                "--insert takes no --expected-count; its anchor must be unique",
            ));
        }
        return Ok(vec![EditOperation::Insert {
            path: path.clone(),
            anchor: anchor.clone(),
            text: text.clone(),
            position: if arguments.before {
                InsertPosition::Before
            } else {
                InsertPosition::After
            },
            expected_hash: None,
        }]);
    }
    let Some(file) = &arguments.operations else {
        return Err(invalid(
            "pass --rename, --replace, --insert, or --operations",
        ));
    };
    if arguments.expected_count.is_some() {
        return Err(invalid("--operations carries expected_count per operation"));
    }
    let mut document = Vec::new();
    let read = if file.as_os_str() == "-" {
        input
            .take(MAX_OPERATIONS_FILE_BYTES + 1)
            .read_to_end(&mut document)
    } else {
        fs::File::open(file).and_then(|handle| {
            handle
                .take(MAX_OPERATIONS_FILE_BYTES + 1)
                .read_to_end(&mut document)
        })
    };
    read.map_err(|_| invalid("--operations could not be read"))?;
    if document.len() as u64 > MAX_OPERATIONS_FILE_BYTES {
        return Err(invalid("--operations is larger than 4 MiB"));
    }
    let value = serde_json::from_slice::<Value>(&document)
        .map_err(|_| invalid("--operations must be a JSON array of operation objects"))?;
    compresso_mcp::edit::parse_operations_value(Some(value)).map_err(invalid)
}

pub(crate) fn execute(
    arguments: EditArgs,
    store: &Store,
    cwd: &Path,
    input: &mut dyn Read,
) -> Result<Rendered, RetrievalError> {
    let operations = operations(&arguments, input)?;
    let repository = resolve_repository(store, arguments.repository.root.as_deref(), cwd)?;
    let request = EditRequest {
        operations,
        dry_run: arguments.dry_run,
        protected: compresso_mcp::edit::protected_paths(store, &repository.root),
    };
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let report = apply_edits(store, &repository.root, &request, counter).map_err(|error| {
        let (code, message) = compresso_mcp::edit::edit_failure_message(&error);
        RetrievalError { code, message }
    })?;
    let identity = operation_identity();
    let payload = compresso_mcp::edit::report_payload(&report, &request.operations, &identity.id);
    let (json, _) = compresso_mcp::edit::fit(payload, CLI_REPORT_MAX_TOKENS, false, counter);
    let human = compresso_mcp::edit::render_text(&json);
    let tokens = counter.count(&human);
    // The ledger records that an edit ran and its size, never file content.
    let _ = store.record_operation(&OperationRecord {
        id: identity.id,
        session_id: None,
        tool: compresso_mcp::edit::TOOL_NAME.to_owned(),
        mode: Mode::Passthrough,
        observed_tokens: tokens,
        applied_tokens: tokens,
        compression_overhead: 0,
        fallback_used: false,
        tokens_served: Some(tokens),
        retrieval: None,
        trace_json: None,
        created_at: identity.created_at,
    });
    Ok(Rendered {
        json: json!({ "schema_version": 1, "data": json }),
        human,
    })
}
