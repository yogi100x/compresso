//! Where the plan manager keeps the copy of a configuration file it changes
//! (E17-53, D118).
//!
//! A copy is written under Compresso's own data directory, in
//! [`BACKUP_DIR_NAME`], never beside the file: a project's `.mcp.json` can
//! hold other servers' tokens, and a copy in the project tree is not ignored
//! by git in most repositories. Each copy is named
//! `<32 hex digits of the BLAKE3 hash of the target path>-<UTC timestamp>`,
//! written once with mode 0600 in a directory of mode 0700, and only the
//! newest [`KEPT_PER_TARGET`] copies of one target are kept.
//!
//! Versions before E17-53 wrote `<file>.compresso-backup-<timestamp>` beside
//! the file and named it in the receipt. Those copies are still read (a
//! verbatim JSON restore on uninstall) when present, are never required, and
//! are never deleted: [`legacy_backups`] lists them so the CLI can warn.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::plan::{IntegrationError, PLAN_TTL_SECONDS};
use crate::receipt::InstallReceipt;

/// Directory inside the data directory that holds integration backups.
pub const BACKUP_DIR_NAME: &str = "integration-backups";

/// Copies kept per target file. The newest is the one the active receipt
/// names (an install writes it); the next covers an uninstall that kept a
/// user-modified entry, whose receipt stays active while the uninstall wrote
/// a newer copy; the third leaves one step of history for a manual recovery.
/// Copies named by the plan being committed or by its prior receipt are
/// never pruned, whatever their age.
pub const KEPT_PER_TARGET: usize = 3;

/// Characters in the target-path hash prefix of a backup name.
const KEY_CHARS: usize = 32;
/// `YYYY-MM-DDTHH-MM-SSZ`.
const TIMESTAMP_CHARS: usize = 20;
/// Name infix of a copy an earlier version wrote beside the file.
const LEGACY_INFIX: &str = ".compresso-backup-";

/// The backup directory inside a Compresso data directory.
pub fn backup_dir_in(data_dir: &Path) -> PathBuf {
    data_dir.join(BACKUP_DIR_NAME)
}

/// Hash prefix naming one target's copies. Hashing the platform's own path
/// bytes keeps the name free of separators, drive letters, and characters a
/// file name cannot hold on any platform.
fn target_key(config_path: &Path) -> String {
    let hash = blake3::hash(config_path.as_os_str().as_encoded_bytes());
    hash.to_hex()[..KEY_CHARS].to_owned()
}

fn timestamp(seconds: i64) -> Result<String, IntegrationError> {
    Ok(DateTime::<Utc>::from_timestamp(seconds, 0)
        .ok_or_else(|| IntegrationError::InvalidRequest {
            reason: "plan timestamp is outside the supported range".to_owned(),
        })?
        .format("%Y-%m-%dT%H-%M-%SZ")
        .to_string())
}

/// A free backup path for `config_path` in `backup_dir`. One-second
/// timestamps can collide for back-to-back plans, so the next free second is
/// taken rather than ever overwriting a copy.
pub(crate) fn planned_path(
    backup_dir: &Path,
    config_path: &Path,
    now: i64,
) -> Result<PathBuf, IntegrationError> {
    if config_path.file_name().is_none() {
        return Err(IntegrationError::InvalidRequest {
            reason: "config_path must identify a file".to_owned(),
        });
    }
    let key = target_key(config_path);
    for offset in 0..=PLAN_TTL_SECONDS {
        let candidate =
            backup_dir.join(format!("{key}-{}", timestamp(now.saturating_add(offset))?));
        if fs::symlink_metadata(&candidate).is_err() {
            return Ok(candidate);
        }
    }
    Err(IntegrationError::InvalidRequest {
        reason: "no collision-free timestamped backup path is available".to_owned(),
    })
}

