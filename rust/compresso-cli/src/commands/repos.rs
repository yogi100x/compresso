use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use clap::{Args, Subcommand};
use compresso_store::{RepoRecord, RepoRemovalPreview, Store, StoreError};
use serde_json::{Value, json};

use super::{CliExitCode, OutputArgs, write_error};

#[derive(Debug, Subcommand)]
pub enum ReposCommand {
    /// List every repository currently approved for local indexing and MCP access.
    List(ReposListArgs),
    /// Remove one exact repository approval and all of its stored index metadata.
    Remove(ReposRemoveArgs),
}

#[derive(Args, Debug, Default)]
pub struct ReposListArgs {
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Args, Debug)]
pub struct ReposRemoveArgs {
    /// Exact approved repository root. The stored absolute path still works after deletion.
    #[arg(value_name = "ROOT")]
    pub root: PathBuf,
    #[command(flatten)]
    pub output: OutputArgs,
    /// Preview the approval and metadata removal without writing the Store.
    #[arg(long)]
    pub dry_run: bool,
    /// Remove without an interactive confirmation prompt.
    #[arg(long)]
    pub yes: bool,
}

pub(crate) fn execute(
    command: ReposCommand,
    store: &Store,
    cwd: &Path,
    input: &mut dyn Read,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    let json = command.json();
    let result = match command {
        ReposCommand::List(arguments) => execute_list(arguments, store, output),
        ReposCommand::Remove(arguments) => {
            execute_remove(arguments, store, cwd, input, output, errors)
        }
    };
    match result {
        Ok(()) => CliExitCode::Success,
        Err(error) => {
            let _ = write_error(errors, error.code, &error.message, json);
            CliExitCode::Failure
        }
    }
}

fn execute_list(
    arguments: ReposListArgs,
    store: &Store,
    output: &mut impl Write,
) -> Result<(), ReposCliError> {
    let repositories = store
        .approved_repositories()
        .map_err(ReposCliError::store)?;
    if arguments.output.json {
        return write_json(
            output,
            &json!({
                "schema_version": 1,
                "data": {
                    "action": "list",
                    "repositories": repositories.iter().map(repository_json).collect::<Vec<_>>(),
                },
            }),
        )
        .map_err(ReposCliError::io);
    }

    if repositories.is_empty() {
        writeln!(output, "No approved repositories.").map_err(ReposCliError::io)?;
        return Ok(());
    }
    writeln!(output, "Approved repositories:").map_err(ReposCliError::io)?;
    for repository in repositories {
        let status = if repository.last_indexed_at.is_some() {
            "indexed"
        } else {
            "not indexed"
        };
        writeln!(output, "{} ({status})", repository.root.display()).map_err(ReposCliError::io)?;
    }
    Ok(())
}

fn execute_remove(
    arguments: ReposRemoveArgs,
    store: &Store,
    cwd: &Path,
    input: &mut dyn Read,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> Result<(), ReposCliError> {
    let preview = resolve_exact_repository(store, &arguments.root, cwd)?;
    let repository = preview.repository();

    if arguments.dry_run {
        return render_remove(output, repository, true, false, arguments.output.json);
    }

    if arguments.output.json && !arguments.yes {
        render_remove(output, repository, false, false, true)?;
        return Err(ReposCliError::confirmation_required(
            "JSON removals require --yes after reviewing the returned repository",
        ));
    }

    if !arguments.output.json {
        writeln!(
            output,
            "Would remove approval and indexed metadata for {}.",
            repository.root.display()
        )
        .map_err(ReposCliError::io)?;
    }

    if !arguments.yes {
        write!(errors, "Remove this repository? [y/N] ").map_err(ReposCliError::io)?;
        errors.flush().map_err(ReposCliError::io)?;
        match read_confirmation(input).map_err(ReposCliError::io)? {
            Confirmation::Approved => {}
            Confirmation::Declined => {
                writeln!(output, "Cancelled; repository remains approved.")
                    .map_err(ReposCliError::io)?;
                return Ok(());
            }
            Confirmation::Unavailable => {
                return Err(ReposCliError::confirmation_required(
                    "interactive confirmation is unavailable; rerun with --yes",
                ));
            }
        }
    }

    let removed = store
        .remove_repo_if_unchanged(&preview)
        .map_err(ReposCliError::store)?;
    render_remove(output, &removed, false, true, arguments.output.json)
}

fn resolve_exact_repository(
    store: &Store,
    requested: &Path,
    cwd: &Path,
) -> Result<RepoRemovalPreview, ReposCliError> {
    let absolute = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        cwd.join(requested)
    };

    // Prefer the normalized path exactly as it was approved. An approved directory may have
    // disappeared or been replaced by a symlink since indexing; canonicalizing first could then
    // select a different approved repository and remove the wrong metadata.
    let contains_parent = absolute
        .components()
        .any(|component| matches!(component, Component::ParentDir));
    let normalized = normalize_absolute(&absolute).ok_or_else(ReposCliError::invalid_root)?;
    if !contains_parent
        && let Some(preview) = store
            .prepare_repo_removal(&normalized)
            .map_err(ReposCliError::store)?
    {
        return Ok(preview);
    }

    // If no exact stored root exists, accept an on-disk alias of an approved repository.
    // Paths containing `..` always take this filesystem-aware route because lexical folding can
    // differ from filesystem resolution when an earlier component is a symlink.
    if let Ok(canonical) = fs::canonicalize(&absolute) {
        return store
            .prepare_repo_removal(&canonical)
            .map_err(ReposCliError::store)?
            .ok_or_else(ReposCliError::not_found);
    }

    Err(ReposCliError::not_found())
}

