//! Opt-in, verified literal edits of indexed repository files (E17-32, D97,
//! ADR-0035).
//!
//! Every other Atlas entry point only reads. This module is the one place
//! Compresso writes repository files, and only when an operator enabled it
//! (`compresso mcp serve --edits`, or running `compresso edit` directly).
//!
//! # Operations
//!
//! * `replace`: exact literal `old` to `new` in one file, `expected_count`
//!   times (default 1).
//! * `rename`: whole-identifier rename across the indexed files of a scope.
//! * `insert`: text before or after a literal anchor that occurs exactly once.
//!
//! One call applies up to [`EDIT_MAX_OPERATIONS`] operations in order, all or
//! nothing: every operation is validated and applied in memory first, and no
//! file is written unless all of them succeed. No regular expression is ever
//! evaluated, no file is created or deleted, no shell command runs, and no
//! network request is made.
//!
//! # Identifier boundaries
//!
//! An identifier character is `_` or any Unicode alphanumeric character
//! (`char::is_alphanumeric`: letters and numbers in any script). `$` is not an
//! identifier character, so `$name` in a shell script or macro is the
//! identifier `name`. A rename matches `old` only where the character before
//! the match (if any) and the character after it (if any) are not identifier
//! characters. Both names must consist of identifier characters only and must
//! not start with a number. Matching is textual: occurrences inside comments
//! and strings are renamed too, and the report lists every remaining substring
//! occurrence (for example inside a longer identifier) so nothing is hidden.
//!
//! # Gates
//!
//! A target must be an indexed file, not a secret path, not excluded by the
//! scanner (for example gitignored), reached without any symbolic-link
//! component (the [`resolve_under_root`] gate shared with every read), inside
//! the repository root, outside `.git` and any protected directory (the
//! Compresso store), valid UTF-8, and no larger than [`EDIT_MAX_FILE_BYTES`]
//! before and after the edit.
//!
//! # Writes
//!
//! Each file is written atomically: a temporary file in the same directory is
//! written, given the original permissions, and synced; then, immediately
//! before it is renamed over the target, the target is resolved through the
//! gate again and must still be the file that was read (device and inode on
//! Unix, and the content hash). A batch is validated and re-verified as a
//! whole before the first write; if a later write fails, every file already
//! written is restored from the in-memory original and the error names what
//! was restored and what was not. Line endings (LF or CRLF) and a UTF-8 byte
//! order mark are preserved: `old`, `new`, anchors, and inserted text written
//! with `\n` are converted to the file's CRLF endings.
//!
//! A residual window remains between the last verification and the rename
//! itself: the standard library offers no directory-handle-relative rename, so
//! a directory component swapped in that instant is not detected.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use compresso_core::TokenCounter;
use compresso_store::{FileRecord, Store, StoreError};

use crate::AtlasError;
use crate::grep::PathFilter;
use crate::indexer::Indexer;
use crate::queries::{QueryError, ResolvedEntry, canonical_query_root, resolve_under_root};
use crate::scanner::{DEFAULT_MAX_FILE_BYTES, is_scanner_excluded, is_secret_path};

/// Most operations one call applies.
pub const EDIT_MAX_OPERATIONS: usize = 16;
/// Largest `old`, `new`, anchor, or inserted text, in bytes.
pub const EDIT_MAX_TEXT_BYTES: usize = 64 * 1024;
/// Longest identifier a rename accepts, in bytes.
pub const EDIT_MAX_IDENTIFIER_BYTES: usize = 256;
/// Most files one call changes.
pub const EDIT_MAX_FILES: usize = 200;
/// Most edit sites one call changes, across all operations.
pub const EDIT_MAX_SITES: usize = 2_000;
/// Most bytes one call writes, across all files.
pub const EDIT_MAX_WRITE_BYTES: u64 = 16 * 1024 * 1024;
/// Largest file an edit reads or writes: the scanner's limit, so an edited
/// file never grows out of the index.
pub const EDIT_MAX_FILE_BYTES: u64 = DEFAULT_MAX_FILE_BYTES;
/// Most bytes a rename reads to search its scope.
pub const EDIT_MAX_SCAN_BYTES: u64 = 256 * 1024 * 1024;
/// Most locations listed per list in a report or error; totals are exact.
pub const EDIT_MAX_LISTED_LOCATIONS: usize = 20;
/// Largest dry-run diff, in bytes.
pub const EDIT_MAX_DIFF_BYTES: usize = 64 * 1024;
const DIFF_CONTEXT_LINES: usize = 2;
/// Lines looked ahead on each side to re-align a diff after a change.
const DIFF_RESYNC_WINDOW: usize = 64;
const LOCATION_TEXT_MAX_BYTES: usize = 160;
const MAX_PATH_BYTES: usize = 4_096;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Where `insert` places its text relative to the anchor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InsertPosition {
    Before,
    After,
}

/// One edit operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditOperation {
    /// Replaces every occurrence of `old` in `path`, which must occur exactly
    /// `expected_count` times.
    Replace {
        path: String,
        old: String,
        new: String,
        expected_count: usize,
        /// Optional BLAKE3 hex of the file's current bytes.
        expected_hash: Option<String>,
    },
    /// Renames the identifier `old` to `new` at identifier boundaries in the
    /// indexed files `path` selects (a prefix or glob; all when `None`).
    Rename {
        old: String,
        new: String,
        path: Option<String>,
        expected_count: Option<usize>,
    },
    /// Inserts `text` before or after `anchor`, which must occur exactly once.
    Insert {
        path: String,
        anchor: String,
        text: String,
        position: InsertPosition,
        expected_hash: Option<String>,
    },
}

impl EditOperation {
    /// The operator-facing operation name.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Replace { .. } => "replace",
            Self::Rename { .. } => "rename",
            Self::Insert { .. } => "insert",
        }
    }
}

/// One edit call.
#[derive(Clone, Debug, Default)]
pub struct EditRequest {
    pub operations: Vec<EditOperation>,
    /// Report and diff only; write nothing and leave the index as it is.
    pub dry_run: bool,
    /// Canonical files or directories never written, such as the store's
    /// database files.
    pub protected: Vec<PathBuf>,
}

/// One line of a repository file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditLocation {
    pub path: String,
    pub line: u32,
    /// The trimmed line, cut to 160 bytes.
    pub text: String,
}