/// Whether `name` is exactly a backup name for the target hashed to `key`.
fn is_own_name(name: &str, key: &str) -> bool {
    let Some(stamp) = name
        .strip_prefix(key)
        .and_then(|rest| rest.strip_prefix('-'))
    else {
        return false;
    };
    stamp.len() == TIMESTAMP_CHARS
        && stamp.bytes().enumerate().all(|(index, byte)| match index {
            4 | 7 | 13 | 16 => byte == b'-',
            10 => byte == b'T',
            19 => byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
}

/// Whether `backup_path` is a copy an earlier version wrote beside
/// `config_path` (`<file>.compresso-backup-<timestamp>` in its directory).
pub(crate) fn is_legacy_beside(config_path: &Path, backup_path: &Path) -> bool {
    let (Some(config_name), Some(backup_name)) = (
        config_path.file_name().and_then(|name| name.to_str()),
        backup_path.file_name().and_then(|name| name.to_str()),
    ) else {
        return false;
    };
    backup_path.parent() == config_path.parent()
        && backup_name
            .strip_prefix(config_name)
            .is_some_and(|rest| rest.starts_with(LEGACY_INFIX) && rest.len() > LEGACY_INFIX.len())
}

/// Whether `backup_path` may be read as the copy of `config_path`: one in
/// `backup_dir` named for it, or one an earlier version wrote beside it.
pub(crate) fn is_backup_of(
    config_path: &Path,
    backup_path: &Path,
    backup_dir: Option<&Path>,
) -> bool {
    if is_legacy_beside(config_path, backup_path) {
        return true;
    }
    let Some(backup_dir) = backup_dir else {
        return false;
    };
    backup_path.parent() == Some(backup_dir)
        && backup_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| is_own_name(name, &target_key(config_path)))
}

/// Creates the backup directory and keeps it private to the user.
pub(crate) fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let metadata = fs::symlink_metadata(dir)?;
        if !metadata.file_type().is_dir() {
            return Err(std::io::Error::other(
                "the integration backup directory is not a directory",
            ));
        }
        if metadata.permissions().mode() & 0o777 != 0o700 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

/// Removes all but the newest [`KEPT_PER_TARGET`] copies of `config_path` in
/// the directory holding `written`, never one named in `protect`. Only
/// regular files whose names are exactly this target's backup names are
/// considered. Best effort: the change is already committed, so a failure
/// here is ignored.
pub(crate) fn prune(config_path: &Path, written: &Path, protect: &[&Path]) {
    let Some(dir) = written.parent() else {
        return;
    };
    let key = target_key(config_path);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut names = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| is_own_name(name, &key))
        .collect::<Vec<_>>();
    // Fixed-width UTC timestamps sort chronologically as text.
    names.sort_unstable_by(|left, right| right.cmp(left));
    for name in names.into_iter().skip(KEPT_PER_TARGET) {
        let path = dir.join(&name);
        if protect.iter().any(|kept| *kept == path) {
            continue;
        }
        let _ignored = fs::remove_file(path);
    }
}