fn normalize_absolute(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
        }
    }
    Some(normalized)
}

fn render_remove(
    output: &mut impl Write,
    repository: &RepoRecord,
    dry_run: bool,
    removed: bool,
    json_output: bool,
) -> Result<(), ReposCliError> {
    if json_output {
        return write_json(
            output,
            &json!({
                "schema_version": 1,
                "data": {
                    "action": "remove",
                    "dry_run": dry_run,
                    "removed": removed,
                    "repository": repository_json(repository),
                },
            }),
        )
        .map_err(ReposCliError::io);
    }
    if dry_run {
        writeln!(
            output,
            "Would remove approval and indexed metadata for {}.",
            repository.root.display()
        )
        .map_err(ReposCliError::io)?;
    } else if removed {
        writeln!(
            output,
            "Removed approval and indexed metadata for {}.",
            repository.root.display()
        )
        .map_err(ReposCliError::io)?;
    }
    Ok(())
}

fn repository_json(repository: &RepoRecord) -> Value {
    json!({
        "approved_at": repository.approved_at,
        "id": repository.id,
        "indexed": repository.last_indexed_at.is_some(),
        "last_indexed_at": repository.last_indexed_at,
        "parser_version": repository.parser_version,
        "root": repository.root,
        "schema_version": repository.schema_version,
    })
}

fn write_json(output: &mut impl Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *output, value).map_err(std::io::Error::other)?;
    writeln!(output)
}

enum Confirmation {
    Approved,
    Declined,
    Unavailable,
}

fn read_confirmation(input: &mut dyn Read) -> Result<Confirmation, std::io::Error> {
    let mut bytes = Vec::new();
    while bytes.len() < 64 {
        let mut byte = [0_u8; 1];
        match input.read(&mut byte)? {
            0 => break,
            1 => {
                bytes.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            _ => unreachable!("one-byte read returned more than one byte"),
        }
    }
    if bytes.is_empty() {
        return Ok(Confirmation::Unavailable);
    }
    let first_line = String::from_utf8_lossy(&bytes);
    let answer = first_line.lines().next().unwrap_or_default().trim();
    if answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes") {
        Ok(Confirmation::Approved)
    } else {
        Ok(Confirmation::Declined)
    }
}

struct ReposCliError {
    code: &'static str,
    message: String,
}

impl ReposCliError {
    fn confirmation_required(message: &str) -> Self {
        Self {
            code: "confirmation_required",
            message: message.to_owned(),
        }
    }

    fn invalid_root() -> Self {
        Self {
            code: "invalid_arguments",
            message: "repository root must resolve to an absolute path".to_owned(),
        }
    }

    fn not_found() -> Self {
        Self {
            code: "repository_not_found",
            message: "no approved repository exactly matches that root; run `compresso repos list`"
                .to_owned(),
        }
    }

    fn store(error: StoreError) -> Self {
        match error {
            StoreError::RepositoryApprovalChanged { .. } => Self {
                code: "repository_changed",
                message: "repository approval changed after the removal preview; nothing was removed; rerun `compresso repos remove` to review the current approval"
                    .to_owned(),
            },
            StoreError::NonUtf8Path { .. } | StoreError::RepositoryRootNotAbsolute { .. } => {
                Self::invalid_root()
            }
            _ => Self {
                code: "internal",
                message: "local repository approval operation failed".to_owned(),
            },
        }
    }

    fn io(_error: std::io::Error) -> Self {
        Self {
            code: "internal",
            message: "failed to write repository command output".to_owned(),
        }
    }
}

impl ReposCommand {
    pub(crate) fn json(&self) -> bool {
        match self {
            Self::List(arguments) => arguments.output.json,
            Self::Remove(arguments) => arguments.output.json,
        }
    }
}