/// A file's line endings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineEnding {
    Lf,
    Crlf,
    /// Both; inserted text is left as written.
    Mixed,
    /// No line break at all.
    None,
}

impl LineEnding {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "lf",
            Self::Crlf => "crlf",
            Self::Mixed => "mixed",
            Self::None => "none",
        }
    }
}

/// One changed file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileChange {
    pub path: String,
    pub sites: usize,
    /// Lines of the sites, each in the file as it is after the operation
    /// that made it.
    pub lines: Vec<u32>,
    /// The file differed from the index before the edit; matches were
    /// computed against its current content.
    pub stale: bool,
    pub line_ending: LineEnding,
    pub bom: bool,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub hash_before: String,
    pub hash_after: String,
    /// Re-read after the write and found to hold exactly the edited bytes.
    /// Always `false` for a dry run.
    pub verified: bool,
}

/// Why a file in a rename scope was not searched.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    InvalidPath,
    NotFound,
    SymbolicLink,
    Secret,
    Excluded,
    NotIndexed,
    NotAFile,
    TooLarge,
    NotUtf8,
    Protected,
}

impl Refusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidPath => "invalid_path",
            Self::NotFound => "not_found",
            Self::SymbolicLink => "symbolic_link",
            Self::Secret => "secret_path",
            Self::Excluded => "excluded",
            Self::NotIndexed => "not_indexed",
            Self::NotAFile => "not_a_file",
            Self::TooLarge => "too_large",
            Self::NotUtf8 => "not_utf8",
            Self::Protected => "protected",
        }
    }

    const fn describe(self) -> &'static str {
        match self {
            Self::InvalidPath => "the path must be repository-relative with forward slashes",
            Self::NotFound => "the file does not exist",
            Self::SymbolicLink => "the path goes through a symbolic link",
            Self::Secret => "the path is a secret path",
            Self::Excluded => "the scanner excludes the path (for example it is gitignored)",
            Self::NotIndexed => "the file is not in the repository index",
            Self::NotAFile => "the path is not a regular file",
            Self::TooLarge => "the file is over the 1 MB edit limit",
            Self::NotUtf8 => "the file is not valid UTF-8",
            Self::Protected => "the path is inside .git or the Compresso store",
        }
    }
}

/// A file a rename scope held but could not search.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkippedFile {
    pub path: String,
    pub reason: Refusal,
}

/// What a rename left behind in its scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenameOutcome {
    /// The scope filter, or `None` for every indexed file.
    pub scope: Option<String>,
    pub files_searched: usize,
    pub files_skipped: Vec<SkippedFile>,
    /// Whole-identifier occurrences of `old` after the edit.
    pub remaining_whole_total: usize,
    pub remaining_whole: Vec<EditLocation>,
    /// Occurrences of `old` inside longer identifiers after the edit.
    pub remaining_substring_total: usize,
    pub remaining_substring: Vec<EditLocation>,
    /// Whole-identifier occurrences of `new` that existed before the edit.
    pub new_name_preexisting: usize,
}

/// What one operation did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationOutcome {
    pub kind: &'static str,
    pub sites: usize,
    pub files: Vec<String>,
    pub rename: Option<RenameOutcome>,
}

/// Whether the index reflects the edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IndexState {
    /// Dry run, or nothing changed.
    NotRun,
    /// Re-indexed after the write.
    Fresh,
    /// The write stands but re-indexing failed; `compresso index` repairs it.
    Stale,
}

/// The verified outcome of one edit call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditReport {
    pub dry_run: bool,
    pub files: Vec<FileChange>,
    pub operations: Vec<OperationOutcome>,
    /// Compact unified diff, dry run only.
    pub diff: Option<String>,
    pub diff_truncated: bool,
    pub bytes_written: u64,
    pub index: IndexState,
}

/// A bound one call may not exceed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditBound {
    Files,
    Sites,
    WriteBytes,
    FileBytes,
    ScanBytes,
}

impl EditBound {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Sites => "sites",
            Self::WriteBytes => "write_bytes",
            Self::FileBytes => "file_bytes",
            Self::ScanBytes => "scan_bytes",
        }
    }
}

/// Typed edit failures. Every variant except [`EditError::WriteFailed`]
/// means nothing was written.
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("invalid edit request: {reason}")]
    Invalid { reason: String },
    #[error("the repository is not indexed")]
    NotIndexed,
    #[error("path {path:?} resolves outside the repository root")]
    OutsideRoot { path: String },
    #[error("refused to edit {path:?}: {}", reason.describe())]
    Refused { path: String, reason: Refusal },
    #[error(
        "operation {operation} expected {expected} match(es) but found {actual}; nothing was written"
    )]
    CountMismatch {
        operation: usize,
        expected: usize,
        actual: usize,
        locations: Vec<EditLocation>,
    },
    #[error(
        "operation {operation}: the rename found no whole-identifier match; nothing was written"
    )]
    NoMatches {
        operation: usize,
        substring_total: usize,
        substring: Vec<EditLocation>,
    },
    #[error("{path:?} does not have the expected content hash; nothing was written")]
    HashMismatch { path: String, actual: String },
    #[error("the edit exceeds the {} bound ({actual} > {limit}); nothing was written", bound.as_str())]
    Bound {
        bound: EditBound,
        limit: u64,
        actual: u64,
    },
    #[error("{path:?} changed while the edit was prepared; nothing was written")]
    Changed { path: String },
    #[error("writing {path:?} failed; restored {} file(s), could not restore {}", restored.len(), unrestored.len())]
    WriteFailed {
        path: String,
        /// Files written and then restored to their original bytes.
        restored: Vec<String>,
        /// Files left with the edited bytes because restoring them failed.
        unrestored: Vec<String>,
        #[source]
        source: io::Error,
    },
    #[error("reading {path:?} failed; nothing was written")]
    Read {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Atlas(Box<AtlasError>),
}

impl From<AtlasError> for EditError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(Box::new(error))
    }
}

/// Whether `character` is part of an identifier (see the module docs).
pub fn is_identifier_char(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

/// Whether `name` is an identifier a rename accepts.
pub fn is_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= EDIT_MAX_IDENTIFIER_BYTES
        && name.chars().all(is_identifier_char)
        && !name.chars().next().is_some_and(char::is_numeric)
}