/// Copies that versions before E17-53 wrote beside a file and that a
/// receipt of ours names, which still exist as regular files, for the
/// targets `relevant` selects; sorted, each once. Compresso never deletes
/// them: no receipt recorded a hash of a copy, so nothing proves that a file
/// at that path is still the copy Compresso wrote.
pub fn legacy_backups(
    receipts: &[InstallReceipt],
    relevant: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    receipts
        .iter()
        .filter(|receipt| relevant(&receipt.config_path))
        .map(|receipt| (&receipt.config_path, PathBuf::from(&receipt.backup_path)))
        .filter(|(config, backup)| is_legacy_beside(config, backup))
        .filter(|(_, backup)| {
            fs::symlink_metadata(backup).is_ok_and(|metadata| metadata.file_type().is_file())
        })
        .map(|(_, backup)| backup)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// A backup directory private to the calling test thread, removed when the
/// thread ends, so managers created by one test share it (as one data
/// directory does across processes) and no test writes outside its own
/// temporary directories.
#[cfg(test)]
pub(crate) fn test_dir() -> PathBuf {
    thread_local! {
        static DIR: tempfile::TempDir = tempfile::tempdir().expect("test backup directory");
    }
    DIR.with(|dir| dir.path().join(BACKUP_DIR_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backup_names_hold_only_portable_characters_for_any_target_path() {
        let directory = tempfile::tempdir().expect("temporary directory");
        // A Windows-shaped path is just bytes here; on every platform its
        // name must carry no separator, drive colon, or other path text.
        for target in [
            PathBuf::from(r"C:\Users\someone\project\.mcp.json"),
            PathBuf::from("/home/someone/project/.mcp.json"),
            PathBuf::from("/tmp/a b/ünïcode/.mcp.json"),
        ] {
            let planned = planned_path(directory.path(), &target, 1_790_000_000).expect("path");
            assert_eq!(planned.parent(), Some(directory.path()));
            let name = planned
                .file_name()
                .and_then(|name| name.to_str())
                .expect("name");
            assert!(
                name.bytes()
                    .all(|byte| byte.is_ascii_hexdigit() || matches!(byte, b'-' | b'T' | b'Z')),
                "{name}"
            );
            assert!(is_backup_of(&target, &planned, Some(directory.path())));
        }
        let one = planned_path(directory.path(), Path::new("/p/.mcp.json"), 0).expect("path");
        let other = planned_path(directory.path(), Path::new("/q/.mcp.json"), 0).expect("path");
        assert_ne!(one, other, "distinct targets get distinct names");
        assert!(!is_backup_of(
            Path::new("/q/.mcp.json"),
            &one,
            Some(directory.path())
        ));
        assert!(!is_backup_of(Path::new("/p/.mcp.json"), &one, None));
    }

    #[cfg(windows)]
    #[test]
    fn test_windows_target_paths_name_backups_in_the_backup_directory() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let target = PathBuf::from(r"C:\Users\someone\project\.mcp.json");
        let planned = planned_path(directory.path(), &target, 1_790_000_000).expect("path");
        fs::write(&planned, b"{}").expect("a backup name is a valid Windows file name");
        assert!(is_backup_of(&target, &planned, Some(directory.path())));
        assert!(is_legacy_beside(
            &target,
            Path::new(r"C:\Users\someone\project\.mcp.json.compresso-backup-2026-01-01T00-00-00Z")
        ));
    }

    #[test]
    fn test_legacy_names_are_recognised_only_beside_their_own_file() {
        let config = Path::new("/p/.mcp.json");
        assert!(is_legacy_beside(
            config,
            Path::new("/p/.mcp.json.compresso-backup-2026-01-01T00-00-00Z")
        ));
        assert!(!is_legacy_beside(
            config,
            Path::new("/q/.mcp.json.compresso-backup-2026-01-01T00-00-00Z")
        ));
        assert!(!is_legacy_beside(
            config,
            Path::new("/p/other.json.compresso-backup-2026-01-01T00-00-00Z")
        ));
        assert!(!is_legacy_beside(
            config,
            Path::new("/p/.mcp.json.compresso-backup-")
        ));
    }

    #[test]
    fn test_prune_keeps_the_newest_copies_and_protected_ones_and_nothing_foreign() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = Path::new("/p/.mcp.json");
        let paths = (0..6)
            .map(|step| {
                let path =
                    planned_path(directory.path(), config, 1_000 + step * 100).expect("path");
                fs::write(&path, step.to_string()).expect("copy");
                path
            })
            .collect::<Vec<_>>();
        let key = target_key(config);
        let foreign = [
            directory.path().join(format!("{key}-notes.txt")),
            directory.path().join("unrelated"),
            planned_path(directory.path(), Path::new("/q/.mcp.json"), 0).expect("path"),
        ];
        for path in &foreign {
            fs::write(path, "keep").expect("foreign file");
        }

        prune(config, &paths[5], &[&paths[0]]);

        for (index, path) in paths.iter().enumerate() {
            assert_eq!(
                path.exists(),
                index == 0 || index >= 3,
                "copy {index} kept only when protected or among the newest three"
            );
        }
        for path in &foreign {
            assert!(
                path.exists(),
                "{} is not this target's copy",
                path.display()
            );
        }
    }
}
