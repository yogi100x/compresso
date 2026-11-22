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
//! * `create` (E17-47, D112): a new file; refused when anything exists at
//!   the path. Missing parent directories are created (E17-49, D114).
//!
//! One call applies up to [`EDIT_MAX_OPERATIONS`] operations carrying at
//! most [`EDIT_MAX_REQUEST_BYTES`] of text in order, all or nothing: every
//! operation is validated and applied in memory first, and no file is
//! written unless all of them succeed. A `replace` whose `old` and `new` are
//! identical still checks its count and is reported `unchanged`. No regular
//! expression is ever evaluated, no file is deleted or moved, no shell
//! command runs, and no network request is made.
//!
//! # Identifier boundaries
//!
//! An identifier character is `_` or any Unicode alphanumeric character
//! (`char::is_alphanumeric`: letters and numbers in any script). `$` is not an
//! identifier character, so `$name` in a shell script or macro is the
//! identifier `name`. A rename matches `old` only where the character before
//! the match (if any) and the character after it (if any) are not identifier
//! characters. Both names must consist of identifier characters only and must
//! not start with a number. A match is not renamed, and is reported as
//! adjacent, when it is an escape (`\n` after an odd run of backslashes, for a
//! one-character or `x`/`u` hexadecimal name), a string literal prefix before
//! a quote or `#` (`f"`, `b'`, `r#`), a printf conversion (`%d`), or, in Rust,
//! a lifetime (`'a`). In JavaScript, TypeScript, and Java `$name` is a
//! different identifier and is not renamed; in Scala and Dart `$name` may be
//! an identifier or string interpolation, so it is adjacent; elsewhere
//! (shell, PHP, Perl, Kotlin templates, ...) `$name` and `${name}` are the
//! variable `name` and are renamed. A C# verbatim identifier `@name` is the
//! identifier `name` and is renamed. Otherwise matching is textual:
//! occurrences inside comments and strings are renamed too, and the report
//! lists every remaining substring and adjacent occurrence.
//!
//! # Gates
//!
//! A target must be an indexed file, not a secret path, not excluded by the
//! scanner (for example gitignored), reached without any symbolic-link
//! component (the [`resolve_under_root`] gate shared with every read), inside
//! the repository root, outside `.git` and any protected directory (the
//! Compresso store), valid UTF-8, and no larger than [`EDIT_MAX_FILE_BYTES`]
//! before and after the edit. A `replace` or `insert` target that exists on
//! disk but is not indexed (created since the last index) makes the call
//! refresh the index once first, as a rename always does; the target is then
//! edited only if the scanner indexed it, so it passed every check an
//! indexed file passed.
//!
//! A `create` target is judged by the same path rules before it exists: not
//! a secret path, not `.git`, not a control file, not excluded by the
//! scanner (judged on both the requested and the canonical spelling of its
//! directory), not protected, every existing directory component a real
//! directory (no symbolic link) inside the root, the deepest one writable,
//! and nothing at the path. Its content may not contain NUL, so the scanner
//! indexes it.
//!
//! Missing directories on the path (E17-49, D114) are created, at most
//! [`EDIT_MAX_NEW_DIRECTORY_DEPTH`] per create and
//! [`EDIT_MAX_NEW_DIRECTORIES`] per call. Each passes the rules above as a
//! directory (a refusal names it): no hidden, reserved, `.git`, control, or
//! scanner-excluded component, since a file there would not be indexed, and
//! no name that differs only by case from an entry beside it or from a
//! directory or file another create of the call adds. A symbolic link on
//! the path is refused `no_directory` before anything follows it, whatever
//! it points to. A dry run creates nothing and reports the directories it
//! would create. Directories are made outermost first, each with a
//! create-new call (something that appeared meanwhile stops the call) and
//! the platform's default permissions (0o777 under the umask on Unix); if
//! the call then fails, the directories it made are removed deepest first,
//! each only while still empty and still the directory made.
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
//! A created file is staged the same way and then hard-linked into place,
//! which fails if anything appeared at the path, so it never replaces a file;
//! a rollback removes it only while it still holds exactly the created bytes.
//!
//! A residual window remains between the last verification and the rename
//! itself: the standard library offers no directory-handle-relative rename, so
//! a directory component swapped in that instant is not detected.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use compresso_core::TokenCounter;
use compresso_store::{FileRecord, Store, StoreError};

use crate::AtlasError;
use crate::grep::PathFilter;
use crate::indexer::Indexer;
use crate::queries::{QueryError, ResolvedEntry, canonical_query_root, resolve_under_root};
use crate::scanner::{DEFAULT_MAX_FILE_BYTES, ScannerExclusions, is_secret_path};

/// Most operations one call applies (E17-47, D112; 16 before).
pub const EDIT_MAX_OPERATIONS: usize = 64;
/// Most bytes of path and text (`path`, `old`, `new`, `anchor`, `text`,
/// `content`) one call carries, across its operations: the size of the
/// operations document `compresso edit --operations` reads.
pub const EDIT_MAX_REQUEST_BYTES: u64 = 4 * 1024 * 1024;
/// Largest `old`, `new`, anchor, inserted text, or created content, in bytes.
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
/// Most directory levels one `create` adds below its deepest existing
/// directory (E17-49, D114).
pub const EDIT_MAX_NEW_DIRECTORY_DEPTH: usize = 8;
/// Most directories one call creates, across its operations (E17-49, D114).
pub const EDIT_MAX_NEW_DIRECTORIES: usize = 32;
/// Most entries of an existing directory read to find a name that differs
/// only by case from a directory a `create` adds there (E17-49, D114).
pub const EDIT_MAX_SIBLING_ENTRIES: usize = 10_000;
const LOCATION_TEXT_MAX_BYTES: usize = 160;
const MAX_PATH_BYTES: usize = 4_096;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const FOREIGN_OWNER: &str = "the file belongs to another user";

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
        /// Rename even though `new` already occurs as an identifier in scope.
        allow_existing: bool,
    },
    /// Inserts `text` before or after `anchor`, which must occur exactly once.
    Insert {
        path: String,
        anchor: String,
        text: String,
        position: InsertPosition,
        expected_hash: Option<String>,
    },
    /// Creates `path` holding `content` (E17-47, D112), and its missing
    /// parent directories (E17-49, D114). Refused when anything exists at
    /// the path.
    Create { path: String, content: String },
}

impl EditOperation {
    /// The operator-facing operation name.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Replace { .. } => "replace",
            Self::Rename { .. } => "rename",
            Self::Insert { .. } => "insert",
            Self::Create { .. } => "create",
        }
    }

    /// Bytes of path and text the operation carries (see
    /// [`EDIT_MAX_REQUEST_BYTES`]).
    fn request_bytes(&self) -> u64 {
        let parts: [&str; 3] = match self {
            Self::Replace { path, old, new, .. } => [path, old, new],
            Self::Rename { old, new, path, .. } => [old, new, path.as_deref().unwrap_or("")],
            Self::Insert {
                path, anchor, text, ..
            } => [path, anchor, text],
            Self::Create { path, content } => [path, content, ""],
        };
        parts.iter().map(|part| part.len() as u64).sum()
    }
}

/// One edit call.
#[derive(Clone, Debug, Default)]
pub struct EditRequest {
    pub operations: Vec<EditOperation>,
    /// Report and diff only; write no repository file. The index is still
    /// refreshed first when the call renames (D97 (f), as on main) or names
    /// a target that exists but is not indexed (E17-47), so a dry run and a
    /// real run judge the same files; that writes only Compresso's store.
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
    /// Lines of the sites in the edited file, ascending.
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
    /// The call created the file (E17-47); `bytes_before` is 0 and
    /// `hash_before` is empty.
    pub created: bool,
}

/// Why a file in a rename scope was not searched.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    InvalidPath,
    /// Missing, a secret path, or reached through a symbolic link: reported
    /// alike, as every read tool reports them.
    NotFound,
    Excluded,
    NotIndexed,
    NotAFile,
    TooLarge,
    NotUtf8,
    Protected,
    /// An ignore file, or agent, editor, CI, or git-hook configuration.
    ControlFile,
    /// The file has no write permission for this user.
    ReadOnly,
    /// The file belongs to another user; replacing it would change its owner.
    ForeignOwner,
    /// `create`: something already exists at the path.
    Exists,
    /// `create`: a component of the path is not a directory or is a
    /// symbolic link (a missing one is created, E17-49).
    NoDirectory,
    /// `create`: a path component starts with `.` (E17-47 review).
    Hidden,
    /// `create`: a component holds `:` or is a Windows reserved device name.
    ReservedName,
    /// `create` (E17-49): a directory it would add differs only by case from
    /// an existing entry beside it or from a path another create of the call
    /// adds.
    CaseDuplicate,
}

impl Refusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidPath => "invalid_path",
            Self::NotFound => "not_found",
            Self::Excluded => "excluded",
            Self::NotIndexed => "not_indexed",
            Self::NotAFile => "not_a_file",
            Self::TooLarge => "too_large",
            Self::NotUtf8 => "not_utf8",
            Self::Protected => "protected",
            Self::ControlFile => "control_file",
            Self::ReadOnly => "read_only",
            Self::ForeignOwner => "foreign_owner",
            Self::Exists => "exists",
            Self::NoDirectory => "no_directory",
            Self::Hidden => "hidden",
            Self::ReservedName => "reserved_name",
            Self::CaseDuplicate => "case_duplicate",
        }
    }

    const fn describe(self) -> &'static str {
        match self {
            Self::InvalidPath => "the path must be repository-relative with forward slashes",
            Self::NotFound => "the file was not found",
            Self::Excluded => "the scanner excludes the path (for example it is gitignored)",
            Self::NotIndexed => {
                "the file is not in the repository index (the scanner skips binary files, text that is not UTF-8, and files over its size limit)"
            }
            Self::NotAFile => "the path is not a regular file",
            Self::TooLarge => "the file is over the 1 MB edit limit",
            Self::NotUtf8 => "the file is not valid UTF-8",
            Self::Protected => "the path is inside .git or the Compresso store",
            Self::ControlFile => {
                "the path is an ignore file or agent, editor, CI, or git-hook configuration"
            }
            Self::ReadOnly => "the file is not writable by this user",
            Self::ForeignOwner => "the file belongs to another user",
            Self::Exists => {
                "something already exists at the path; create makes new files only (edit an existing file with replace or insert)"
            }
            Self::NoDirectory => {
                "a component of the path is not a directory or is a symbolic link; create makes missing directories but never follows a link"
            }
            Self::Hidden => {
                "create makes ordinary source files only, never a path with a component starting with '.', because such files often configure tools and a denylist always has gaps; write it through the client's own file tools, which ask for their own permission"
            }
            Self::ReservedName => {
                "a path component contains ':' or is a reserved device name (CON, PRN, AUX, NUL, COM1-9, LPT1-9, with or without an extension), which create refuses on every platform"
            }
            Self::CaseDuplicate => {
                "a directory create would add differs only by case from an entry beside it or from a path another create of this call adds; a case-insensitive file system cannot hold both"
            }
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
    /// Occurrences of `old` next to a string prefix, escape, or sigil, left
    /// unchanged: check them by hand.
    pub remaining_adjacent_total: usize,
    pub remaining_adjacent: Vec<EditLocation>,
    /// Whole-identifier occurrences of `new` that existed before the edit.
    pub new_name_preexisting: usize,
    /// For a scoped rename (E17-36, D101), whole-identifier occurrences of
    /// `old` in indexed files outside the scope, which the rename did not
    /// touch; always 0 for an unscoped rename.
    /// Counted after every operation of the call, so a match a later
    /// operation renamed is not reported.
    pub outside_whole_total: usize,
    pub outside_whole: Vec<EditLocation>,
    /// Indexed files outside the scope that were not searched: the call's
    /// read budget ran out, or the file could not be read as text (secret
    /// paths are never searched and not listed). Not a claim that the old
    /// name is there; `outside_whole_total` is then a lower bound.
    pub outside_unsearched_total: usize,
    pub outside_unsearched: Vec<String>,
    /// The rename left nothing behind in its scope or outside it: every file
    /// in scope was searched (`false` when a file holding the old name could
    /// not be searched or edited, see `files_skipped`), the index was
    /// refreshed, and no whole match was found outside a scope. Files outside
    /// the scope that were not searched do not make it `false`.
    pub complete: bool,
}

/// What one operation did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationOutcome {
    pub kind: &'static str,
    pub sites: usize,
    pub files: Vec<String>,
    pub rename: Option<RenameOutcome>,
    /// A `replace` whose `old` and `new` are identical (E17-47): its count
    /// was checked and nothing changed. `sites` is 0 and `matched` holds
    /// the count.
    pub unchanged: bool,
    /// Matches an `unchanged` replace found; 0 otherwise.
    pub matched: usize,
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
    /// The pre-rename index refresh succeeded (always true without a rename).
    pub index_refreshed: bool,
    /// Directories the call created (E17-49), repository-relative as
    /// requested, outermost first; for a dry run, the ones it would create.
    pub directories_created: Vec<String>,
}

/// A bound one call may not exceed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditBound {
    Files,
    Sites,
    WriteBytes,
    FileBytes,
    ScanBytes,
    /// Operations in one call (E17-47).
    Operations,
    /// Bytes of path and text one call carries (E17-47).
    RequestBytes,
    /// Directory levels one create adds (E17-49).
    DirectoryDepth,
    /// Directories one call creates (E17-49).
    Directories,
    /// Entries of the directory a create adds a directory to (E17-49).
    DirectoryEntries,
}