/// Byte offsets of whole-identifier and substring-only occurrences of
/// `name` in `text`, in order.
pub fn identifier_occurrences(text: &str, name: &str) -> (Vec<usize>, Vec<usize>) {
    let mut whole = Vec::new();
    let mut substring = Vec::new();
    if name.is_empty() {
        return (whole, substring);
    }
    for (start, _) in text.match_indices(name) {
        let end = start + name.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        if before.is_some_and(is_identifier_char) || after.is_some_and(is_identifier_char) {
            substring.push(start);
        } else {
            whole.push(start);
        }
    }
    (whole, substring)
}

/// Applies `request` to the repository at `root`, then re-indexes it.
///
/// `counter` must be the counter `compresso index` uses, so re-indexing
/// re-parses only the changed files.
pub fn apply_edits(
    store: &Store,
    root: &Path,
    request: &EditRequest,
    counter: &dyn TokenCounter,
) -> Result<EditReport, EditError> {
    apply_edits_with_hook(store, root, request, counter, &mut |_, _| Ok(()))
}

/// [`apply_edits`] with a hook run before each file write, given the write's
/// position and the file's absolute path. An error from the hook fails that
/// write. Used to test rollback and swaps between validation and write.
pub(crate) fn apply_edits_with_hook(
    store: &Store,
    root: &Path,
    request: &EditRequest,
    counter: &dyn TokenCounter,
    before_write: &mut dyn FnMut(usize, &Path) -> io::Result<()>,
) -> Result<EditReport, EditError> {
    validate_request(request)?;
    let root = canonical_query_root(root)?;
    let repository = store.repo_by_root(&root)?.ok_or(EditError::NotIndexed)?;
    if repository.last_indexed_at.is_none() {
        return Err(EditError::NotIndexed);
    }
    let indexed = store
        .indexed_files(repository.id)?
        .into_iter()
        .map(|record| (record.path.clone(), record))
        .collect::<BTreeMap<_, _>>();
    let gate = Gate {
        root: &root,
        indexed: &indexed,
        protected: &request.protected,
    };

    let mut working = BTreeMap::<String, WorkingFile>::new();
    let mut outcomes = Vec::with_capacity(request.operations.len());
    let mut total_sites = 0_usize;
    for (index, operation) in request.operations.iter().enumerate() {
        let number = index + 1;
        let outcome = match operation {
            EditOperation::Replace {
                path,
                old,
                new,
                expected_count,
                expected_hash,
            } => {
                let file = load_into(&gate, &mut working, path)?;
                check_hash(file, path, expected_hash.as_deref())?;
                apply_replace(file, path, old, new, *expected_count, number)?
            }
            EditOperation::Insert {
                path,
                anchor,
                text,
                position,
                expected_hash,
            } => {
                let file = load_into(&gate, &mut working, path)?;
                check_hash(file, path, expected_hash.as_deref())?;
                apply_insert(file, path, anchor, text, *position, number)?
            }
            EditOperation::Rename {
                old,
                new,
                path,
                expected_count,
            } => apply_rename(
                &gate,
                &mut working,
                old,
                new,
                path.as_deref(),
                *expected_count,
                number,
            )?,
        };
        total_sites = total_sites.saturating_add(outcome.sites);
        bound(EditBound::Sites, EDIT_MAX_SITES as u64, total_sites as u64)?;
        outcomes.push(outcome);
    }

    let changed = working
        .into_iter()
        .filter(|(_, file)| file.edited_bytes() != file.original)
        .collect::<Vec<_>>();
    bound(
        EditBound::Files,
        EDIT_MAX_FILES as u64,
        changed.len() as u64,
    )?;
    let mut bytes_written = 0_u64;
    for (_, file) in &changed {
        let size = file.edited_bytes().len() as u64;
        bound(EditBound::FileBytes, EDIT_MAX_FILE_BYTES, size)?;
        bytes_written = bytes_written.saturating_add(size);
    }
    bound(EditBound::WriteBytes, EDIT_MAX_WRITE_BYTES, bytes_written)?;

    let mut files = changed
        .iter()
        .map(|(path, file)| file.change(path))
        .collect::<Vec<_>>();
    // A file whose operations cancel out is not written and not reported.
    for outcome in &mut outcomes {
        outcome
            .files
            .retain(|path| changed.iter().any(|(changed, _)| changed == path));
    }

    if request.dry_run {
        let mut diff = String::new();
        let mut diff_truncated = false;
        for (path, file) in &changed {
            let original = String::from_utf8_lossy(&file.original);
            let edited = String::from_utf8_lossy(&file.edited_bytes()).into_owned();
            if !append_diff(&mut diff, path, &original, &edited) {
                diff_truncated = true;
                break;
            }
        }
        return Ok(EditReport {
            dry_run: true,
            files,
            operations: outcomes,
            diff: Some(diff),
            diff_truncated,
            bytes_written: 0,
            index: IndexState::NotRun,
        });
    }
    if changed.is_empty() {
        return Ok(EditReport {
            dry_run: false,
            files,
            operations: outcomes,
            diff: None,
            diff_truncated: false,
            bytes_written: 0,
            index: IndexState::NotRun,
        });
    }

    commit(&root, &changed, before_write)?;
    for change in &mut files {
        change.verified = on_disk_hash(&root, &change.path).as_deref() == Some(&change.hash_after);
    }
    let index = match Indexer::new(store, counter).index(&root, |_| {}) {
        Ok(_) => IndexState::Fresh,
        Err(_) => IndexState::Stale,
    };
    Ok(EditReport {
        dry_run: false,
        files,
        operations: outcomes,
        diff: None,
        diff_truncated: false,
        bytes_written,
        index,
    })
}

fn bound(bound: EditBound, limit: u64, actual: u64) -> Result<(), EditError> {
    if actual > limit {
        Err(EditError::Bound {
            bound,
            limit,
            actual,
        })
    } else {
        Ok(())
    }
}

fn invalid(reason: impl Into<String>) -> EditError {
    EditError::Invalid {
        reason: reason.into(),
    }
}