impl EditBound {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Sites => "sites",
            Self::WriteBytes => "write_bytes",
            Self::FileBytes => "file_bytes",
            Self::ScanBytes => "scan_bytes",
            Self::Operations => "operations",
            Self::RequestBytes => "request_bytes",
            Self::DirectoryDepth => "directory_depth",
            Self::Directories => "directories",
            Self::DirectoryEntries => "directory_entries",
        }
    }

    /// What the caller can do about it, for the bounds a smaller call fixes.
    const fn advice(self) -> &'static str {
        match self {
            Self::Operations
            | Self::RequestBytes
            | Self::Files
            | Self::Sites
            | Self::WriteBytes
            | Self::Directories => {
                "; split the operations into several calls (each call is all or nothing)"
            }
            Self::DirectoryDepth => {
                "; create the outer directories with an earlier call, then the rest"
            }
            Self::FileBytes | Self::ScanBytes | Self::DirectoryEntries => "",
        }
    }
}

fn operation_prefix(operation: Option<usize>) -> String {
    operation.map_or_else(String::new, |number| format!("operation {number}: "))
}

fn directory_note(directory: Option<&str>) -> String {
    directory.map_or_else(String::new, |directory| {
        format!(" (directory {directory:?})")
    })
}

fn earlier_note(edited_earlier: bool) -> &'static str {
    if edited_earlier {
        " in the text as an earlier operation of this call left it (operations apply in order)"
    } else {
        ""
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
    #[error("{}refused to edit {path:?}{}: {}", operation_prefix(*operation), directory_note(directory.as_deref()), reason.describe())]
    Refused {
        path: String,
        reason: Refusal,
        /// For a `create` (E17-49), the directory on the path the refusal
        /// is about, repository-relative, when a directory and not the file
        /// name is the cause.
        directory: Option<String>,
        /// The operation whose target this is, when one operation names it
        /// (E17-47); `None` for a file a rename sweep or the commit reached.
        operation: Option<usize>,
    },
    #[error(
        "operation {operation} expected {expected} match(es) but found {actual}{}; nothing was written",
        earlier_note(*edited_earlier)
    )]
    CountMismatch {
        operation: usize,
        expected: usize,
        actual: usize,
        locations: Vec<EditLocation>,
        /// An earlier operation of the call already changed this file, so
        /// the count was taken in its edited text (E17-47).
        edited_earlier: bool,
    },
    #[error(
        "operation {operation}: the rename found no whole-identifier match; nothing was written"
    )]
    NoMatches {
        operation: usize,
        substring_total: usize,
        substring: Vec<EditLocation>,
    },
    #[error(
        "operation {operation}: '{name}' already occurs {count} time(s) as an identifier in scope; pass allow_existing to rename into it; nothing was written"
    )]
    NameExists {
        operation: usize,
        name: String,
        count: usize,
        locations: Vec<EditLocation>,
    },
    #[error("{path:?} does not have the expected content hash; nothing was written")]
    HashMismatch { path: String, actual: String },
    #[error("the edit exceeds the {} bound ({actual} > {limit}){}; nothing was written", bound.as_str(), bound.advice())]
    Bound {
        bound: EditBound,
        limit: u64,
        actual: u64,
    },
    #[error("{path:?} changed while the edit was prepared; nothing was written")]
    Changed { path: String },
    #[error("writing {path:?} failed; restored {} file(s), could not restore {}; original copies kept at {backups:?}", restored.len(), unrestored.len())]
    WriteFailed {
        path: String,
        /// Files written and then restored to their original bytes.
        restored: Vec<String>,
        /// Files left with the edited bytes because restoring them failed.
        unrestored: Vec<String>,
        /// Repository-relative copies of the originals of `unrestored` files,
        /// kept beside them.
        backups: Vec<String>,
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

impl EditError {
    /// Names operation `number` in a failure that one operation caused but
    /// that does not say which (E17-47): in a large batch the caller could
    /// not otherwise tell which operation to fix.
    fn at_operation(self, number: usize) -> Self {
        match self {
            Self::Invalid { reason } if !reason.starts_with("operation ") => Self::Invalid {
                reason: format!("operation {number}: {reason}"),
            },
            Self::Refused {
                path,
                reason,
                directory,
                operation: None,
            } => Self::Refused {
                path,
                reason,
                directory,
                operation: Some(number),
            },
            other => other,
        }
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

/// How a file's language treats `$` before a name, and whether `'name` is
/// a Rust lifetime. Chosen from the file extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceLanguage {
    /// `$` is an identifier character (JavaScript, TypeScript, Java):
    /// `$name` is a different identifier.
    DollarIdentifier,
    /// `$` is both an identifier character and string interpolation (Scala,
    /// Dart): `$name` may be either, so it is listed for a hand check.
    DollarAmbiguous,
    /// Rust: `'name` is a lifetime or a character literal.
    Rust,
    /// Shell, PHP, Perl, Kotlin (string templates), and everything else:
    /// `$name` and `${name}` are the variable `name`.
    Other,
}

impl SourceLanguage {
    /// The language of a repository path, by extension.
    pub fn for_path(path: &str) -> Self {
        let extension = path
            .rsplit('/')
            .next()
            .and_then(|name| name.rsplit_once('.'))
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .unwrap_or_default();
        match extension.as_str() {
            "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" | "java" => {
                Self::DollarIdentifier
            }
            "scala" | "sc" | "dart" => Self::DollarAmbiguous,
            "rs" => Self::Rust,
            _ => Self::Other,
        }
    }
}

/// Where a name occurs in a text.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Occurrences {
    /// Whole identifiers: renamed.
    pub whole: Vec<usize>,
    /// Inside a longer identifier: never renamed.
    pub substring: Vec<usize>,
    /// Next to a string prefix, an escape, or a sigil (`f"`, `\n`, `%d`,
    /// `'a`): never renamed, reported to be checked by hand.
    pub adjacent: Vec<usize>,
}

/// Names that are string or character literal prefixes in Python, Rust, C,
/// or C++ when a quote or `#` follows (compared case-insensitively: `f`, `r`,
/// `b`, `u`, `rb`, `br`, `fr`, `rf`, `c`, `cr`, `L`, `U`, `u8`, `R`).
const LITERAL_PREFIXES: &[&str] = &[
    "f", "r", "b", "u", "rb", "br", "fr", "rf", "c", "cr", "l", "u8",
];

fn is_literal_prefix(name: &str) -> bool {
    LITERAL_PREFIXES.contains(&name.to_ascii_lowercase().as_str())
}

/// Whether `\name` can be an escape sequence: one character (`\n`, `\t`,
/// `\0`, ...) or a hexadecimal or Unicode escape (`\x41`, `é`).
fn is_escape_form(name: &str) -> bool {
    let mut characters = name.chars();
    match (characters.next(), name.chars().count()) {
        (Some(_), 1) => true,
        (Some('x' | 'u' | 'U'), _) => characters.all(|character| character.is_ascii_hexdigit()),
        _ => false,
    }
}

/// Byte offsets of `name` in `text`, classified as whole identifiers,
/// substrings of longer identifiers, or occurrences adjacent to a string
/// prefix, escape, or sigil (see the module docs).
pub fn identifier_occurrences(text: &str, name: &str, language: SourceLanguage) -> Occurrences {
    let mut found = Occurrences::default();
    if name.is_empty() {
        return found;
    }
    let single = name.chars().count() == 1;
    for (start, _) in text.match_indices(name) {
        let end = start + name.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        let dollar_identifier = language == SourceLanguage::DollarIdentifier && before == Some('$');
        if before.is_some_and(is_identifier_char)
            || after.is_some_and(is_identifier_char)
            || dollar_identifier
        {
            found.substring.push(start);
            continue;
        }
        let backslashes = text[..start]
            .bytes()
            .rev()
            .take_while(|byte| *byte == b'\\')
            .count();
        let escape = backslashes % 2 == 1 && is_escape_form(name);
        let prefix = matches!(after, Some('"' | '\'' | '#')) && is_literal_prefix(name);
        let conversion = before == Some('%') && single;
        let lifetime = language == SourceLanguage::Rust && before == Some('\'');
        let interpolation = language == SourceLanguage::DollarAmbiguous && before == Some('$');
        // `\$name` is an escaped dollar: literal text, not the variable.
        let escaped_dollar = before == Some('$')
            && text[..start - 1]
                .bytes()
                .rev()
                .take_while(|byte| *byte == b'\\')
                .count()
                % 2
                == 1;
        if escape || prefix || conversion || lifetime || interpolation || escaped_dollar {
            found.adjacent.push(start);
        } else {
            found.whole.push(start);
        }
    }
    found
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
    let load_indexed = || -> Result<BTreeMap<String, FileRecord>, EditError> {
        Ok(store
            .indexed_files(repository.id)?
            .into_iter()
            .map(|record| (record.path.clone(), record))
            .collect())
    };
    let mut indexed = load_indexed()?;
    // A rename's scope is the indexed file list, so files created or changed
    // since the last index must be indexed first or they would be missed.
    // A replace or insert target that exists on disk but is not indexed
    // (created since the last index, E17-47) needs the same refresh: after
    // it the target is edited only if the scanner indexed it. This writes
    // only the index (also on a dry run), never a repository file.
    let has_rename = request
        .operations
        .iter()
        .any(|operation| matches!(operation, EditOperation::Rename { .. }));
    let exclusions = ScannerExclusions::load(&root);
    let candidates = refresh_candidates(&root, &exclusions, &request.operations, &indexed);
    // At most one refresh per call.
    let refresh = has_rename || !candidates.is_empty();
    #[cfg(test)]
    if refresh {
        tests::REFRESHES.with(|count| count.set(count.get() + 1));
    }
    let index_refreshed = !refresh || Indexer::new(store, counter).index(&root, |_| {}).is_ok();
    if refresh {
        indexed = load_indexed()?;
        // Also after a failed refresh (review): an unchanged target does
        // not repeat it on every call.
        remember_unindexed(&root, &candidates, &indexed);
    }
    let gate = Gate {
        root: &root,
        exclusions: &exclusions,
        indexed: &indexed,
        protected: &request.protected,
        index_refreshed,
    };

    let mut working = BTreeMap::<String, WorkingFile>::new();
    // Directories the call's creates add (E17-49).
    let mut new_directories = BTreeSet::<String>::new();
    // One read budget for every rename in the call.
    let mut scanned = 0_u64;
    let mut outcomes = Vec::with_capacity(request.operations.len());
    let mut total_sites = 0_usize;
    // Bounded by EDIT_MAX_OPERATIONS (validated above).
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
                let file =
                    load_into(&gate, &mut working, path).map_err(|e| e.at_operation(number))?;
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
                let file =
                    load_into(&gate, &mut working, path).map_err(|e| e.at_operation(number))?;
                check_hash(file, path, expected_hash.as_deref())?;
                apply_insert(file, path, anchor, text, *position, number)?
            }
            EditOperation::Create { path, content } => {
                if working.contains_key(path) {
                    return Err(refused(path, Refusal::Exists).at_operation(number));
                }
                let file = create_working(&gate, path, content)
                    .map_err(|error| error.at_operation(number))?;
                if let Some(new_file) = &file.created {
                    new_directories.extend(
                        new_file
                            .new_directories
                            .iter()
                            .map(|directory| directory.relative.clone()),
                    );
                    bound(
                        EditBound::Directories,
                        EDIT_MAX_NEW_DIRECTORIES as u64,
                        new_directories.len() as u64,
                    )?;
                }
                working.insert(path.clone(), file);
                OperationOutcome {
                    kind: "create",
                    sites: 0,
                    files: vec![path.clone()],
                    rename: None,
                    unchanged: false,
                    matched: 0,
                }
            }
            EditOperation::Rename {
                old,
                new,
                path,
                expected_count,
                allow_existing,
            } => apply_rename(
                &gate,
                &mut working,
                &mut scanned,
                RenameSpec {
                    old,
                    new,
                    filter: path.as_deref(),
                    expected: *expected_count,
                    allow_existing: *allow_existing,
                },
                number,
            )?,
        };
        total_sites = total_sites.saturating_add(outcome.sites);
        bound(EditBound::Sites, EDIT_MAX_SITES as u64, total_sites as u64)?;
        outcomes.push(outcome);
    }
    // A scoped rename's outside count (E17-36, D101) is taken against the
    // call's final working set, so a later operation's renames count.
    for (outcome, operation) in outcomes.iter_mut().zip(&request.operations) {
        let (
            Some(rename),
            EditOperation::Rename {
                old,
                path: Some(scope),
                ..
            },
        ) = (outcome.rename.as_mut(), operation)
        else {
            continue;
        };
        let Ok(Some(filter)) = PathFilter::parse(scope) else {
            continue;
        };
        let outside = outside_matches(&gate, &working, &filter, old, &mut scanned);
        rename.complete &= outside.total == 0;
        rename.outside_whole_total = outside.total;
        rename.outside_whole = outside.locations;
        rename.outside_unsearched_total = outside.unsearched_total;
        rename.outside_unsearched = outside.unsearched;
    }

    // A created file is written even when empty.
    let changed = working
        .into_iter()
        .filter(|(_, file)| file.created.is_some() || file.edited_bytes() != file.original)
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
    // Only the files that will change are asked, and in a dry run too, so
    // both runs refuse alike. A created file's directory was asked when the
    // operation ran.
    for (path, file) in &changed {
        if file.created.is_none()
            && let Some(reason) = os_write_refusal(&root.join(path))
        {
            return Err(refused(path, reason));
        }
    }

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

    let directories_created = planned_directories(&changed)
        .into_iter()
        .map(|directory| directory.relative.clone())
        .collect::<Vec<_>>();
    if request.dry_run {
        let mut diff = String::new();
        let mut diff_truncated = false;
        for (path, file) in &changed {
            let original = String::from_utf8_lossy(&file.original);
            let edited = String::from_utf8_lossy(&file.edited_bytes()).into_owned();
            if !append_diff(&mut diff, path, &original, &edited, file.created.is_some()) {
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
            index_refreshed,
            directories_created,
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
            index_refreshed,
            directories_created: Vec::new(),
        });
    }

    let directories_created = commit(&root, &changed, before_write)?;
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
        index_refreshed,
        directories_created,
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
    if request.operations.is_empty() {
        return Err(invalid(format!(
            "a call carries 1 to {EDIT_MAX_OPERATIONS} operations"
        )));
    }
    bound(
        EditBound::Operations,
        EDIT_MAX_OPERATIONS as u64,
        request.operations.len() as u64,
    )?;
    let request_bytes = request
        .operations
        .iter()
        .map(EditOperation::request_bytes)
        .fold(0_u64, u64::saturating_add);
    bound(
        EditBound::RequestBytes,
        EDIT_MAX_REQUEST_BYTES,
        request_bytes,
    )?;
    // Two creates whose paths differ only by case would be one file on a
    // case-insensitive file system; refused alike everywhere, dry runs too.
    let mut created = BTreeMap::<String, (usize, &str)>::new();
    // E17-49: the directories the creates name, by lowercased spelling.
    let mut directories = BTreeMap::<String, &str>::new();
    for (index, operation) in request.operations.iter().enumerate() {
        validate_operation(operation).map_err(|error| error.at_operation(index + 1))?;
        let EditOperation::Create { path, .. } = operation else {
            continue;
        };
        if let Some((earlier, earlier_path)) =
            created.insert(path.to_lowercase(), (index + 1, path))
            && earlier_path != path
        {
            return Err(invalid(format!(
                "operation {}: create path '{path}' differs only by case from operation {earlier}'s '{earlier_path}'; a case-insensitive file system cannot hold both",
                index + 1
            )));
        }
        // A directory spelled two ways, or a file where another create
        // needs a directory, is refused alike everywhere, dry runs too.
        let clash = |directory: &str, other: &str| {
            Err(EditError::Refused {
                path: path.clone(),
                reason: if directory == other {
                    Refusal::Exists
                } else {
                    Refusal::CaseDuplicate
                },
                directory: Some(directory.to_owned()),
                operation: Some(index + 1),
            })
        };
        if let Some(other) = directories.get(&path.to_lowercase()) {
            return clash(other, path);
        }
        let mut end = 0;
        while let Some(slash) = path[end..].find('/') {
            end += slash;
            let directory = &path[..end];
            let key = directory.to_lowercase();
            if let Some((_, file)) = created.get(&key) {
                return clash(directory, file);
            }
            match directories.get(&key) {
                Some(other) if *other != directory => return clash(directory, other),
                Some(_) => {}
                None => {
                    directories.insert(key, directory);
                }
            }
            end += 1;
        }
    }
    Ok(())
}