fn validate_request(request: &EditRequest) -> Result<(), EditError> {
    if request.operations.is_empty() || request.operations.len() > EDIT_MAX_OPERATIONS {
        return Err(invalid(format!(
            "a call carries 1 to {EDIT_MAX_OPERATIONS} operations"
        )));
    }
    let text = |name: &str, value: &str, allow_empty: bool| -> Result<(), EditError> {
        if !allow_empty && value.is_empty() {
            return Err(invalid(format!("'{name}' must not be empty")));
        }
        if value.len() > EDIT_MAX_TEXT_BYTES {
            return Err(invalid(format!(
                "'{name}' must be at most {EDIT_MAX_TEXT_BYTES} bytes"
            )));
        }
        if value.contains('\0') {
            return Err(invalid(format!("'{name}' must not contain NUL")));
        }
        Ok(())
    };
    let hash = |value: Option<&str>| -> Result<(), EditError> {
        match value {
            Some(hash)
                if hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) =>
            {
                Err(invalid(
                    "'expected_hash' must be 64 lowercase hex characters (BLAKE3)",
                ))
            }
            _ => Ok(()),
        }
    };
    for operation in &request.operations {
        match operation {
            EditOperation::Replace {
                path,
                old,
                new,
                expected_count,
                expected_hash,
            } => {
                validate_path(path)?;
                text("old", old, false)?;
                text("new", new, true)?;
                if old == new {
                    return Err(invalid("'old' and 'new' are identical"));
                }
                if !(1..=EDIT_MAX_SITES).contains(expected_count) {
                    return Err(invalid(format!(
                        "'expected_count' must be from 1 through {EDIT_MAX_SITES}"
                    )));
                }
                hash(expected_hash.as_deref())?;
            }
            EditOperation::Rename {
                old,
                new,
                path,
                expected_count,
            } => {
                for (name, value) in [("old", old), ("new", new)] {
                    if !is_identifier(value) {
                        return Err(invalid(format!(
                            "rename '{name}' must be an identifier: letters, digits, or '_', not starting with a digit, at most {EDIT_MAX_IDENTIFIER_BYTES} bytes"
                        )));
                    }
                }
                if old == new {
                    return Err(invalid("'old' and 'new' are identical"));
                }
                if let Some(filter) = path {
                    PathFilter::parse(filter).map_err(|error| invalid(error.to_string()))?;
                }
                if expected_count.is_some_and(|count| !(1..=EDIT_MAX_SITES).contains(&count)) {
                    return Err(invalid(format!(
                        "'expected_count' must be from 1 through {EDIT_MAX_SITES}"
                    )));
                }
            }
            EditOperation::Insert {
                path,
                anchor,
                text: inserted,
                expected_hash,
                ..
            } => {
                validate_path(path)?;
                text("anchor", anchor, false)?;
                text("text", inserted, false)?;
                hash(expected_hash.as_deref())?;
            }
        }
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<(), EditError> {
    let valid = !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && !path.contains('\0')
        && !path.contains('\\')
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    if valid {
        Ok(())
    } else {
        Err(EditError::Refused {
            path: path.to_owned(),
            reason: Refusal::InvalidPath,
        })
    }
}

/// The checks every target passes before it is read.
struct Gate<'a> {
    root: &'a Path,
    indexed: &'a BTreeMap<String, FileRecord>,
    protected: &'a [PathBuf],
}

/// A file read for editing, with its edited text.
struct WorkingFile {
    original: Vec<u8>,
    hash_before: String,
    identity: Option<(u64, u64)>,
    permissions: fs::Permissions,
    bom: bool,
    line_ending: LineEnding,
    stale: bool,
    /// Edited text without the byte order mark.
    text: String,
    sites: usize,
    lines: Vec<u32>,
}

impl WorkingFile {
    fn edited_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.text.len() + 3);
        if self.bom {
            bytes.extend_from_slice("\u{feff}".as_bytes());
        }
        bytes.extend_from_slice(self.text.as_bytes());
        bytes
    }

    fn change(&self, path: &str) -> FileChange {
        let edited = self.edited_bytes();
        FileChange {
            path: path.to_owned(),
            sites: self.sites,
            lines: self.lines.clone(),
            stale: self.stale,
            line_ending: self.line_ending,
            bom: self.bom,
            bytes_before: self.original.len() as u64,
            bytes_after: edited.len() as u64,
            hash_before: self.hash_before.clone(),
            hash_after: blake3::hash(&edited).to_hex().to_string(),
            verified: false,
        }
    }

    /// `value` with lone `\n` turned into `\r\n` in a CRLF file.
    fn native(&self, value: &str) -> String {
        if self.line_ending == LineEnding::Crlf && !value.contains('\r') {
            value.replace('\n', "\r\n")
        } else {
            value.to_owned()
        }
    }
}

fn load_into<'w>(
    gate: &Gate<'_>,
    working: &'w mut BTreeMap<String, WorkingFile>,
    path: &str,
) -> Result<&'w mut WorkingFile, EditError> {
    if !working.contains_key(path) {
        let file = load(gate, path)?;
        working.insert(path.to_owned(), file);
    }
    Ok(working.get_mut(path).expect("inserted above"))
}

fn refused(path: &str, reason: Refusal) -> EditError {
    EditError::Refused {
        path: path.to_owned(),
        reason,
    }
}

fn load(gate: &Gate<'_>, path: &str) -> Result<WorkingFile, EditError> {
    validate_path(path)?;
    let relative = Path::new(path);
    if is_secret_path(relative) {
        return Err(refused(path, Refusal::Secret));
    }
    if relative
        .components()
        .any(|component| component.as_os_str() == ".git")
    {
        return Err(refused(path, Refusal::Protected));
    }
    if is_scanner_excluded(gate.root, relative) {
        return Err(refused(path, Refusal::Excluded));
    }
    let Some(record) = gate.indexed.get(path) else {
        return Err(refused(path, Refusal::NotIndexed));
    };
    let resolved = match resolve_under_root(gate.root, path) {
        Ok(ResolvedEntry::File(resolved)) => resolved,
        Ok(ResolvedEntry::Missing) => {
            let reason = if traverses_symlink(gate.root, relative) {
                Refusal::SymbolicLink
            } else {
                Refusal::NotFound
            };
            return Err(refused(path, reason));
        }
        Err(AtlasError::OutsideRoot { .. }) => {
            return Err(EditError::OutsideRoot {
                path: path.to_owned(),
            });
        }
        Err(AtlasError::Query(QueryError::NotAFile { .. })) => {
            return Err(refused(path, Refusal::NotAFile));
        }
        Err(error) => return Err(error.into()),
    };
    if gate
        .protected
        .iter()
        .any(|protected| resolved.path().starts_with(protected))
    {
        return Err(refused(path, Refusal::Protected));
    }
    let read_error = |source: io::Error| EditError::Read {
        path: path.to_owned(),
        source,
    };
    let file = resolved
        .open()
        .map_err(|_| refused(path, Refusal::NotFound))?;
    let metadata = file.metadata().map_err(read_error)?;
    if metadata.len() > EDIT_MAX_FILE_BYTES {
        return Err(refused(path, Refusal::TooLarge));
    }
    let mut original = Vec::new();
    file.take(EDIT_MAX_FILE_BYTES + 1)
        .read_to_end(&mut original)
        .map_err(read_error)?;
    if original.len() as u64 > EDIT_MAX_FILE_BYTES {
        return Err(refused(path, Refusal::TooLarge));
    }
    let Ok(text) = String::from_utf8(original.clone()) else {
        return Err(refused(path, Refusal::NotUtf8));
    };
    let hash_before = blake3::hash(&original).to_hex().to_string();
    let stale = hash_before != record.hash || original.len() as u64 != record.size_bytes;
    let (bom, text) = match text.strip_prefix('\u{feff}') {
        Some(rest) => (true, rest.to_owned()),
        None => (false, text),
    };
    Ok(WorkingFile {
        line_ending: line_ending(&text),
        original,
        hash_before,
        identity: resolved.identity(),
        permissions: metadata.permissions(),
        bom,
        stale,
        text,
        sites: 0,
        lines: Vec::new(),
    })
}

fn traverses_symlink(root: &Path, relative: &Path) -> bool {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if let Component::Normal(part) = component {
            current.push(part);
            if fs::symlink_metadata(&current)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                return true;
            }
        }
    }
    false
}

fn line_ending(text: &str) -> LineEnding {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count();
    match (crlf, lf) {
        (0, 0) => LineEnding::None,
        (0, _) => LineEnding::Lf,
        (crlf, lf) if crlf == lf => LineEnding::Crlf,
        _ => LineEnding::Mixed,
    }
}

fn check_hash(file: &WorkingFile, path: &str, expected: Option<&str>) -> Result<(), EditError> {
    match expected {
        Some(expected) if expected != file.hash_before => Err(EditError::HashMismatch {
            path: path.to_owned(),
            actual: file.hash_before.clone(),
        }),
        _ => Ok(()),
    }
}

/// 1-based line lookup over one text.
struct Lines<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(index, _)| index + 1));
        Self { text, starts }
    }

    fn line(&self, offset: usize) -> u32 {
        let index = self.starts.partition_point(|start| *start <= offset);
        u32::try_from(index.max(1)).unwrap_or(u32::MAX)
    }

    fn location(&self, path: &str, offset: usize) -> EditLocation {
        let line = self.line(offset);
        let start = self.starts[line as usize - 1];
        let end = self.text[start..]
            .find('\n')
            .map_or(self.text.len(), |end| start + end);
        let mut text = self.text[start..end].trim();
        if text.len() > LOCATION_TEXT_MAX_BYTES {
            let mut cut = LOCATION_TEXT_MAX_BYTES;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text = &text[..cut];
        }
        EditLocation {
            path: path.to_owned(),
            line,
            text: text.to_owned(),
        }
    }
}

fn locations(path: &str, text: &str, offsets: &[usize]) -> Vec<EditLocation> {
    let lines = Lines::new(text);
    offsets
        .iter()
        .take(EDIT_MAX_LISTED_LOCATIONS)
        .map(|offset| lines.location(path, *offset))
        .collect()
}

/// Replaces `length` bytes at each of `offsets` (ascending, disjoint) with
/// `replacement`; returns the new text and the replacement offsets in it.
fn splice(text: &str, offsets: &[usize], length: usize, replacement: &str) -> (String, Vec<usize>) {
    let mut result = String::with_capacity(text.len() + offsets.len() * replacement.len());
    let mut placed = Vec::with_capacity(offsets.len());
    let mut cursor = 0;
    for offset in offsets {
        result.push_str(&text[cursor..*offset]);
        placed.push(result.len());
        result.push_str(replacement);
        cursor = offset + length;
    }
    result.push_str(&text[cursor..]);
    (result, placed)
}

fn record_sites(file: &mut WorkingFile, placed: &[usize]) {
    let lines = Lines::new(&file.text);
    file.sites += placed.len();
    file.lines
        .extend(placed.iter().map(|offset| lines.line(*offset)));
}

fn apply_replace(
    file: &mut WorkingFile,
    path: &str,
    old: &str,
    new: &str,
    expected: usize,
    operation: usize,
) -> Result<OperationOutcome, EditError> {
    let old = file.native(old);
    let new = file.native(new);
    let offsets = file
        .text
        .match_indices(old.as_str())
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    if offsets.len() != expected {
        return Err(EditError::CountMismatch {
            operation,
            expected,
            actual: offsets.len(),
            locations: locations(path, &file.text, &offsets),
        });
    }
    let (text, placed) = splice(&file.text, &offsets, old.len(), &new);
    file.text = text;
    record_sites(file, &placed);
    Ok(OperationOutcome {
        kind: "replace",
        sites: placed.len(),
        files: vec![path.to_owned()],
        rename: None,
    })
}

fn apply_insert(
    file: &mut WorkingFile,
    path: &str,
    anchor: &str,
    inserted: &str,
    position: InsertPosition,
    operation: usize,
) -> Result<OperationOutcome, EditError> {
    let anchor = file.native(anchor);
    let inserted = file.native(inserted);
    let offsets = file
        .text
        .match_indices(anchor.as_str())
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    if offsets.len() != 1 {
        return Err(EditError::CountMismatch {
            operation,
            expected: 1,
            actual: offsets.len(),
            locations: locations(path, &file.text, &offsets),
        });
    }
    let at = match position {
        InsertPosition::Before => offsets[0],
        InsertPosition::After => offsets[0] + anchor.len(),
    };
    let (text, placed) = splice(&file.text, &[at], 0, &inserted);
    file.text = text;
    record_sites(file, &placed);
    Ok(OperationOutcome {
        kind: "insert",
        sites: 1,
        files: vec![path.to_owned()],
        rename: None,
    })
}