fn validate_operation(operation: &EditOperation) -> Result<(), EditError> {
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
            // Identical texts are a reported no-op (E17-47), not an error.
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
            ..
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
        EditOperation::Create { path, content } => {
            validate_path(path)?;
            // Empty is allowed (an empty module file); NUL is not, so the
            // scanner never sniffs the new file as binary.
            text("content", content, true)?;
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
        Err(refused(path, Refusal::InvalidPath))
    }
}

/// The checks every target passes before it is read.
struct Gate<'a> {
    root: &'a Path,
    /// The scanner's exclusion rules, loaded once for the whole call.
    exclusions: &'a ScannerExclusions,
    indexed: &'a BTreeMap<String, FileRecord>,
    protected: &'a [PathBuf],
    /// The index was refreshed before a rename read its scope from it.
    index_refreshed: bool,
}

/// A file read for editing, with its edited text.
struct WorkingFile {
    original: Vec<u8>,
    hash_before: String,
    identity: Option<(u64, u64)>,
    /// The original file's permissions, kept by the replacement; `None` for
    /// a created file, which gets the default mode under the umask.
    permissions: Option<fs::Permissions>,
    bom: bool,
    line_ending: LineEnding,
    stale: bool,
    /// Edited text without the byte order mark.
    text: String,
    sites: usize,
    /// Byte offsets of every site in `text`, kept current across splices.
    site_offsets: Vec<usize>,
    /// The file's owner (Unix), so a replacement never changes it.
    owner: Option<u32>,
    /// How identifiers are matched in this file.
    language: SourceLanguage,
    /// Where a file this call creates goes; `None` for an existing file.
    created: Option<NewFile>,
}

/// The place a `create` operation writes: a path with nothing at it below
/// a real directory of the repository, and the directories it adds there.
#[derive(Clone, Debug)]
struct NewFile {
    /// The absolute target, under the canonical directory.
    target: PathBuf,
    /// The deepest existing directory on the path, canonical: the file's
    /// directory, or the one the first new directory is created in.
    anchor: PathBuf,
    /// Device and inode of `anchor` (Unix), re-checked before writing.
    directory_identity: Option<(u64, u64)>,
    /// Directories that do not exist yet, outermost first (E17-49).
    new_directories: Vec<NewDirectory>,
}

/// A directory a `create` adds (E17-49).
#[derive(Clone, Debug, Eq, PartialEq)]
struct NewDirectory {
    /// Repository-relative, as requested.
    relative: String,
    /// Absolute, below the canonical existing directory.
    absolute: PathBuf,
}

/// A directory this call created, with its identity (Unix) when created.
struct MadeDirectory {
    relative: String,
    absolute: PathBuf,
    identity: Option<(u64, u64)>,
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
            lines: {
                let lines = Lines::new(&self.text);
                let mut offsets = self.site_offsets.clone();
                offsets.sort_unstable();
                offsets.iter().map(|offset| lines.line(*offset)).collect()
            },
            stale: self.stale,
            line_ending: self.line_ending,
            bom: self.bom,
            bytes_before: self.original.len() as u64,
            bytes_after: edited.len() as u64,
            hash_before: self.hash_before.clone(),
            hash_after: blake3::hash(&edited).to_hex().to_string(),
            verified: false,
            created: self.created.is_some(),
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
        directory: None,
        operation: None,
    }
}

fn load(gate: &Gate<'_>, path: &str) -> Result<WorkingFile, EditError> {
    validate_path(path)?;
    let relative = Path::new(path);
    // A secret path reads as missing, exactly as every read tool reports it.
    if is_secret_path(relative) {
        return Err(refused(path, Refusal::NotFound));
    }
    if relative
        .components()
        .any(|component| component.as_os_str() == ".git")
    {
        return Err(refused(path, Refusal::Protected));
    }
    if is_control_path(relative) {
        return Err(refused(path, Refusal::ControlFile));
    }
    if gate.exclusions.excludes(gate.root, relative) {
        return Err(refused(path, Refusal::Excluded));
    }
    // Judged without following any link first (E17-47 review): a path
    // through a link, or a missing one, reads as missing whatever the link
    // points to, so an answer never tells whether something outside the
    // root exists. The read gate below still re-checks.
    match leaf_state(gate.root, path) {
        LeafState::Link | LeafState::Missing => return Err(refused(path, Refusal::NotFound)),
        LeafState::File(_) | LeafState::Other => {}
    }
    let resolved = match resolve_under_root(gate.root, path) {
        Ok(ResolvedEntry::File(resolved)) => resolved,
        // Like the read gate, a path through a symbolic link reads as missing.
        Ok(ResolvedEntry::Missing) => return Err(refused(path, Refusal::NotFound)),
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
    let Some(record) = gate.indexed.get(path) else {
        return Err(refused(path, Refusal::NotIndexed));
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
    // Decided from the open handle's metadata alone: nothing is opened for
    // writing, so a rename scope or a dry run never touches a file.
    if let Some(reason) = write_refusal(&metadata) {
        return Err(refused(path, reason));
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
        permissions: Some(metadata.permissions()),
        bom,
        stale,
        text,
        sites: 0,
        site_offsets: Vec::new(),
        owner: file_owner(&metadata),
        language: SourceLanguage::for_path(path),
        created: None,
    })
}

/// Most targets the unindexed-target memory holds per process; it is
/// cleared when full, so it never grows past this.
pub const EDIT_UNINDEXED_MEMO_ENTRIES: usize = 1_024;

/// Size and modification time of a target that existed on disk but that an
/// index refresh did not index, keyed by canonical root and path.
type UnindexedMemo = BTreeMap<(PathBuf, String), (u64, Option<std::time::SystemTime>)>;

/// Targets a refresh did not index, remembered for this process (E17-47
/// review): an unchanged one does not cost another refresh.
fn unindexed_memo() -> &'static std::sync::Mutex<UnindexedMemo> {
    static MEMO: std::sync::OnceLock<std::sync::Mutex<UnindexedMemo>> = std::sync::OnceLock::new();
    MEMO.get_or_init(|| std::sync::Mutex::new(BTreeMap::new()))
}

/// A `replace` or `insert` target that may be indexed by a refresh, with
/// the size and modification time it had when judged.
struct RefreshCandidate {
    path: String,
    size: u64,
    modified: Option<std::time::SystemTime>,
}

/// The `replace` and `insert` targets that exist on disk but are not
/// indexed, that no earlier `create` of the call makes, and that a refresh
/// could index: the call then refreshes the index once first (E17-47).
/// Every cheap check that would make the scanner or the edit gates refuse
/// the target is applied first, so such a target never costs a refresh:
/// secret, `.git`, control file, scanner-excluded, any symbolic link on the
/// path (judged without following links), not a regular file, over the
/// edit limit, binary (a NUL in the first 8 KiB, the scanner's sniff) or
/// not UTF-8 in those 8 KiB, or
/// remembered unchanged from an earlier refresh that did not index it.
/// Bounded by the operation count; reads at most 8 KiB per target.
fn refresh_candidates(
    root: &Path,
    exclusions: &ScannerExclusions,
    operations: &[EditOperation],
    indexed: &BTreeMap<String, FileRecord>,
) -> Vec<RefreshCandidate> {
    let mut created = BTreeSet::new();
    let mut candidates = Vec::new();
    for operation in operations {
        let path = match operation {
            EditOperation::Create { path, .. } => {
                created.insert(path.as_str());
                continue;
            }
            EditOperation::Replace { path, .. } | EditOperation::Insert { path, .. } => path,
            EditOperation::Rename { .. } => continue,
        };
        let relative = Path::new(path);
        if indexed.contains_key(path)
            || created.contains(path.as_str())
            || candidates
                .iter()
                .any(|candidate: &RefreshCandidate| &candidate.path == path)
            || validate_path(path).is_err()
            || is_secret_path(relative)
            || has_git_component(relative)
            || is_control_path(relative)
            || exclusions.excludes(root, relative)
        {
            continue;
        }
        let LeafState::File(metadata) = leaf_state(root, path) else {
            continue;
        };
        if metadata.len() > EDIT_MAX_FILE_BYTES {
            continue;
        }
        let modified = metadata.modified().ok();
        let remembered = unindexed_memo()
            .lock()
            .ok()
            .and_then(|memo| memo.get(&(root.to_path_buf(), path.clone())).copied());
        if remembered == Some((metadata.len(), modified)) {
            continue;
        }
        let binary = match resolve_under_root(root, path) {
            Ok(ResolvedEntry::File(resolved)) => {
                let mut prefix = Vec::new();
                resolved
                    .open()
                    .and_then(|file| file.take(BINARY_SNIFF_BYTES).read_to_end(&mut prefix))
                    .map_or(true, |_| {
                        // A NUL, or bytes that are not UTF-8 (beyond a
                        // character cut at the end of the sniff): the
                        // scanner skips the file either way.
                        prefix.contains(&0)
                            || std::str::from_utf8(&prefix)
                                .is_err_and(|error| error.error_len().is_some())
                    })
            }
            _ => true,
        };
        if binary {
            continue;
        }
        candidates.push(RefreshCandidate {
            path: path.clone(),
            size: metadata.len(),
            modified,
        });
    }
    candidates
}

/// Bytes the scanner reads to decide a file is binary.
const BINARY_SNIFF_BYTES: u64 = 8 * 1024;

/// Remembers the candidates a refresh did not index.
fn remember_unindexed(
    root: &Path,
    candidates: &[RefreshCandidate],
    indexed: &BTreeMap<String, FileRecord>,
) {
    let Ok(mut memo) = unindexed_memo().lock() else {
        return;
    };
    for candidate in candidates
        .iter()
        .filter(|candidate| !indexed.contains_key(&candidate.path))
    {
        if memo.len() >= EDIT_UNINDEXED_MEMO_ENTRIES {
            memo.clear();
        }
        memo.insert(
            (root.to_path_buf(), candidate.path.clone()),
            (candidate.size, candidate.modified),
        );
    }
}

/// What a repository path names, judged without following any link.
enum LeafState {
    File(fs::Metadata),
    /// A directory or another non-file type.
    Other,
    /// A component of the path, the leaf included, is a symbolic link.
    Link,
    /// A component is missing or is not a directory.
    Missing,
}

/// Walks `path` under `root` one component at a time with
/// `symlink_metadata`, so no link is followed and nothing outside the root
/// is consulted: a path through a link is `Link` whatever it points to.
/// Bounded by the path's components ([`MAX_PATH_BYTES`]).
fn leaf_state(root: &Path, path: &str) -> LeafState {
    let mut current = root.to_path_buf();
    let mut components = Path::new(path).components().peekable();
    while let Some(component) = components.next() {
        current.push(component);
        let Ok(metadata) = fs::symlink_metadata(&current) else {
            return LeafState::Missing;
        };
        if metadata.file_type().is_symlink() {
            return LeafState::Link;
        }
        if components.peek().is_none() {
            return if metadata.is_file() {
                LeafState::File(metadata)
            } else {
                LeafState::Other
            };
        }
        if !metadata.is_dir() {
            return LeafState::Missing;
        }
    }
    LeafState::Missing
}

/// Whether any component of `relative` is `.git`, in any case (a create on
/// a case-insensitive file system must not reach `.GIT/config`).
fn has_git_component(relative: &Path) -> bool {
    relative
        .components()
        .any(|component| component.as_os_str().eq_ignore_ascii_case(".git"))
}

/// Resolves where `create` would write `path`: every existing directory
/// component below the root must be a real directory (no symbolic link) and
/// the deepest one's canonical form must lie inside the root; the missing
/// components after it are the directories the create adds (E17-49).
/// Nothing may exist at the path. A link anywhere on the path is refused
/// `no_directory` before anything follows it, so the answer is the same
/// whatever it points to. Returns the target and the repository-relative
/// canonical form of the path. Bounded by the path's components
/// ([`MAX_PATH_BYTES`]).
fn resolve_new_file(root: &Path, path: &str) -> Result<(NewFile, PathBuf), EditError> {
    let relative = Path::new(path);
    let (Some(parent), Some(name)) = (relative.parent(), relative.file_name()) else {
        return Err(refused(path, Refusal::InvalidPath));
    };
    let mut directory = root.to_path_buf();
    // Components from the first missing one on.
    let mut missing = Vec::new();
    for component in parent.components() {
        if !missing.is_empty() {
            missing.push(component.as_os_str());
            continue;
        }
        let next = directory.join(component);
        match fs::symlink_metadata(&next) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                directory = next;
            }
            Ok(_) => return Err(refused(path, Refusal::NoDirectory)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(component.as_os_str());
            }
            Err(source) => {
                return Err(EditError::Read {
                    path: path.to_owned(),
                    source,
                });
            }
        }
    }
    let anchor = fs::canonicalize(&directory).map_err(|source| EditError::Read {
        path: path.to_owned(),
        source,
    })?;
    let Ok(anchor_relative) = anchor.strip_prefix(root) else {
        return Err(EditError::OutsideRoot {
            path: path.to_owned(),
        });
    };
    let mut canonical_relative = anchor_relative.to_path_buf();
    let mut target = anchor.clone();
    let parts = path.split('/').collect::<Vec<_>>();
    let existing = parts.len() - 1 - missing.len();
    let mut new_directories = Vec::with_capacity(missing.len());
    for (offset, component) in missing.iter().enumerate() {
        canonical_relative.push(component);
        target.push(component);
        new_directories.push(NewDirectory {
            relative: parts[..=existing + offset].join("/"),
            absolute: target.clone(),
        });
    }
    canonical_relative.push(name);
    target.push(name);
    if new_directories.is_empty() {
        match fs::symlink_metadata(&target) {
            // A dangling link, a directory, or a file in another case on a
            // case-insensitive file system all exist.
            Ok(_) => return Err(refused(path, Refusal::Exists)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(EditError::Read {
                    path: path.to_owned(),
                    source,
                });
            }
        }
    }
    Ok((
        NewFile {
            target,
            directory_identity: file_identity(&anchor),
            anchor,
            new_directories,
        },
        canonical_relative,
    ))
}

/// Checks a `create` of `path` against every path rule an indexed file
/// passes (see the module docs) and returns its working file. Nothing is
/// written.
fn create_working(gate: &Gate<'_>, path: &str, content: &str) -> Result<WorkingFile, EditError> {
    validate_path(path)?;
    let relative = Path::new(path);
    // The name rules first, judged on the requested spelling alone, so such
    // a refusal never depends on what exists on disk.
    judge_create_name(gate, path, relative)?;
    // Then the link-free walk, before anything that could follow a link,
    // so a linked ancestor is answered alike whatever it points to.
    let (new_file, canonical_relative) = resolve_new_file(gate.root, path)?;
    bound(
        EditBound::DirectoryDepth,
        EDIT_MAX_NEW_DIRECTORY_DEPTH as u64,
        new_file.new_directories.len() as u64,
    )?;
    judge_create_excluded(gate, path, relative)?;
    if let Some(first) = new_file.new_directories.first() {
        check_case_siblings(path, &new_file.anchor, first)?;
    }
    check_new_file(gate, path, content, new_file, &canonical_relative)
}

/// Refuses a new directory whose name differs only by case from an entry
/// already in `anchor`, the directory it is created in (E17-49): a
/// case-insensitive file system cannot hold both. Reads at most
/// [`EDIT_MAX_SIBLING_ENTRIES`] entries.
fn check_case_siblings(path: &str, anchor: &Path, first: &NewDirectory) -> Result<(), EditError> {
    let name = first
        .absolute
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let read = |source| EditError::Read {
        path: path.to_owned(),
        source,
    };
    for (count, entry) in fs::read_dir(anchor).map_err(read)?.enumerate() {
        if count >= EDIT_MAX_SIBLING_ENTRIES {
            return Err(EditError::Bound {
                bound: EditBound::DirectoryEntries,
                limit: EDIT_MAX_SIBLING_ENTRIES as u64,
                actual: count as u64 + 1,
            });
        }
        if entry
            .map_err(read)?
            .file_name()
            .to_string_lossy()
            .to_lowercase()
            == name
        {
            return Err(EditError::Refused {
                path: path.to_owned(),
                reason: Refusal::CaseDuplicate,
                directory: Some(first.relative.clone()),
                operation: None,
            });
        }
    }
    Ok(())
}

/// The path rules of `create`, judged on one spelling of the path. Create
/// is stricter than edit (E17-47 review): besides the shared denylist it
/// refuses any hidden component, `:`, and reserved device names.
fn judge_create_path(gate: &Gate<'_>, path: &str, relative: &Path) -> Result<(), EditError> {
    judge_create_name(gate, path, relative)?;
    judge_create_excluded(gate, path, relative)
}

/// The rules of [`judge_create_path`] that read the path's spelling only.
fn judge_create_name(gate: &Gate<'_>, path: &str, relative: &Path) -> Result<(), EditError> {
    let refuse = |reason| Err(refused_at(gate, path, relative, reason));
    if relative
        .components()
        .any(|component| is_reserved_component(&component.as_os_str().to_string_lossy()))
    {
        return refuse(Refusal::ReservedName);
    }
    if is_secret_path(relative) {
        return refuse(Refusal::Excluded);
    }
    if has_git_component(relative) {
        return refuse(Refusal::Protected);
    }
    if is_control_path(relative) {
        return refuse(Refusal::ControlFile);
    }
    if relative
        .components()
        .any(|component| component.as_os_str().to_string_lossy().starts_with('.'))
    {
        return refuse(Refusal::Hidden);
    }
    Ok(())
}

/// The scanner's exclusion rules of [`judge_create_path`]: a file the
/// scanner would skip is never created, nor a directory for one.
fn judge_create_excluded(gate: &Gate<'_>, path: &str, relative: &Path) -> Result<(), EditError> {
    if gate.exclusions.excludes(gate.root, relative) {
        return Err(refused_at(gate, path, relative, Refusal::Excluded));
    }
    Ok(())
}

/// A create refusal of `path`, naming the outermost directory of
/// `relative` the rule refuses on its own (E17-49), if a directory and not
/// the file name is the cause.
fn refused_at(gate: &Gate<'_>, path: &str, relative: &Path, reason: Refusal) -> EditError {
    let mut directory = None;
    let mut prefix = PathBuf::new();
    for component in relative.parent().into_iter().flat_map(Path::components) {
        prefix.push(component);
        let name = component.as_os_str().to_string_lossy();
        let refuses = match reason {
            Refusal::ReservedName => is_reserved_component(&name),
            Refusal::Excluded => {
                is_secret_path(&prefix) || gate.exclusions.excludes_directory(gate.root, &prefix)
            }
            Refusal::Protected => name.eq_ignore_ascii_case(".git"),
            Refusal::ControlFile => is_control_path(&prefix),
            Refusal::Hidden => name.starts_with('.'),
            _ => false,
        };
        if refuses {
            directory = Some(prefix.to_string_lossy().replace('\\', "/"));
            break;
        }
    }
    EditError::Refused {
        path: path.to_owned(),
        reason,
        directory,
        operation: None,
    }
}

/// Whether a path component holds `:` (an alternate data stream or drive
/// on Windows) or is a Windows reserved device name, with or without an
/// extension, compared as Windows does (case-insensitive, trailing dots and
/// spaces ignored).
fn is_reserved_component(name: &str) -> bool {
    if name.contains(':') {
        return true;
    }
    let key = name.trim_end_matches(['.', ' ']).to_ascii_uppercase();
    let stem = key
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    if matches!(stem, "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") {
        return true;
    }
    // COM1-9 and LPT1-9, and the superscript digits Windows also reserves.
    let Some(number) = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
    else {
        return false;
    };
    let mut characters = number.chars();
    matches!(
        (characters.next(), characters.next()),
        (Some('1'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None)
    )
}

/// The checks of `create` that follow the resolution of its directory,
/// given the directory's canonical spelling (separate so a test can judge a
/// spelling only a case-insensitive file system produces).
fn check_new_file(
    gate: &Gate<'_>,
    path: &str,
    content: &str,
    new_file: NewFile,
    canonical_relative: &Path,
) -> Result<WorkingFile, EditError> {
    // The canonical spelling of the directory too: on a case-insensitive
    // file system `Ignored/x` is inside the ignored directory `ignored/`.
    judge_create_path(gate, path, canonical_relative)?;
    if gate
        .protected
        .iter()
        .any(|protected| new_file.target.starts_with(protected))
    {
        return Err(refused(path, Refusal::Protected));
    }
    // The directory the file, or its first new directory, is created in.
    if let Some(reason) = directory_write_refusal(&new_file.anchor) {
        return Err(refused(path, reason));
    }
    let (bom, text) = match content.strip_prefix('\u{feff}') {
        Some(rest) => (true, rest.to_owned()),
        None => (false, content.to_owned()),
    };
    Ok(WorkingFile {
        line_ending: line_ending(&text),
        original: Vec::new(),
        hash_before: String::new(),
        identity: None,
        // A created file gets what any new file in the directory gets: the
        // default mode under the process umask, never executable.
        permissions: None,
        bom,
        stale: false,
        text,
        sites: 0,
        site_offsets: Vec::new(),
        owner: None,
        language: SourceLanguage::for_path(path),
        created: Some(new_file),
    })
}

/// Names (as any path component) of directories whose files configure
/// agents, editors, CI, development containers, or git hooks. Both lists are
/// matched at every component, case-insensitively.
const CONTROL_DIRECTORIES: &[&str] = &[
    ".claude",
    ".codex",
    ".cursor",
    ".vscode",
    ".idea",
    ".github",
    ".gitlab",
    ".husky",
    ".circleci",
    ".devcontainer",
    ".buildkite",
    ".woodpecker",
    ".windsurf",
    ".gemini",
    ".continue",
    ".zed",
    // E17-47 review: version-control metadata of other systems, a package
    // manager's executable releases and plugins, and agent configuration.
    ".hg",
    ".jj",
    ".svn",
    ".yarn",
    ".opencode",
];
/// File names (at any depth) that are ignore files, agent instructions, or
/// hook, CI, or environment-execution configuration. Compared
/// case-insensitively.
const CONTROL_FILES: &[&str] = &[
    ".ignore",
    ".rgignore",
    ".mcp.json",
    ".claude.json",
    "claude.md",
    "claude.local.md",
    "agents.md",
    "gemini.md",
    ".cursorrules",
    ".windsurfrules",
    ".clinerules",
    ".pre-commit-config.yaml",
    ".pre-commit-config.yml",
    ".travis.yml",
    "azure-pipelines.yml",
    "jenkinsfile",
    ".envrc",
    "lefthook.yml",
    "lefthook.yaml",
    ".lefthook.yml",
    ".lefthook.yaml",
    ".devcontainer.json",
    "codeowners",
    ".cursorignore",
    ".aiexclude",
    ".codeiumignore",
    ".geminiignore",
    ".npmrc",
    ".yarnrc.yml",
    // E17-47 review: files `create` could plant that tools load and run, and
    // their siblings (same tool, another file name).
    ".yarnrc",
    ".pnpmfile.cjs",
    ".pnpmfile.mjs",
    "mise.toml",
    ".mise.toml",
    "mise.local.toml",
    ".mise.local.toml",
    ".tool-versions",
    "rust-toolchain",
    "rust-toolchain.toml",
    ".exrc",
    ".vimrc",
    ".nvimrc",
    ".nvim.lua",
    "agent.md",
    ".rules",
    "opencode.json",
    "opencode.jsonc",
    ".drone.yml",
    ".drone.yaml",
    "bitbucket-pipelines.yml",
    "bitbucket-pipelines.yaml",
    "azure-pipelines.yaml",
    ".travis.yaml",
    // Final round of the review; the list stops here. It is defence in
    // depth, not a boundary (see `is_control_path`).
    "agents.override.md",
    "crush.md",
    "lefthook-local.yml",
    "lefthook-local.yaml",
    ".lefthook-local.yml",
    ".lefthook-local.yaml",
    "appveyor.yml",
    "appveyor.yaml",
    ".appveyor.yml",
    ".appveyor.yaml",
    "bunfig.toml",
];

/// A path component as case-insensitive file systems and Windows compare it:
/// ASCII-lowercased, without trailing dots and spaces.
fn control_key(name: &str) -> String {
    name.trim_end_matches(['.', ' ']).to_ascii_lowercase()
}

/// Whether `relative` is a known control file: one that decides what the
/// edit gates allow (ignore files) or how an agent, editor, CI system,
/// development container, or git hook behaves, at any depth. Every name
/// starting with `.git` (`.gitignore`, `.gitattributes`, `.gitmodules`,
/// `.gitlab-ci.yml`, ...) or `.aider` is included.
///
/// This is a denylist of known control files, defence in depth and not a
/// boundary: ordinary code and build files that tools run (`build.rs`,
/// `package.json`, `Makefile`, `conftest.py`, `eslint.config.js`,
/// `go.work`) stay editable and creatable on purpose, because editing code
/// is the tool's purpose; the client's approval of the edit tool is the
/// boundary. Names are compared after
/// ASCII case folding and trimming trailing dots and spaces; ASCII names
/// have no other Unicode-equivalent spelling, and a look-alike name using
/// other scripts names a different file, which clients do not load as the
/// control file.
pub fn is_control_path(relative: &Path) -> bool {
    let names = relative
        .components()
        .map(|component| control_key(&component.as_os_str().to_string_lossy()))
        .collect::<Vec<_>>();
    names.iter().enumerate().any(|(index, name)| {
        name.starts_with(".git")
            || name.starts_with(".aider")
            || name.ends_with(".code-workspace")
            || CONTROL_DIRECTORIES.contains(&name.as_str())
            || CONTROL_FILES.contains(&name.as_str())
            // `mise.<env>.toml` and `.mise.<env>.toml` configuration files.
            || ((name.starts_with("mise.") || name.starts_with(".mise."))
                && name.ends_with(".toml"))
            || (name == ".cargo"
                && names
                    .get(index + 1)
                    .is_some_and(|next| next == "config" || next == "config.toml"))
    })
}

#[cfg(unix)]
/// Why this process may not replace a file with `owner` and `mode`, as
/// effective user `euid`: another user's file would change owner, and a file
/// without the owner write bit is read-only for everyone, root included.
/// This is the cheap first check at load; [`os_write_refusal`] asks the OS
/// for the files that will actually change.
fn ownership_refusal(owner: u32, mode: u32, euid: u32) -> Option<Refusal> {
    if owner != euid {
        Some(Refusal::ForeignOwner)
    } else if mode & 0o200 == 0 {
        Some(Refusal::ReadOnly)
    } else {
        None
    }
}

/// Asks the OS whether this process may replace `path`, opening nothing:
/// `faccessat` with `AT_EACCESS` for `W_OK` on the file and `W_OK | X_OK` on
/// its directory (the replacement is created and renamed there), so
/// access-control lists, read-only mounts, immutable flags, and unwritable
/// directories refuse the edit up front. A path that vanished or became a
/// link reads as not found.
#[cfg(unix)]
fn os_access_refusal(path: &Path, mode: libc::c_int) -> Option<Refusal> {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return Some(Refusal::ReadOnly);
    };
    // SAFETY: `path` is a valid NUL-terminated string for the whole call
    // and faccessat only reads it.
    let status = unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), mode, libc::AT_EACCESS) };
    if status == 0 {
        return None;
    }
    Some(match io::Error::last_os_error().raw_os_error() {
        Some(libc::ENOENT | libc::ELOOP | libc::ENOTDIR) => Refusal::NotFound,
        _ => Refusal::ReadOnly,
    })
}