/// Files a rename may skip instead of failing the whole call.
const fn skippable(reason: Refusal) -> bool {
    !matches!(reason, Refusal::InvalidPath | Refusal::NotIndexed)
}

#[allow(clippy::too_many_arguments)]
fn apply_rename(
    gate: &Gate<'_>,
    working: &mut BTreeMap<String, WorkingFile>,
    old: &str,
    new: &str,
    filter: Option<&str>,
    expected: Option<usize>,
    operation: usize,
) -> Result<OperationOutcome, EditError> {
    let path_filter = match filter {
        Some(filter) => PathFilter::parse(filter).map_err(|error| invalid(error.to_string()))?,
        None => None,
    };
    let scope = gate
        .indexed
        .keys()
        .filter(|path| {
            path_filter
                .as_ref()
                .is_none_or(|filter| filter.matches(path))
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut skipped = Vec::new();
    let mut searched = BTreeSet::new();
    let mut matched = Vec::<(String, Vec<usize>)>::new();
    let mut untouched_substring = Vec::<EditLocation>::new();
    let mut untouched_substring_total = 0_usize;
    let mut new_name_preexisting = 0_usize;
    let mut scanned = 0_u64;
    for path in scope {
        let loaded = if working.contains_key(&path) {
            None
        } else {
            match load(gate, &path) {
                Ok(file) => {
                    scanned = scanned.saturating_add(file.original.len() as u64);
                    bound(EditBound::ScanBytes, EDIT_MAX_SCAN_BYTES, scanned)?;
                    Some(file)
                }
                Err(EditError::Refused { reason, .. }) if skippable(reason) => {
                    skipped.push(SkippedFile { path, reason });
                    continue;
                }
                Err(error) => return Err(error),
            }
        };
        let text = loaded
            .as_ref()
            .map_or_else(|| working[&path].text.as_str(), |file| file.text.as_str());
        let (whole, substring) = identifier_occurrences(text, old);
        new_name_preexisting += identifier_occurrences(text, new).0.len();
        searched.insert(path.clone());
        if whole.is_empty() {
            untouched_substring_total += substring.len();
            if untouched_substring.len() < EDIT_MAX_LISTED_LOCATIONS {
                untouched_substring.extend(locations(&path, text, &substring));
            }
        } else {
            matched.push((path.clone(), whole));
            if let Some(file) = loaded {
                working.insert(path, file);
            }
        }
    }
    let total = matched
        .iter()
        .map(|(_, offsets)| offsets.len())
        .sum::<usize>();
    if total == 0 {
        untouched_substring.truncate(EDIT_MAX_LISTED_LOCATIONS);
        return Err(EditError::NoMatches {
            operation,
            substring_total: untouched_substring_total,
            substring: untouched_substring,
        });
    }
    if let Some(expected) = expected
        && expected != total
    {
        let mut listed = Vec::new();
        for (path, offsets) in &matched {
            if listed.len() >= EDIT_MAX_LISTED_LOCATIONS {
                break;
            }
            listed.extend(locations(path, &working[path].text, offsets));
        }
        listed.truncate(EDIT_MAX_LISTED_LOCATIONS);
        return Err(EditError::CountMismatch {
            operation,
            expected,
            actual: total,
            locations: listed,
        });
    }
    bound(
        EditBound::Files,
        EDIT_MAX_FILES as u64,
        matched.len() as u64,
    )?;
    bound(EditBound::Sites, EDIT_MAX_SITES as u64, total as u64)?;

    let mut remaining_whole = Vec::new();
    let mut remaining_whole_total = 0_usize;
    let mut remaining_substring = untouched_substring;
    let mut remaining_substring_total = untouched_substring_total;
    let mut files = Vec::with_capacity(matched.len());
    for (path, offsets) in &matched {
        let file = working.get_mut(path).expect("matched files are loaded");
        let (text, placed) = splice(&file.text, offsets, old.len(), new);
        file.text = text;
        record_sites(file, &placed);
        // Verify against the edited text rather than assume: a new name that
        // contains the old one, or adjacent sites, would show up here.
        let (whole, substring) = identifier_occurrences(&file.text, old);
        remaining_whole_total += whole.len();
        if remaining_whole.len() < EDIT_MAX_LISTED_LOCATIONS {
            remaining_whole.extend(locations(path, &file.text, &whole));
        }
        remaining_substring_total += substring.len();
        if remaining_substring.len() < EDIT_MAX_LISTED_LOCATIONS {
            remaining_substring.extend(locations(path, &file.text, &substring));
        }
        files.push(path.clone());
    }
    remaining_whole.truncate(EDIT_MAX_LISTED_LOCATIONS);
    remaining_substring.truncate(EDIT_MAX_LISTED_LOCATIONS);
    Ok(OperationOutcome {
        kind: "rename",
        sites: total,
        files,
        rename: Some(RenameOutcome {
            scope: filter.map(str::to_owned),
            files_searched: searched.len(),
            files_skipped: skipped,
            remaining_whole_total,
            remaining_whole,
            remaining_substring_total,
            remaining_substring,
            new_name_preexisting,
        }),
    })
}

/// Resolves `path` through the read gate again and confirms it is still the
/// file with `identity` (where known) and `hash`.
fn reverify(
    root: &Path,
    path: &str,
    identity: Option<(u64, u64)>,
    hash: &str,
) -> io::Result<PathBuf> {
    let changed = || io::Error::other("the file changed after it was validated");
    let Ok(ResolvedEntry::File(resolved)) = resolve_under_root(root, path) else {
        return Err(changed());
    };
    if identity.is_some() && resolved.identity() != identity {
        return Err(changed());
    }
    let mut bytes = Vec::new();
    resolved
        .open()
        .map_err(|_| changed())?
        .take(EDIT_MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if blake3::hash(&bytes).to_hex().as_str() != hash {
        return Err(changed());
    }
    Ok(resolved.path().to_path_buf())
}

fn file_identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        fs::symlink_metadata(path)
            .ok()
            .map(|metadata| (metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Writes `bytes` over `path` atomically after re-verifying the target, and
/// returns the new file's identity.
fn write_atomic(
    root: &Path,
    path: &str,
    bytes: &[u8],
    identity: Option<(u64, u64)>,
    hash: &str,
    permissions: &fs::Permissions,
) -> io::Result<Option<(u64, u64)>> {
    let target = reverify(root, path, identity, hash)?;
    let directory = target
        .parent()
        .ok_or_else(|| io::Error::other("the file has no parent directory"))?;
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = directory.join(format!(
        ".{name}.compresso-edit-{}-{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    let written = (|| {
        file.write_all(bytes)?;
        file.set_permissions(permissions.clone())?;
        file.sync_all()?;
        drop(file);
        // The last check before the rename: same gate, same file, same bytes.
        let again = reverify(root, path, identity, hash)?;
        if again != target {
            return Err(io::Error::other("the file changed after it was validated"));
        }
        fs::rename(&temp, &target)
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    #[cfg(unix)]
    if let Ok(directory) = fs::File::open(directory) {
        let _ = directory.sync_all();
    }
    Ok(file_identity(&target))
}

/// Re-verifies every file, then writes them in path order, restoring the
/// files already written if a later write fails.
fn commit(
    root: &Path,
    changed: &[(String, WorkingFile)],
    before_write: &mut dyn FnMut(usize, &Path) -> io::Result<()>,
) -> Result<(), EditError> {
    for (path, file) in changed {
        if reverify(root, path, file.identity, &file.hash_before).is_err() {
            return Err(EditError::Changed { path: path.clone() });
        }
    }
    let mut written = Vec::<(&str, &WorkingFile, Option<(u64, u64)>, String)>::new();
    for (index, (path, file)) in changed.iter().enumerate() {
        let edited = file.edited_bytes();
        let outcome = before_write(index, &root.join(path)).and_then(|()| {
            write_atomic(
                root,
                path,
                &edited,
                file.identity,
                &file.hash_before,
                &file.permissions,
            )
        });
        match outcome {
            Ok(identity) => written.push((
                path,
                file,
                identity,
                blake3::hash(&edited).to_hex().to_string(),
            )),
            Err(source) => {
                let mut restored = Vec::new();
                let mut unrestored = Vec::new();
                for (path, file, identity, hash) in written.into_iter().rev() {
                    match write_atomic(
                        root,
                        path,
                        &file.original,
                        identity,
                        &hash,
                        &file.permissions,
                    ) {
                        Ok(_) => restored.push(path.to_owned()),
                        Err(_) => unrestored.push(path.to_owned()),
                    }
                }
                restored.reverse();
                unrestored.reverse();
                return Err(EditError::WriteFailed {
                    path: path.clone(),
                    restored,
                    unrestored,
                    source,
                });
            }
        }
    }
    Ok(())
}

fn on_disk_hash(root: &Path, path: &str) -> Option<String> {
    let Ok(ResolvedEntry::File(resolved)) = resolve_under_root(root, path) else {
        return None;
    };
    let mut bytes = Vec::new();
    resolved
        .open()
        .ok()?
        .take(EDIT_MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(blake3::hash(&bytes).to_hex().to_string())
}

/// Appends a compact unified diff of `before` to `after`; returns `false`
/// when it would exceed [`EDIT_MAX_DIFF_BYTES`] (nothing is appended then).
fn append_diff(out: &mut String, path: &str, before: &str, after: &str) -> bool {
    let diff = unified_diff(path, before, after);
    if out.len() + diff.len() > EDIT_MAX_DIFF_BYTES {
        return false;
    }
    out.push_str(&diff);
    true
}

/// A changed region: `before[a.0..a.1]` became `after[b.0..b.1]`.
type Block = ((usize, usize), (usize, usize));

fn changed_blocks(before: &[&str], after: &[&str]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < before.len() && j < after.len() {
        if before[i] == after[j] {
            i += 1;
            j += 1;
            continue;
        }
        let mut resync = None;
        'search: for distance in 1..=2 * DIFF_RESYNC_WINDOW {
            for skip_before in 0..=distance {
                let skip_after = distance - skip_before;
                if skip_before > DIFF_RESYNC_WINDOW || skip_after > DIFF_RESYNC_WINDOW {
                    continue;
                }
                let (x, y) = (i + skip_before, j + skip_after);
                if x < before.len()
                    && y < after.len()
                    && before[x] == after[y]
                    && (x + 1 >= before.len()
                        || y + 1 >= after.len()
                        || before[x + 1] == after[y + 1])
                {
                    resync = Some((x, y));
                    break 'search;
                }
            }
        }
        let (x, y) = resync.unwrap_or((before.len(), after.len()));
        blocks.push(((i, x), (j, y)));
        i = x;
        j = y;
    }
    if i < before.len() || j < after.len() {
        blocks.push(((i, before.len()), (j, after.len())));
    }
    blocks
}

fn unified_diff(path: &str, before: &str, after: &str) -> String {
    let before_lines = before.split_inclusive('\n').collect::<Vec<_>>();
    let after_lines = after.split_inclusive('\n').collect::<Vec<_>>();
    let blocks = changed_blocks(&before_lines, &after_lines);
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    let clean = |line: &str| {
        line.trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_owned()
    };
    let mut index = 0;
    while index < blocks.len() {
        // Merge blocks whose context would overlap into one hunk.
        let mut last = index;
        while last + 1 < blocks.len()
            && blocks[last + 1].0.0 <= blocks[last].0.1 + 2 * DIFF_CONTEXT_LINES
        {
            last += 1;
        }
        let start_before = blocks[index].0.0.saturating_sub(DIFF_CONTEXT_LINES);
        let start_after = blocks[index].1.0.saturating_sub(DIFF_CONTEXT_LINES);
        let end_before = (blocks[last].0.1 + DIFF_CONTEXT_LINES).min(before_lines.len());
        let end_after = (blocks[last].1.1 + DIFF_CONTEXT_LINES).min(after_lines.len());
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            start_before + 1,
            end_before - start_before,
            start_after + 1,
            end_after - start_after
        ));
        let mut cursor = start_before;
        for block in &blocks[index..=last] {
            for line in &before_lines[cursor..block.0.0] {
                out.push_str(&format!(" {}\n", clean(line)));
            }
            for line in &before_lines[block.0.0..block.0.1] {
                out.push_str(&format!("-{}\n", clean(line)));
            }
            for line in &after_lines[block.1.0..block.1.1] {
                out.push_str(&format!("+{}\n", clean(line)));
            }
            cursor = block.0.1;
        }
        for line in &before_lines[cursor..end_before] {
            out.push_str(&format!(" {}\n", clean(line)));
        }
        index = last + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use compresso_core::{TokenProvider, counter_for};
    use tempfile::TempDir;

    use super::*;

    struct Fixture {
        _directory: TempDir,
        root: PathBuf,
        store: Store,
    }

    impl Fixture {
        fn new(files: &[(&str, &str)]) -> Self {
            let directory = TempDir::new().expect("temporary directory");
            let root = fs::canonicalize(directory.path())
                .expect("root")
                .join("repo");
            fs::create_dir_all(&root).expect("repo");
            for (path, contents) in files {
                let target = root.join(path);
                fs::create_dir_all(target.parent().expect("parent")).expect("parent dir");
                fs::write(target, contents).expect("file");
            }
            let store = Store::open(&directory.path().join("store.db")).expect("store");
            Indexer::new(&store, counter_for(TokenProvider::Heuristic))
                .index(&root, |_| {})
                .expect("index");
            Self {
                _directory: directory,
                root,
                store,
            }
        }

        fn apply(
            &self,
            operations: Vec<EditOperation>,
            hook: &mut dyn FnMut(usize, &Path) -> io::Result<()>,
        ) -> Result<EditReport, EditError> {
            apply_edits_with_hook(
                &self.store,
                &self.root,
                &EditRequest {
                    operations,
                    dry_run: false,
                    protected: Vec::new(),
                },
                counter_for(TokenProvider::Heuristic),
                hook,
            )
        }
    }

    fn replace(path: &str, old: &str, new: &str) -> EditOperation {
        EditOperation::Replace {
            path: path.to_owned(),
            old: old.to_owned(),
            new: new.to_owned(),
            expected_count: 1,
            expected_hash: None,
        }
    }

    #[test]
    fn test_identifier_boundaries_are_explicit() {
        let text = "format_utc(); format_utc_now(); reformat_utc(); x.format_utc2; café_format_utc; $format_utc";
        let (whole, substring) = identifier_occurrences(text, "format_utc");
        assert_eq!(whole.len(), 2, "call and `$format_utc`");
        assert_eq!(substring.len(), 4);
        assert!(is_identifier("naïve_ß"));
        assert!(!is_identifier("1abc"));
        assert!(!is_identifier("a-b"));
        assert!(!is_identifier(""));
    }

    #[test]
    fn test_a_failed_write_mid_batch_restores_the_files_already_written() {
        let fixture = Fixture::new(&[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
        let error = fixture
            .apply(
                vec![
                    replace("a.rs", "fn a", "fn aa"),
                    replace("b.rs", "fn b", "fn bb"),
                ],
                &mut |index, _| {
                    if index == 1 {
                        Err(io::Error::other("injected"))
                    } else {
                        Ok(())
                    }
                },
            )
            .expect_err("second write fails");
        match error {
            EditError::WriteFailed {
                path,
                restored,
                unrestored,
                ..
            } => {
                assert_eq!(path, "b.rs");
                assert_eq!(restored, ["a.rs"]);
                assert!(unrestored.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            fs::read_to_string(fixture.root.join("a.rs")).unwrap(),
            "fn a() {}\n"
        );
        assert_eq!(
            fs::read_to_string(fixture.root.join("b.rs")).unwrap(),
            "fn b() {}\n"
        );
        let leftovers = fs::read_dir(&fixture.root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0, "no temporary file is left behind");
    }

    #[test]
    fn test_a_file_changed_between_validation_and_write_is_not_written() {
        let fixture = Fixture::new(&[("a.rs", "fn a() {}\n")]);
        let error = fixture
            .apply(vec![replace("a.rs", "fn a", "fn aa")], &mut |_, path| {
                fs::write(path, "fn a() { changed }\n")
            })
            .expect_err("content changed");
        assert!(matches!(error, EditError::WriteFailed { .. }), "{error:?}");
        assert_eq!(
            fs::read_to_string(fixture.root.join("a.rs")).unwrap(),
            "fn a() { changed }\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_a_symlink_swapped_in_after_validation_is_never_written_through() {
        let fixture = Fixture::new(&[("src/a.rs", "fn a() {}\n")]);
        let outside = fixture.root.parent().unwrap().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("a.rs"), "fn a() {}\n").unwrap();
        let src = fixture.root.join("src");
        let error = fixture
            .apply(vec![replace("src/a.rs", "fn a", "fn aa")], &mut |_, _| {
                fs::rename(&src, fixture.root.join("src-moved"))?;
                std::os::unix::fs::symlink(&outside, &src)
            })
            .expect_err("swap is refused");
        assert!(matches!(error, EditError::WriteFailed { .. }), "{error:?}");
        assert_eq!(
            fs::read_to_string(outside.join("a.rs")).unwrap(),
            "fn a() {}\n"
        );
        assert_eq!(
            fs::read_dir(&outside).unwrap().count(),
            1,
            "no temporary file outside"
        );
        assert_eq!(
            fs::read_to_string(fixture.root.join("src-moved/a.rs")).unwrap(),
            "fn a() {}\n"
        );
    }

    #[test]
    fn test_diff_resyncs_after_each_change() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let after = "a\nB\nc\nd\ne\nf\ng\nh\nI\nj\n";
        let diff = unified_diff("x.txt", before, after);
        assert_eq!(
            diff,
            "--- a/x.txt\n+++ b/x.txt\n@@ -1,4 +1,4 @@\n a\n-b\n+B\n c\n d\n@@ -7,4 +7,4 @@\n g\n h\n-i\n+I\n j\n"
        );
        let inserted = unified_diff("x.txt", "a\nb\n", "a\nnew\nb\n");
        assert_eq!(
            inserted,
            "--- a/x.txt\n+++ b/x.txt\n@@ -1,2 +1,3 @@\n a\n+new\n b\n"
        );
    }
}