#[cfg(unix)]
fn os_write_refusal(path: &Path) -> Option<Refusal> {
    os_access_refusal(path, libc::W_OK).or_else(|| path.parent().and_then(directory_write_refusal))
}

/// Asks the OS whether this process may create and rename files in
/// `directory` (`W_OK | X_OK`), opening nothing; used alone for `create`.
#[cfg(unix)]
fn directory_write_refusal(directory: &Path) -> Option<Refusal> {
    os_access_refusal(directory, libc::W_OK | libc::X_OK)
}

#[cfg(not(unix))]
fn os_write_refusal(path: &Path) -> Option<Refusal> {
    let writable = matches!(fs::metadata(path), Ok(metadata) if !metadata.permissions().readonly());
    (!writable).then_some(Refusal::ReadOnly)
}

#[cfg(not(unix))]
fn directory_write_refusal(directory: &Path) -> Option<Refusal> {
    os_write_refusal(directory)
}

fn write_refusal(metadata: &fs::Metadata) -> Option<Refusal> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        ownership_refusal(metadata.uid(), metadata.mode(), euid)
    }
    #[cfg(not(unix))]
    {
        metadata
            .permissions()
            .readonly()
            .then_some(Refusal::ReadOnly)
    }
}

fn file_owner(metadata: &fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Some(metadata.uid())
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
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

/// Splices `replacement` over `length` bytes at each of `offsets` in the
/// file's text, moves the sites earlier operations recorded so they still
/// point at the same text, and records the new sites.
fn splice_sites(
    file: &mut WorkingFile,
    offsets: &[usize],
    length: usize,
    replacement: &str,
) -> usize {
    let (text, placed) = splice(&file.text, offsets, length, replacement);
    for site in &mut file.site_offsets {
        let before = offsets.partition_point(|offset| *offset <= *site);
        *site = match before.checked_sub(1) {
            // Inside a replaced span: the site now starts the replacement.
            Some(last) if *site < offsets[last] + length => placed[last],
            Some(last) => placed[last] + replacement.len() + (*site - offsets[last] - length),
            None => *site,
        };
    }
    file.text = text;
    file.sites += placed.len();
    file.site_offsets.extend_from_slice(&placed);
    placed.len()
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
            edited_earlier: file.sites > 0,
        });
    }
    // Identical texts (E17-47): the count above still held, nothing changes.
    if old == new {
        return Ok(OperationOutcome {
            kind: "replace",
            sites: 0,
            files: Vec::new(),
            rename: None,
            unchanged: true,
            matched: offsets.len(),
        });
    }
    let sites = splice_sites(file, &offsets, old.len(), &new);
    Ok(OperationOutcome {
        kind: "replace",
        sites,
        files: vec![path.to_owned()],
        rename: None,
        unchanged: false,
        matched: 0,
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
            edited_earlier: file.sites > 0,
        });
    }
    let at = match position {
        InsertPosition::Before => offsets[0],
        InsertPosition::After => offsets[0] + anchor.len(),
    };
    splice_sites(file, &[at], 0, &inserted);
    Ok(OperationOutcome {
        kind: "insert",
        sites: 1,
        files: vec![path.to_owned()],
        rename: None,
        unchanged: false,
        matched: 0,
    })
}

/// Files a rename may skip instead of failing the whole call.
const fn skippable(reason: Refusal) -> bool {
    !matches!(reason, Refusal::InvalidPath | Refusal::NotIndexed)
}

/// Refusals for files that may still be read (only writing them is refused,
/// or they are not text), so a rename can confirm they do not hold the old
/// name instead of listing them as skipped.
const fn readable_refusal(reason: Refusal) -> bool {
    matches!(
        reason,
        Refusal::ControlFile
            | Refusal::Protected
            | Refusal::ReadOnly
            | Refusal::ForeignOwner
            | Refusal::NotUtf8
    )
}

/// The bytes of an in-root file for searching only, through the read gate.
fn peek_bytes(root: &Path, path: &str) -> Option<Vec<u8>> {
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
    (bytes.len() as u64 <= EDIT_MAX_FILE_BYTES).then_some(bytes)
}

/// One rename's arguments.
struct RenameSpec<'a> {
    old: &'a str,
    new: &'a str,
    filter: Option<&'a str>,
    expected: Option<usize>,
    allow_existing: bool,
}

/// Running totals of a rename's scan.
#[derive(Default)]
struct RenameScan {
    skipped: Vec<SkippedFile>,
    searched: BTreeSet<String>,
    matched: Vec<(String, Vec<usize>)>,
    untouched_substring: Vec<EditLocation>,
    untouched_substring_total: usize,
    untouched_adjacent: Vec<EditLocation>,
    untouched_adjacent_total: usize,
    new_name_preexisting: usize,
    new_name_locations: Vec<EditLocation>,
}

impl RenameScan {
    /// Records one searched text; returns its whole-identifier matches.
    fn search(&mut self, path: &str, text: &str, spec: &RenameSpec<'_>) -> Vec<usize> {
        let language = SourceLanguage::for_path(path);
        let Occurrences {
            whole,
            substring,
            adjacent,
        } = identifier_occurrences(text, spec.old, language);
        // The same matcher decides what counts as the new name in use.
        let existing = identifier_occurrences(text, spec.new, language).whole;
        self.new_name_preexisting += existing.len();
        if self.new_name_locations.len() < EDIT_MAX_LISTED_LOCATIONS {
            self.new_name_locations
                .extend(locations(path, text, &existing));
        }
        self.searched.insert(path.to_owned());
        if whole.is_empty() {
            self.untouched_substring_total += substring.len();
            if self.untouched_substring.len() < EDIT_MAX_LISTED_LOCATIONS {
                self.untouched_substring
                    .extend(locations(path, text, &substring));
            }
            self.untouched_adjacent_total += adjacent.len();
            if self.untouched_adjacent.len() < EDIT_MAX_LISTED_LOCATIONS {
                self.untouched_adjacent
                    .extend(locations(path, text, &adjacent));
            }
        }
        whole
    }
}

fn apply_rename(
    gate: &Gate<'_>,
    working: &mut BTreeMap<String, WorkingFile>,
    scanned: &mut u64,
    spec: RenameSpec<'_>,
    operation: usize,
) -> Result<OperationOutcome, EditError> {
    let (old, new) = (spec.old, spec.new);
    let path_filter = match spec.filter {
        Some(filter) => PathFilter::parse(filter).map_err(|error| invalid(error.to_string()))?,
        None => None,
    };
    // The indexed files, and files an earlier operation of this call
    // created (E17-47), which are not indexed until the call writes them.
    let created = working
        .iter()
        .filter(|(_, file)| file.created.is_some())
        .map(|(path, _)| path);
    let scope = gate
        .indexed
        .keys()
        .chain(created)
        .filter(|path| {
            path_filter
                .as_ref()
                .is_none_or(|filter| filter.matches(path))
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut scan = RenameScan::default();
    for path in scope {
        let loaded = if working.contains_key(&path) {
            None
        } else {
            match load(gate, &path) {
                Ok(file) => {
                    *scanned = scanned.saturating_add(file.original.len() as u64);
                    bound(EditBound::ScanBytes, EDIT_MAX_SCAN_BYTES, *scanned)?;
                    Some(file)
                }
                Err(EditError::Refused { reason, .. }) if skippable(reason) => {
                    // A file the edit may not write but may read is skipped
                    // only when it actually holds the old name.
                    let peeked = readable_refusal(reason)
                        .then(|| peek_bytes(gate.root, &path))
                        .flatten();
                    match peeked {
                        Some(bytes) => {
                            *scanned = scanned.saturating_add(bytes.len() as u64);
                            bound(EditBound::ScanBytes, EDIT_MAX_SCAN_BYTES, *scanned)?;
                            let holds_name = match std::str::from_utf8(&bytes) {
                                Ok(text) => {
                                    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
                                    !scan.search(&path, text, &spec).is_empty()
                                }
                                // Not text: skipped only if the bytes hold the
                                // name at all, since it cannot be edited.
                                Err(_) => bytes
                                    .windows(old.len())
                                    .any(|window| window == old.as_bytes()),
                            };
                            if holds_name {
                                scan.searched.remove(&path);
                                scan.skipped.push(SkippedFile { path, reason });
                            } else {
                                scan.searched.insert(path);
                            }
                        }
                        None => scan.skipped.push(SkippedFile { path, reason }),
                    }
                    continue;
                }
                Err(error) => return Err(error),
            }
        };
        let text = loaded
            .as_ref()
            .map_or_else(|| working[&path].text.clone(), |file| file.text.clone());
        let whole = scan.search(&path, &text, &spec);
        if !whole.is_empty() {
            // A file the OS will not let this process replace (immutable,
            // read-only mount, access-control list, unwritable directory) is
            // skipped and listed, like any other file the sweep may not edit.
            if loaded.is_some()
                && let Some(reason) = os_write_refusal(&gate.root.join(&path))
            {
                scan.searched.remove(&path);
                scan.skipped.push(SkippedFile { path, reason });
                continue;
            }
            scan.matched.push((path.clone(), whole));
            // Fail as soon as the bound is crossed, not after reading the rest.
            bound(
                EditBound::Files,
                EDIT_MAX_FILES as u64,
                scan.matched.len() as u64,
            )?;
            if let Some(file) = loaded {
                working.insert(path, file);
            }
        }
    }
    let RenameScan {
        skipped,
        searched,
        matched,
        mut untouched_substring,
        untouched_substring_total,
        untouched_adjacent,
        untouched_adjacent_total,
        new_name_preexisting,
        mut new_name_locations,
    } = scan;
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
    if let Some(expected) = spec.expected
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
            edited_earlier: false,
        });
    }
    if new_name_preexisting > 0 && !spec.allow_existing {
        new_name_locations.truncate(EDIT_MAX_LISTED_LOCATIONS);
        return Err(EditError::NameExists {
            operation,
            name: new.to_owned(),
            count: new_name_preexisting,
            locations: new_name_locations,
        });
    }
    bound(EditBound::Sites, EDIT_MAX_SITES as u64, total as u64)?;

    let mut remaining_whole = Vec::new();
    let mut remaining_whole_total = 0_usize;
    let mut remaining_substring = untouched_substring;
    let mut remaining_substring_total = untouched_substring_total;
    let mut remaining_adjacent = untouched_adjacent;
    let mut remaining_adjacent_total = untouched_adjacent_total;
    let mut files = Vec::with_capacity(matched.len());
    for (path, offsets) in &matched {
        let file = working.get_mut(path).expect("matched files are loaded");
        splice_sites(file, offsets, old.len(), new);
        // Verify against the edited text rather than assume: a new name that
        // contains the old one, or adjacent sites, would show up here.
        let Occurrences {
            whole,
            substring,
            adjacent,
        } = identifier_occurrences(&file.text, old, file.language);
        remaining_adjacent_total += adjacent.len();
        if remaining_adjacent.len() < EDIT_MAX_LISTED_LOCATIONS {
            remaining_adjacent.extend(locations(path, &file.text, &adjacent));
        }
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
    remaining_adjacent.truncate(EDIT_MAX_LISTED_LOCATIONS);
    Ok(OperationOutcome {
        kind: "rename",
        sites: total,
        files,
        unchanged: false,
        matched: 0,
        rename: Some(RenameOutcome {
            scope: spec.filter.map(str::to_owned),
            files_searched: searched.len(),
            // Outside-scope matches are added once every operation ran.
            complete: skipped.is_empty() && gate.index_refreshed,
            files_skipped: skipped,
            remaining_whole_total,
            remaining_whole,
            remaining_substring_total,
            remaining_substring,
            remaining_adjacent_total,
            remaining_adjacent,
            new_name_preexisting,
            outside_whole_total: 0,
            outside_whole: Vec::new(),
            outside_unsearched_total: 0,
            outside_unsearched: Vec::new(),
        }),
    })
}

/// Whole matches of `old` a scoped rename left outside its scope.
#[derive(Default)]
struct Outside {
    total: usize,
    locations: Vec<EditLocation>,
    unsearched_total: usize,
    unsearched: Vec<String>,
}

/// Counts the whole matches of `old` in the indexed files outside `filter`
/// as the call leaves them: a file an operation edited is read from the
/// working set, any other through the read gate only (nothing is written).
/// Reads share the call's `scanned` budget; a file that would cross it, that
/// cannot be read as text, or that the scanner excludes is counted as not
/// searched rather than failing the call. Secret paths are never searched.
fn outside_matches(
    gate: &Gate<'_>,
    working: &BTreeMap<String, WorkingFile>,
    filter: &PathFilter,
    old: &str,
    scanned: &mut u64,
) -> Outside {
    let mut outside = Outside::default();
    let unsearched = |outside: &mut Outside, path: &str| {
        outside.unsearched_total += 1;
        if outside.unsearched.len() < EDIT_MAX_LISTED_LOCATIONS {
            outside.unsearched.push(path.to_owned());
        }
    };
    for (path, record) in gate
        .indexed
        .iter()
        .filter(|(path, _)| !filter.matches(path))
    {
        let relative = Path::new(path);
        if is_secret_path(relative) {
            continue;
        }
        // What the scanner excludes (E17-35: `info/exclude`, nested
        // worktrees) or `.git` is never read, even from a stale index row.
        if relative
            .components()
            .any(|component| component.as_os_str() == ".git")
            || gate.exclusions.excludes(gate.root, relative)
        {
            unsearched(&mut outside, path);
            continue;
        }
        let peeked;
        let (text, language) = if let Some(file) = working.get(path) {
            (file.text.as_str(), file.language)
        } else {
            if scanned.saturating_add(record.size_bytes) > EDIT_MAX_SCAN_BYTES {
                unsearched(&mut outside, path);
                continue;
            }
            let Some(bytes) = peek_bytes(gate.root, path) else {
                unsearched(&mut outside, path);
                continue;
            };
            *scanned = scanned.saturating_add(bytes.len() as u64);
            match String::from_utf8(bytes) {
                Ok(text) => {
                    peeked = text;
                    (
                        peeked.strip_prefix('\u{feff}').unwrap_or(&peeked),
                        SourceLanguage::for_path(path),
                    )
                }
                // Not text: identifiers cannot be told apart, so a file
                // holding the bytes of the name is reported as not searched.
                Err(error) => {
                    if error
                        .as_bytes()
                        .windows(old.len())
                        .any(|window| window == old.as_bytes())
                    {
                        unsearched(&mut outside, path);
                    }
                    continue;
                }
            }
        };
        if !text.contains(old) {
            continue;
        }
        let whole = identifier_occurrences(text, old, language).whole;
        outside.total += whole.len();
        if outside.locations.len() < EDIT_MAX_LISTED_LOCATIONS {
            outside.locations.extend(locations(path, text, &whole));
        }
    }
    // Files this call created are not indexed yet (E17-47); bounded by the
    // operation count.
    for (path, file) in working
        .iter()
        .filter(|(path, file)| file.created.is_some() && !filter.matches(path))
    {
        let whole = identifier_occurrences(&file.text, old, file.language).whole;
        outside.total += whole.len();
        if outside.locations.len() < EDIT_MAX_LISTED_LOCATIONS {
            outside
                .locations
                .extend(locations(path, &file.text, &whole));
        }
    }
    outside.locations.truncate(EDIT_MAX_LISTED_LOCATIONS);
    outside
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

/// A temporary file next to `target`, unique to this process and call.
fn temp_beside(target: &Path, role: &str) -> io::Result<PathBuf> {
    let directory = target
        .parent()
        .ok_or_else(|| io::Error::other("the file has no parent directory"))?;
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(directory.join(format!(
        ".{name}.compresso-edit-{role}-{}-{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    )))
}

/// Creates `temp` (never over an existing file) holding `bytes` with
/// `permissions`, synced to disk. Without `permissions` (a created file) it
/// gets the default mode for a new file under the process umask.
fn write_temp(temp: &Path, bytes: &[u8], permissions: Option<&fs::Permissions>) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(if permissions.is_some() { 0o600 } else { 0o666 });
    }
    let mut file = options.open(temp)?;
    let written = (|| {
        file.write_all(bytes)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions.clone())?;
        }
        file.sync_all()
    })();
    if written.is_err() {
        drop(file);
        let _ = fs::remove_file(temp);
    }
    written
}

fn sync_directory(path: &Path) {
    #[cfg(unix)]
    if let Some(directory) = path.parent()
        && let Ok(directory) = fs::File::open(directory)
    {
        let _ = directory.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// One file ready to commit: the edited bytes and the original bytes, both
/// already written and synced beside the target.
struct Staged<'a> {
    path: &'a str,
    file: &'a WorkingFile,
    target: PathBuf,
    edited: PathBuf,
    /// The copy of the original bytes; `None` for a created file, which has
    /// none: undoing it removes it.
    original: Option<PathBuf>,
    hash_after: String,
}

/// Resolves where `file` is written and confirms the target is still what
/// was validated: an existing file through the read gate with the same
/// identity and bytes ([`reverify`]); for a created file, the same real
/// directory (identity on Unix) with still nothing at the path.
///
/// For a created file under new directories (E17-49), the ones this call
/// already made (`made`) must be the directories it made, by identity on
/// Unix, and the rest must still be missing.
fn recheck(
    root: &Path,
    path: &str,
    file: &WorkingFile,
    made: &[MadeDirectory],
) -> io::Result<PathBuf> {
    let Some(new_file) = &file.created else {
        return reverify(root, path, file.identity, &file.hash_before);
    };
    let changed = || io::Error::other("the path changed after it was validated");
    let (again, _) = resolve_new_file(root, path).map_err(|_| changed())?;
    // Directories are made outermost first, so the made ones are a prefix.
    let done = new_file
        .new_directories
        .iter()
        .take_while(|wanted| made.iter().any(|made| made.relative == wanted.relative))
        .count();
    let identity = match done.checked_sub(1) {
        None => new_file.directory_identity,
        Some(last) => made
            .iter()
            .find(|made| made.relative == new_file.new_directories[last].relative)
            .and_then(|made| made.identity),
    };
    if again.target != new_file.target
        || again.directory_identity != identity
        || again.new_directories != new_file.new_directories[done..]
    {
        return Err(changed());
    }
    Ok(again.target)
}

/// The directories the created files of `changed` add, each once,
/// outermost first (E17-49).
fn planned_directories(changed: &[(String, WorkingFile)]) -> Vec<&NewDirectory> {
    let mut directories = changed
        .iter()
        .filter_map(|(_, file)| file.created.as_ref())
        .flat_map(|new_file| &new_file.new_directories)
        .collect::<Vec<_>>();
    directories.sort_by(|left, right| {
        (left.relative.matches('/').count(), &left.relative)
            .cmp(&(right.relative.matches('/').count(), &right.relative))
    });
    directories.dedup_by(|left, right| left.relative == right.relative);
    directories
}

/// Creates `wanted` outermost first, each with a create-new call that
/// fails if anything is there (E17-49): something that appeared since
/// validation stops the call (`changed`) rather than being used. Each
/// directory made is recorded in `made` and confirmed to be a real
/// directory reached from the root without a link. Directories get the
/// platform default (mode 0o777 under the umask on Unix).
fn make_directories(
    root: &Path,
    wanted: &[&NewDirectory],
    made: &mut Vec<MadeDirectory>,
) -> Result<(), EditError> {
    for directory in wanted {
        #[cfg(test)]
        tests::run_before_directory(&directory.absolute);
        match fs::create_dir(&directory.absolute) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(EditError::Changed {
                    path: directory.relative.clone(),
                });
            }
            Err(source) => {
                return Err(EditError::WriteFailed {
                    path: directory.relative.clone(),
                    restored: Vec::new(),
                    unrestored: Vec::new(),
                    backups: Vec::new(),
                    source,
                });
            }
        }
        let identity = file_identity(&directory.absolute);
        made.push(MadeDirectory {
            relative: directory.relative.clone(),
            absolute: directory.absolute.clone(),
            identity,
        });
        sync_directory(&directory.absolute);
        let real = fs::symlink_metadata(&directory.absolute)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
        if !real || !matches!(leaf_state(root, &directory.relative), LeafState::Other) {
            return Err(EditError::Changed {
                path: directory.relative.clone(),
            });
        }
    }
    Ok(())
}

/// Removes the directories in `made`, deepest first, each only while it is
/// still the directory made (identity on Unix) and empty: never one the
/// call did not create, never one holding anything.
fn remove_made_directories(made: &[MadeDirectory]) {
    for directory in made.iter().rev() {
        let same = fs::symlink_metadata(&directory.absolute)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
            && file_identity(&directory.absolute) == directory.identity;
        if same && fs::remove_dir(&directory.absolute).is_ok() {
            sync_directory(&directory.absolute);
        }
    }
}

/// Commits the changed files all or nothing.
///
/// 1. Every file is re-verified (same gate, identity, and content hash; for
///    a created file, same directory and nothing at the path).
/// 2. The directories the created files need are made, outermost first
///    (E17-49); then, for every file, the edited bytes and a copy of the
///    original bytes (an existing file only) are written to temporary files
///    beside it and synced, so no later step needs new disk space. A failure
///    here changes no repository file, and the directories made are removed
///    again.
/// 3. Each target is re-verified once more and the edited file renamed over
///    it, or, for a created file, hard-linked to the path, which fails if
///    anything appeared there. If one step fails, every file already
///    written is undone after checking it still holds exactly the edited
///    bytes: an existing file by renaming its original copy back (no
///    allocation), a created file by removing it; then the directories made
///    are removed, deepest first, each only while still empty.
///
/// Returns the directories made, outermost first.
fn commit(
    root: &Path,
    changed: &[(String, WorkingFile)],
    before_write: &mut dyn FnMut(usize, &Path) -> io::Result<()>,
) -> Result<Vec<String>, EditError> {
    for (path, file) in changed {
        if recheck(root, path, file, &[]).is_err() {
            return Err(EditError::Changed { path: path.clone() });
        }
    }
    let mut made = Vec::new();
    if let Err(error) = make_directories(root, &planned_directories(changed), &mut made) {
        remove_made_directories(&made);
        return Err(error);
    }
    let mut staged = Vec::<Staged<'_>>::with_capacity(changed.len());
    // Removes the staged files, except the original copies in `keep`: for a
    // file left unrestored that copy is the only one left.
    let cleanup = |staged: &[Staged<'_>], keep: &[usize]| {
        for (index, entry) in staged.iter().enumerate() {
            let _ = fs::remove_file(&entry.edited);
            if !keep.contains(&index)
                && let Some(original) = &entry.original
            {
                let _ = fs::remove_file(original);
            }
        }
    };
    for (path, file) in changed {
        let prepared = (|| {
            let target = recheck(root, path, file, &made)?;
            let edited_bytes = file.edited_bytes();
            let edited = temp_beside(&target, "new")?;
            write_temp(&edited, &edited_bytes, file.permissions.as_ref())?;
            // A replacement is owned by this process's user; refuse rather than
            // change the owner of another user's file.
            if file.owner.is_some()
                && fs::symlink_metadata(&edited)
                    .ok()
                    .and_then(|metadata| file_owner(&metadata))
                    != file.owner
            {
                let _ = fs::remove_file(&edited);
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    FOREIGN_OWNER,
                ));
            }
            let original = if file.created.is_some() {
                None
            } else {
                let original = temp_beside(&target, "old")?;
                if let Err(error) = write_temp(&original, &file.original, file.permissions.as_ref())
                {
                    let _ = fs::remove_file(&edited);
                    return Err(error);
                }
                Some(original)
            };
            Ok(Staged {
                path,
                file,
                target,
                edited,
                original,
                hash_after: blake3::hash(&edited_bytes).to_hex().to_string(),
            })
        })();
        match prepared {
            Ok(entry) => staged.push(entry),
            Err(source) => {
                cleanup(&staged, &[]);
                remove_made_directories(&made);
                if source.to_string() == FOREIGN_OWNER {
                    return Err(refused(path, Refusal::ForeignOwner));
                }
                return Err(EditError::WriteFailed {
                    path: path.clone(),
                    restored: Vec::new(),
                    unrestored: Vec::new(),
                    backups: Vec::new(),
                    source,
                });
            }
        }
    }
    let mut renamed = Vec::<(usize, Option<(u64, u64)>)>::new();
    for (index, entry) in staged.iter().enumerate() {
        let outcome = before_write(index, &entry.target).and_then(|()| {
            // The last check before the write: same gate, file, and bytes, or
            // the same directory with nothing at the path.
            let again = recheck(root, entry.path, entry.file, &made)?;
            if again != entry.target {
                return Err(io::Error::other("the file changed after it was validated"));
            }
            if entry.file.created.is_some() {
                #[cfg(test)]
                tests::run_after_recheck(&entry.target);
                // Never over an existing path: a link fails if one appeared.
                fs::hard_link(&entry.edited, &entry.target)?;
                let _ = fs::remove_file(&entry.edited);
                Ok(())
            } else {
                fs::rename(&entry.edited, &entry.target)
            }
        });
        match outcome {
            Ok(()) => {
                sync_directory(&entry.target);
                renamed.push((index, file_identity(&entry.target)));
            }
            Err(source) => {
                let mut restored = Vec::new();
                let mut unrestored = Vec::new();
                let mut kept = Vec::new();
                for (done, identity) in renamed.into_iter().rev() {
                    let entry = &staged[done];
                    let back =
                        reverify(root, entry.path, identity, &entry.hash_after).and_then(|_| {
                            match &entry.original {
                                Some(original) => fs::rename(original, &entry.target),
                                None => fs::remove_file(&entry.target),
                            }
                        });
                    match back {
                        Ok(()) => {
                            sync_directory(&entry.target);
                            restored.push(entry.path.to_owned());
                        }
                        Err(_) => {
                            unrestored.push(entry.path.to_owned());
                            kept.push(done);
                        }
                    }
                }
                restored.reverse();
                unrestored.reverse();
                kept.reverse();
                cleanup(&staged, &kept);
                remove_made_directories(&made);
                let backups = kept
                    .iter()
                    .filter_map(|index| {
                        staged[*index]
                            .original
                            .as_ref()?
                            .strip_prefix(root)
                            .ok()
                            .map(|path| path.to_string_lossy().into_owned())
                    })
                    .collect();
                return Err(EditError::WriteFailed {
                    path: entry.path.to_owned(),
                    restored,
                    unrestored,
                    backups,
                    source,
                });
            }
        }
    }
    cleanup(&staged, &[]);
    Ok(made
        .into_iter()
        .map(|directory| directory.relative)
        .collect())
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
fn append_diff(out: &mut String, path: &str, before: &str, after: &str, created: bool) -> bool {
    let mut diff = unified_diff(path, before, after);
    if created {
        diff = diff.replacen(&format!("--- a/{path}\n"), "--- /dev/null\n", 1);
    }
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

    type AfterRecheck = Box<dyn FnMut(&Path)>;

    thread_local! {
        pub(super) static REFRESHES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
        /// Runs between a created file's last re-check and its link step.
        static AFTER_RECHECK: std::cell::RefCell<Option<AfterRecheck>> =
            const { std::cell::RefCell::new(None) };
    }

    type BeforeDirectory = Box<dyn FnMut(&Path)>;

    thread_local! {
        /// Runs before the call creates each directory (E17-49).
        static BEFORE_DIRECTORY: std::cell::RefCell<Option<BeforeDirectory>> =
            const { std::cell::RefCell::new(None) };
    }

    pub(super) fn run_before_directory(directory: &Path) {
        BEFORE_DIRECTORY.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook(directory);
            }
        });
    }

    pub(super) fn run_after_recheck(target: &Path) {
        AFTER_RECHECK.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook(target);
            }
        });
    }

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
    fn test_a_rename_loads_the_exclude_rules_independently_of_its_file_count() {
        let loads_for = |files: usize| {
            let mut sources = vec![(".git/info/exclude".to_owned(), "*.log\n".to_owned())];
            for number in 0..files {
                sources.push((
                    format!("src/f{number}.rs"),
                    "fn shared_name() {}\n".to_owned(),
                ));
            }
            let borrowed = sources
                .iter()
                .map(|(path, text)| (path.as_str(), text.as_str()))
                .collect::<Vec<_>>();
            let fixture = Fixture::new(&borrowed);
            let before = crate::scanner::exclude_loads_on_this_thread();
            let report = fixture
                .apply(
                    vec![EditOperation::Rename {
                        old: "shared_name".to_owned(),
                        new: "renamed".to_owned(),
                        path: None,
                        expected_count: None,
                        allow_existing: false,
                    }],
                    &mut |_, _| Ok(()),
                )
                .expect("rename applies");
            assert_eq!(report.operations[0].files.len(), files);
            crate::scanner::exclude_loads_on_this_thread() - before
        };
        assert_eq!(loads_for(2), loads_for(12));
    }

    #[test]
    fn test_identifier_boundaries_are_explicit() {
        let text = "format_utc(); format_utc_now(); reformat_utc(); x.format_utc2; café_format_utc; $format_utc";
        let found = identifier_occurrences(text, "format_utc", SourceLanguage::Other);
        assert_eq!(found.whole.len(), 2, "call and `$format_utc`");
        assert_eq!(found.substring.len(), 4);
        let found = identifier_occurrences(text, "format_utc", SourceLanguage::DollarIdentifier);
        assert_eq!(found.whole.len(), 1, "`$format_utc` is another identifier");
        assert!(is_identifier("naïve_ß"));
        assert!(!is_identifier("1abc"));
        assert!(!is_identifier("a-b"));
        assert!(!is_identifier(""));
    }

    #[test]
    fn test_control_paths_are_matched_at_any_depth_and_any_case() {
        for path in [
            ".gitignore",
            "sub/.GITIGNORE",
            ".gitlab-ci.yml",
            ".Claude/settings.json",
            "pkg/.claude/agents/x.md",
            "CLAUDE.md",
            "docs/claude.md",
            "CLAUDE.local.md",
            "AGENTS.md",
            "GEMINI.md",
            ".cursorrules",
            ".windsurfrules",
            ".clinerules",
            ".claude.json",
            ".aider.conf.yml",
            ".aider/cache",
            ".github/copilot-instructions.md",
            ".pre-commit-config.yaml",
            ".circleci/config.yml",
            ".devcontainer/devcontainer.json",
            ".travis.yml",
            "azure-pipelines.yml",
            "Jenkinsfile",
            ".envrc",
            ".buildkite/pipeline.yml",
            ".woodpecker/ci.yml",
            "lefthook.yml",
            ".lefthook.yml",
            ".windsurf/rules.md",
            ".gemini/settings.json",
            ".continue/config.json",
            ".zed/settings.json",
            ".mcp.json",
            ".husky/pre-commit",
            "AGENTS.md.",
            ".envrc ",
        ] {
            assert!(is_control_path(Path::new(path)), "{path}");
        }
        for path in [
            "src/lib.rs",
            "build.rs",
            "package.json",
            "Makefile",
            "docs/agents.txt",
            "claude/notes.md",
            "src/.claudeignore_not",
        ] {
            assert!(!is_control_path(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn test_escapes_and_literal_prefixes_are_adjacent_not_whole() {
        let other = SourceLanguage::Other;
        let found = identifier_occurrences("let n = 1; print(\"a\\n\", n)", "n", other);
        assert_eq!(found.whole.len(), 2, "the binding and the argument");
        assert_eq!(found.substring.len(), 1, "`print`");
        assert_eq!(found.adjacent.len(), 1, "the `\\n` escape");
        let found = identifier_occurrences("f = 1\ns = f\"x{f}\"\n", "f", other);
        assert_eq!((found.whole.len(), found.adjacent.len()), (2, 1));
        let found = identifier_occurrences("let r = r#type; b'x'; r", "r", other);
        assert_eq!((found.whole.len(), found.adjacent.len()), (2, 1));
        assert!(identifier_occurrences("b'x'", "b", other).whole.is_empty());
        // Not a known prefix: a quote after the name is an ordinary boundary.
        assert_eq!(
            identifier_occurrences("foo\"x\"", "foo", other).whole.len(),
            1
        );
        // Not an escape form: `\App\User` is a namespace.
        assert_eq!(
            identifier_occurrences("\\App\\User", "User", other)
                .whole
                .len(),
            1
        );
        // An even run of backslashes escapes itself, not the letter after it.
        assert_eq!(identifier_occurrences("\\\\n", "n", other).whole.len(), 1);
        assert_eq!(identifier_occurrences("%d d", "d", other).adjacent.len(), 1);
        let rust = SourceLanguage::Rust;
        assert_eq!(
            identifier_occurrences("<'a>(a)", "a", rust).adjacent.len(),
            1
        );
        assert_eq!(
            SourceLanguage::for_path("src/app.TS"),
            SourceLanguage::DollarIdentifier
        );
        assert_eq!(SourceLanguage::for_path("lib.rs"), SourceLanguage::Rust);
        assert_eq!(SourceLanguage::for_path("run.sh"), SourceLanguage::Other);
    }

    /// E17-40 (D105): in C#, `@name` is the verbatim spelling of the same
    /// identifier `name` (`@class`, `@event`), so it is renamed; `$` is not
    /// an identifier character (`$"{name}"` is an interpolated string), so
    /// C# needs no `$` rule. Java keeps `$name` as a different identifier.
    #[test]
    fn test_csharp_verbatim_identifiers_are_renamed_and_java_dollar_names_are_not() {
        let csharp = SourceLanguage::for_path("src/Acme/Store.cs");
        assert_eq!(csharp, SourceLanguage::Other);
        let found =
            identifier_occurrences("var @event = event; Log($\"{event}\");", "event", csharp);
        assert_eq!(found.whole.len(), 3, "{found:?}");
        assert!(found.substring.is_empty() && found.adjacent.is_empty());
        let java = SourceLanguage::for_path("src/a/Store.java");
        assert_eq!(java, SourceLanguage::DollarIdentifier);
        let found = identifier_occurrences("int $count = count;", "count", java);
        assert_eq!((found.whole.len(), found.substring.len()), (1, 1));
    }

    #[cfg(unix)]
    #[test]
    fn test_ownership_and_write_bits_decide_without_opening_the_file() {
        // Owner bits alone decide for the owner (POSIX).
        assert_eq!(ownership_refusal(501, 0o464, 501), Some(Refusal::ReadOnly));
        assert_eq!(ownership_refusal(501, 0o644, 501), None);
        // Another user's file: a replacement would change its owner.
        assert_eq!(
            ownership_refusal(0, 0o666, 501),
            Some(Refusal::ForeignOwner)
        );
        assert_eq!(
            ownership_refusal(501, 0o644, 0),
            Some(Refusal::ForeignOwner)
        );
        // No exemption for root: no owner write bit is read-only for anyone.
        assert_eq!(ownership_refusal(0, 0o444, 0), Some(Refusal::ReadOnly));
        assert_eq!(ownership_refusal(0, 0o644, 0), None);
    }

    #[test]
    fn test_a_rename_shares_one_scan_budget_across_the_call() {
        let fixture = Fixture::new(&[("a.rs", "fn alpha() {}\n")]);
        let root = fixture.root.clone();
        let indexed = fixture
            .store
            .indexed_files(fixture.store.repo_by_root(&root).unwrap().expect("repo").id)
            .unwrap()
            .into_iter()
            .map(|record| (record.path.clone(), record))
            .collect::<BTreeMap<_, _>>();
        let exclusions = ScannerExclusions::load(&root);
        let gate = Gate {
            root: &root,
            exclusions: &exclusions,
            indexed: &indexed,
            protected: &[],
            index_refreshed: true,
        };
        // An earlier rename in the call already read almost the whole budget.
        let mut scanned = EDIT_MAX_SCAN_BYTES - 4;
        let error = apply_rename(
            &gate,
            &mut BTreeMap::new(),
            &mut scanned,
            RenameSpec {
                old: "alpha",
                new: "beta",
                filter: None,
                expected: None,
                allow_existing: false,
            },
            2,
        )
        .expect_err("budget is per call");
        assert!(matches!(
            error,
            EditError::Bound {
                bound: EditBound::ScanBytes,
                ..
            }
        ));
    }

    /// E17-36 (D101, review): the outside-scope count shares the call's
    /// read budget; when it runs out, the rest is reported as not searched,
    /// never as a match and never as an error.
    #[test]
    fn test_the_outside_scope_count_shares_the_call_budget_and_says_what_it_skipped() {
        let fixture = Fixture::new(&[("a.rs", "fn alpha() {}\n"), ("b/c.rs", "// alpha\n")]);
        let root = fixture.root.clone();
        let indexed = fixture
            .store
            .indexed_files(fixture.store.repo_by_root(&root).unwrap().expect("repo").id)
            .unwrap()
            .into_iter()
            .map(|record| (record.path.clone(), record))
            .collect::<BTreeMap<_, _>>();
        let exclusions = ScannerExclusions::load(&root);
        let gate = Gate {
            root: &root,
            exclusions: &exclusions,
            indexed: &indexed,
            protected: &[],
            index_refreshed: true,
        };
        let filter = PathFilter::parse("a.rs").unwrap().expect("filter");
        let mut scanned = 0;
        let outside = outside_matches(&gate, &BTreeMap::new(), &filter, "alpha", &mut scanned);
        assert_eq!((outside.total, outside.unsearched_total), (1, 0));
        assert!(scanned > 0, "reads count against the call budget");
        let mut scanned = EDIT_MAX_SCAN_BYTES - 4;
        let outside = outside_matches(&gate, &BTreeMap::new(), &filter, "alpha", &mut scanned);
        assert_eq!(outside.total, 0);
        assert_eq!(outside.unsearched_total, 1);
        assert_eq!(outside.unsearched, ["b/c.rs"]);
    }

    /// E17-36 with E17-35: even from a stale index row (a refresh that
    /// failed), the outside-scope count never reads a file the scanner
    /// excludes; it is reported as not searched.
    #[test]
    fn test_the_outside_scope_count_never_reads_an_excluded_stale_row() {
        let fixture = Fixture::new(&[
            (".git/HEAD", "ref: refs/heads/main\n"),
            ("a.rs", "fn alpha() {}\n"),
            ("copy/c.rs", "// alpha\n"),
        ]);
        let root = fixture.root.clone();
        let indexed = fixture
            .store
            .indexed_files(fixture.store.repo_by_root(&root).unwrap().expect("repo").id)
            .unwrap()
            .into_iter()
            .map(|record| (record.path.clone(), record))
            .collect::<BTreeMap<_, _>>();
        assert!(indexed.contains_key("copy/c.rs"));
        fs::write(root.join(".git/info/exclude"), "copy/\n")
            .or_else(|_| {
                fs::create_dir_all(root.join(".git/info"))?;
                fs::write(root.join(".git/info/exclude"), "copy/\n")
            })
            .expect("exclude rule");
        let exclusions = ScannerExclusions::load(&root);
        let gate = Gate {
            root: &root,
            exclusions: &exclusions,
            indexed: &indexed,
            protected: &[],
            index_refreshed: false,
        };
        let filter = PathFilter::parse("a.rs").unwrap().expect("filter");
        let mut scanned = 0;
        let outside = outside_matches(&gate, &BTreeMap::new(), &filter, "alpha", &mut scanned);
        assert_eq!(outside.total, 0);
        assert_eq!(outside.unsearched, ["copy/c.rs"]);
        assert_eq!(scanned, 0, "nothing excluded was read");
    }

    #[test]
    fn test_a_restore_is_refused_when_the_written_file_changed_again() {
        let fixture = Fixture::new(&[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
        let root = fixture.root.clone();
        let error = fixture
            .apply(
                vec![
                    replace("a.rs", "fn a", "fn aa"),
                    replace("b.rs", "fn b", "fn bb"),
                ],
                &mut |index, _| {
                    if index == 1 {
                        // Someone edits a.rs after Compresso wrote it.
                        fs::write(root.join("a.rs"), "fn theirs() {}\n")?;
                        Err(io::Error::other("injected"))
                    } else {
                        Ok(())
                    }
                },
            )
            .expect_err("second write fails");
        match error {
            EditError::WriteFailed {
                restored,
                unrestored,
                ..
            } => {
                assert!(restored.is_empty());
                assert_eq!(unrestored, ["a.rs"]);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            fs::read_to_string(root.join("a.rs")).unwrap(),
            "fn theirs() {}\n",
            "a concurrent change is never overwritten by the restore"
        );
        assert_eq!(
            fs::read_to_string(root.join("b.rs")).unwrap(),
            "fn b() {}\n"
        );
        let leftovers = fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(
            leftovers, 1,
            "only the unrestored file's original copy is kept"
        );
    }

    #[test]
    fn test_an_unrestored_file_keeps_its_original_copy() {
        let fixture = Fixture::new(&[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
        let root = fixture.root.clone();
        let error = fixture
            .apply(
                vec![
                    replace("a.rs", "fn a", "fn aa"),
                    replace("b.rs", "fn b", "fn bb"),
                ],
                &mut |index, _| {
                    if index == 1 {
                        fs::write(root.join("a.rs"), "fn theirs() {}\n")?;
                        Err(io::Error::other("injected"))
                    } else {
                        Ok(())
                    }
                },
            )
            .expect_err("second write fails");
        let EditError::WriteFailed { .. } = error else {
            panic!("unexpected {error:?}");
        };
        let originals = fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("compresso-edit-old")
            })
            .map(|entry| fs::read_to_string(entry.path()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            originals,
            ["fn a() {}\n"],
            "the only copy of the original is kept"
        );
        assert!(error.to_string().contains("compresso-edit-old"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn test_a_staging_failure_changes_no_file() {
        let fixture = Fixture::new(&[("a.rs", "fn a() {}\n"), ("sub/b.rs", "fn b() {}\n")]);
        let sub = fixture.root.join("sub");
        // Occupy every temporary name the next calls could choose beside
        // b.rs, so creating its staged copy fails, as on a full disk. (A
        // read-only directory is now refused before staging.)
        let first = TEMP_COUNTER.load(Ordering::Relaxed);
        let occupied = (first..first + 2_000)
            .flat_map(|counter| {
                ["new", "old"].map(|role| {
                    sub.join(format!(
                        ".b.rs.compresso-edit-{role}-{}-{counter}.tmp",
                        std::process::id()
                    ))
                })
            })
            .collect::<Vec<_>>();
        for path in &occupied {
            fs::write(path, "").unwrap();
        }
        let error = fixture
            .apply(
                vec![
                    replace("a.rs", "fn a", "fn aa"),
                    replace("sub/b.rs", "fn b", "fn bb"),
                ],
                &mut |_, _| Ok(()),
            )
            .expect_err("staging fails");
        for path in &occupied {
            let _ = fs::remove_file(path);
        }
        match error {
            EditError::WriteFailed {
                path,
                restored,
                unrestored,
                ..
            } => {
                assert_eq!(path, "sub/b.rs");
                assert!(restored.is_empty() && unrestored.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            fs::read_to_string(fixture.root.join("a.rs")).unwrap(),
            "fn a() {}\n"
        );
        let leftovers = fs::read_dir(&fixture.root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }

    fn create(path: &str, content: &str) -> EditOperation {
        EditOperation::Create {
            path: path.to_owned(),
            content: content.to_owned(),
        }
    }

    /// Index refreshes this thread attempted (review: refresh memory).
    fn refreshes() -> u64 {
        REFRESHES.with(std::cell::Cell::get)
    }

    /// Final review item 2: an edit naming a non-UTF-8 text file is refused
    /// without any refresh. Item 3: a failing refresh is remembered too.
    #[test]
    fn test_a_non_utf8_target_and_a_failed_refresh_cost_no_repeated_refresh() {
        let fixture = Fixture::new(&[("a.rs", "fn a() {}\n")]);
        fs::write(fixture.root.join("latin.py"), b"# caf\xe9\nx = 1\n").unwrap();
        let before = refreshes();
        let error = fixture
            .apply(vec![replace("latin.py", "x = 1", "x = 2")], &mut |_, _| {
                Ok(())
            })
            .map(|_| ())
            .expect_err("not indexable");
        assert!(matches!(error, EditError::Refused { .. }), "{error:?}");
        assert_eq!(refreshes(), before, "no refresh for a non-UTF-8 target");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::write(fixture.root.join("new.rs"), "fn n() {}\n").unwrap();
            let locked = fixture.root.join("locked");
            fs::create_dir(&locked).unwrap();
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
            if fs::read_dir(&locked).is_ok() {
                fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
                eprintln!("skipped: this process can read a 0o000 directory");
                return;
            }
            let before = refreshes();
            for _ in 0..3 {
                let _ = fixture.apply(vec![replace("new.rs", "fn n", "fn m")], &mut |_, _| Ok(()));
            }
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(refreshes() - before, 1, "a failed refresh is not repeated");
        }
    }

    /// E17-47 review item 2: a file that appears after the last re-check and
    /// before the link step is never replaced.
    #[test]
    fn test_a_file_appearing_after_the_last_recheck_is_never_replaced() {
        let fixture = Fixture::new(&[("b.rs", "fn b() {}\n")]);
        AFTER_RECHECK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(|target: &Path| {
                fs::write(target, "fn theirs() {}\n").expect("planted");
            }));
        });
        let result = fixture.apply(vec![create("a.rs", "fn mine() {}\n")], &mut |_, _| Ok(()));
        AFTER_RECHECK.with(|hook| *hook.borrow_mut() = None);
        let error = result.expect_err("the path was taken");
        assert!(matches!(error, EditError::WriteFailed { .. }), "{error:?}");
        assert_eq!(
            fs::read_to_string(fixture.root.join("a.rs")).unwrap(),
            "fn theirs() {}\n",
            "the file that appeared is kept"
        );
    }

    /// E17-47 review item 7: the canonical spelling of a created file's
    /// directory is judged, on every file system: `Ignored/x` as a
    /// case-insensitive file system spells `ignored/x`.
    #[test]
    fn test_create_judges_the_canonical_directory_spelling_on_every_file_system() {
        let fixture = Fixture::new(&[(".gitignore", "ignored/\n"), ("ignored/keep.txt", "x\n")]);
        let exclusions = ScannerExclusions::load(&fixture.root);
        let indexed = BTreeMap::new();
        let gate = Gate {
            root: &fixture.root,
            exclusions: &exclusions,
            indexed: &indexed,
            protected: &[],
            index_refreshed: true,
        };
        judge_create_path(&gate, "Ignored/new.rs", Path::new("Ignored/new.rs"))
            .expect("the requested spelling alone passes");
        let new_file = NewFile {
            target: fixture.root.join("ignored/new.rs"),
            anchor: fixture.root.join("ignored"),
            directory_identity: None,
            new_directories: Vec::new(),
        };
        let error = check_new_file(
            &gate,
            "Ignored/new.rs",
            "x\n",
            new_file,
            Path::new("ignored/new.rs"),
        )
        .map(|_| ())
        .expect_err("the canonical spelling is ignored");
        assert!(
            matches!(
                error,
                EditError::Refused {
                    reason: Refusal::Excluded,
                    ..
                }
            ),
            "{error:?}"
        );
    }

    /// Every path under `root` except temporary edit files, sorted.
    fn tree(root: &Path) -> Vec<String> {
        let mut paths = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).unwrap().filter_map(Result::ok) {
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if entry.file_type().unwrap().is_dir() {
                    paths.push(format!("{relative}/"));
                    pending.push(path);
                } else {
                    paths.push(relative);
                }
            }
        }
        paths.sort();
        paths
    }

    fn fail_write(at: usize) -> impl FnMut(usize, &Path) -> io::Result<()> {
        move |index, _| {
            if index == at {
                Err(io::Error::other("injected"))
            } else {
                Ok(())
            }
        }
    }

    /// E17-49: a rollback removes the directories the call created, deepest
    /// first, and leaves every directory that existed before untouched.
    #[test]
    fn test_a_rollback_removes_the_directories_the_call_created() {
        let fixture = Fixture::new(&[("keep/k.rs", "fn k() {}\n"), ("z.rs", "fn z() {}\n")]);
        let before = tree(&fixture.root);
        // The created file is written first, then the replace fails.
        let error = fixture
            .apply(
                vec![
                    create("keep/fresh/deep/x.rs", "fn x() {}\n"),
                    replace("z.rs", "fn z", "fn zz"),
                ],
                &mut fail_write(1),
            )
            .expect_err("second write fails");
        assert!(matches!(error, EditError::WriteFailed { .. }), "{error:?}");
        assert_eq!(tree(&fixture.root), before, "created directories removed");
        // The created file's own write fails after its directories exist.
        let error = fixture
            .apply(
                vec![
                    create("new/x.rs", "fn x() {}\n"),
                    replace("keep/k.rs", "fn k", "fn kk"),
                ],
                &mut fail_write(1),
            )
            .expect_err("second write fails");
        assert!(matches!(error, EditError::WriteFailed { .. }), "{error:?}");
        assert_eq!(tree(&fixture.root), before, "nothing is left behind");
    }

    /// E17-49: a created directory another writer put a file into is not
    /// empty and is kept; the empty ones below the call's file are removed.
    #[test]
    fn test_a_rollback_keeps_a_created_directory_that_is_no_longer_empty() {
        let fixture = Fixture::new(&[("z.rs", "fn z() {}\n")]);
        let root = fixture.root.clone();
        let error = fixture
            .apply(
                vec![
                    create("a/b/c/x.rs", "fn x() {}\n"),
                    replace("z.rs", "fn z", "fn zz"),
                ],
                &mut |index, _| {
                    if index == 1 {
                        fs::write(root.join("a/theirs.txt"), "theirs\n")?;
                        Err(io::Error::other("injected"))
                    } else {
                        Ok(())
                    }
                },
            )
            .expect_err("second write fails");
        assert!(matches!(error, EditError::WriteFailed { .. }), "{error:?}");
        assert!(!fixture.root.join("a/b").exists(), "empty ones removed");
        assert_eq!(
            fs::read_to_string(fixture.root.join("a/theirs.txt")).unwrap(),
            "theirs\n"
        );
    }

    /// E17-49: a directory that appears between validation and creation is
    /// not taken as created: the call stops, removes only what it created,
    /// and keeps the other writer's directory.
    #[test]
    fn test_a_directory_appearing_before_creation_stops_the_call() {
        let fixture = Fixture::new(&[("z.rs", "fn z() {}\n")]);
        BEFORE_DIRECTORY.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(|directory: &Path| {
                if directory.ends_with("p/q") {
                    fs::create_dir(directory).expect("planted");
                }
            }));
        });
        let result = fixture.apply(vec![create("p/q/x.rs", "fn x() {}\n")], &mut |_, _| Ok(()));
        BEFORE_DIRECTORY.with(|hook| *hook.borrow_mut() = None);
        let error = result.expect_err("the directory appeared");
        assert!(matches!(error, EditError::Changed { .. }), "{error:?}");
        assert!(
            fixture.root.join("p/q").is_dir(),
            "the other writer's directory is kept"
        );
        assert!(!fixture.root.join("p/q/x.rs").exists());
        assert_eq!(fs::read_dir(fixture.root.join("p")).unwrap().count(), 1);
    }

    /// E17-47: undoing a created file removes it.
    #[test]
    fn test_a_created_file_is_removed_when_a_later_write_fails() {
        let fixture = Fixture::new(&[("b.rs", "fn b() {}\n")]);
        let error = fixture
            .apply(
                vec![
                    create("a.rs", "fn a() {}\n"),
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
        assert!(
            !fixture.root.join("a.rs").exists(),
            "the created file is gone"
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

    /// E17-47: a file that appears at a create target after validation is
    /// never overwritten, and a created file another writer changed is not
    /// removed by the rollback.
    #[test]
    fn test_a_create_never_overwrites_a_file_that_appeared_after_validation() {
        let fixture = Fixture::new(&[("b.rs", "fn b() {}\n")]);
        let error = fixture
            .apply(vec![create("a.rs", "fn mine() {}\n")], &mut |_, path| {
                fs::write(path, "fn theirs() {}\n")
            })
            .expect_err("the path is taken");
        assert!(matches!(error, EditError::WriteFailed { .. }), "{error:?}");
        assert_eq!(
            fs::read_to_string(fixture.root.join("a.rs")).unwrap(),
            "fn theirs() {}\n"
        );

        let error = fixture
            .apply(
                vec![
                    create("a_new.rs", "fn c() {}\n"),
                    replace("b.rs", "fn b", "fn bb"),
                ],
                &mut |index, _| {
                    if index == 1 {
                        fs::write(fixture.root.join("a_new.rs"), "fn edited() {}\n")?;
                        Err(io::Error::other("injected"))
                    } else {
                        Ok(())
                    }
                },
            )
            .expect_err("second write fails");
        match error {
            EditError::WriteFailed {
                restored,
                unrestored,
                backups,
                ..
            } => {
                assert!(restored.is_empty());
                assert_eq!(unrestored, ["a_new.rs"]);
                assert!(backups.is_empty(), "a created file has no original copy");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            fs::read_to_string(fixture.root.join("a_new.rs")).unwrap(),
            "fn edited() {}\n",
            "another writer's content is kept"
        );
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
