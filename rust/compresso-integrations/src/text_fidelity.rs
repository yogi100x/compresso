//! Byte-fidelity rules for user-owned configuration text.
//!
//! Adapters render a complete target document with `\n` line endings and no
//! byte-order mark. Before a plan binds its result hash, the plan manager
//! reshapes that rendering against the bytes currently on disk so a mutation
//! never changes how the user's editor or platform encoded the file:
//!
//! - **Byte-order mark.** A leading UTF-8 BOM (`EF BB BF`) on the existing
//!   file is carried onto the target. A file without one never gains one.
//! - **Untouched lines.** Lines before the first and after the last line that
//!   Compresso changed keep their original terminator byte-for-byte, whether
//!   that is `\r\n`, `\n`, or none at all for a final line without a trailing
//!   newline.
//! - **Inserted or re-rendered lines.** Lines inside the changed region use
//!   the file's *dominant* line ending: `\r\n` when the existing file has
//!   strictly more `\r\n` terminators than bare `\n` terminators, otherwise
//!   `\n`. This is the rule for mixed files: the majority wins, a tie resolves
//!   to `\n`, and existing lines are never normalized toward the winner.
//! - **New files.** A file Compresso creates uses `\n` and no BOM on every
//!   platform, so the created bytes are deterministic.
//!
//! A lone `\r` is not a line terminator; it is preserved as ordinary content.
//! The changed region only ever contains text rendered by Compresso, so user
//! newlines inside TOML multi-line strings are never rewritten.
//!
//! Removing the owned entry is the exact inverse of adding it for TOML, which
//! is edited in place. JSON is re-rendered whole, so an uninstall of a file
//! that is still byte-for-byte what the install wrote, and whose result is
//! structurally identical to the pre-install backup, reuses the backup bytes
//! verbatim; see [`pristine_json_backup`]. Any edit made since the install,
//! including a whitespace-only one, disables that reuse so no user change is
//! ever reverted.

use std::fs;
use std::path::Path;

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";
const BOM_CHARACTER: char = '\u{feff}';
const CRLF: &str = "\r\n";
const LF: &str = "\n";
/// Upper bound for a backup considered for verbatim restoration.
const MAX_BACKUP_BYTES: u64 = 64 * 1024 * 1024;

/// Returns `bytes` without a leading UTF-8 byte-order mark.
pub(crate) fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes)
}

/// Returns `text` without a leading UTF-8 byte-order mark.
pub(crate) fn strip_bom_str(text: &str) -> &str {
    text.strip_prefix(BOM_CHARACTER).unwrap_or(text)
}

#[derive(Clone, Copy)]
struct Line<'a> {
    content: &'a str,
    terminator: &'a str,
}

fn split_lines(text: &str) -> Vec<Line<'_>> {
    text.split_inclusive('\n')
        .map(|line| {
            if let Some(content) = line.strip_suffix(CRLF) {
                Line {
                    content,
                    terminator: CRLF,
                }
            } else if let Some(content) = line.strip_suffix(LF) {
                Line {
                    content,
                    terminator: LF,
                }
            } else {
                Line {
                    content: line,
                    terminator: "",
                }
            }
        })
        .collect()
}

fn dominant_line_ending(lines: &[Line<'_>]) -> &'static str {
    let crlf = lines.iter().filter(|line| line.terminator == CRLF).count();
    let lf = lines.iter().filter(|line| line.terminator == LF).count();
    if crlf > lf { CRLF } else { LF }
}

/// Reshapes an adapter rendering so it keeps the existing file's byte-order
/// mark and line endings according to the module-level rules.
///
/// A base that is not UTF-8 is left to the format validators to reject; the
/// rendering is returned unchanged.
pub(crate) fn preserve_text_fidelity(base: &[u8], rendered: &str) -> String {
    let Ok(base_text) = std::str::from_utf8(base) else {
        return rendered.to_owned();
    };
    let has_bom = base_text.starts_with(BOM_CHARACTER);
    let base_lines = split_lines(strip_bom_str(base_text));
    let rendered_lines = split_lines(strip_bom_str(rendered));
    let dominant = dominant_line_ending(&base_lines);

    let shortest = base_lines.len().min(rendered_lines.len());
    let prefix = base_lines
        .iter()
        .zip(&rendered_lines)
        .take_while(|(base, rendered)| base.content == rendered.content)
        .count();
    let suffix = base_lines
        .iter()
        .rev()
        .zip(rendered_lines.iter().rev())
        .take(shortest - prefix)
        .take_while(|(base, rendered)| base.content == rendered.content)
        .count();

    let mut shaped = String::with_capacity(rendered.len() + rendered_lines.len() + 3);
    if has_bom {
        shaped.push(BOM_CHARACTER);
    }
    let last_rendered = rendered_lines.len().saturating_sub(1);
    let last_base = base_lines.len().saturating_sub(1);
    for (index, line) in rendered_lines.iter().enumerate() {
        let base_index = if index < prefix {
            Some(index)
        } else if index >= rendered_lines.len() - suffix {
            Some(base_lines.len() - (rendered_lines.len() - index))
        } else {
            None
        };
        let terminator = match base_index.map(|base_index| (base_index, base_lines[base_index])) {
            // An unchanged line keeps its exact terminator. Whether the file
            // ends with a newline is only inherited final-line to final-line,
            // so a formerly final line that gained successors is terminated.
            Some((base_index, base))
                if base.terminator.is_empty() == line.terminator.is_empty()
                    || (base_index == last_base && index == last_rendered) =>
            {
                base.terminator
            }
            _ if line.terminator.is_empty() => "",
            _ => dominant,
        };
        shaped.push_str(line.content);
        shaped.push_str(terminator);
    }
    shaped
}

/// Returns the pre-install backup text when restoring it verbatim is exactly
/// equivalent to writing `rendered`.
///
/// JSON adapters re-render the whole document, which cannot reproduce the
/// original indentation, mixed line endings, or other insignificant bytes.
/// The backup bytes are reused only when both of these hold:
///
/// - `current` is byte-for-byte the canonical two-space rendering of its own
///   content shaped against the backup, which is exactly what an install
///   writes. A file the user has since edited or reformatted fails this.
/// - `rendered`, the uninstall result, has the same structure, values, and
///   key order as the backup.
///
/// Together they mean the only difference between the file and its backup is
/// the owned entry and formatting Compresso itself introduced, so restoring
/// the backup cannot revert a user edit. Anything else (another key changed,
/// the owned entry is being retained, the backup is missing or is not a
/// regular file that [`crate::backups::is_backup_of`] accepts for the
/// config: one in the backup directory named for it, or one an earlier
/// version wrote beside it) returns `None` and the caller writes the
/// reshaped rendering instead. This never widens what an uninstall may
/// change: the plan manager still validates the returned text against the
/// owned paths.
pub(crate) fn pristine_json_backup(
    config_path: &Path,
    backup_path: &Path,
    backup_dir: Option<&Path>,
    current: &[u8],
    rendered: &str,
) -> Option<String> {
    if !crate::backups::is_backup_of(config_path, backup_path, backup_dir) {
        return None;
    }
    let metadata = fs::symlink_metadata(backup_path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_BACKUP_BYTES {
        return None;
    }
    let backup = String::from_utf8(fs::read(backup_path).ok()?).ok()?;
    let current = std::str::from_utf8(current).ok()?;
    let installed: serde_json::Value = serde_json::from_str(strip_bom_str(current)).ok()?;
    let mut canonical = serde_json::to_string_pretty(&installed).ok()?;
    canonical.push('\n');
    if preserve_text_fidelity(backup.as_bytes(), &canonical) != current {
        return None;
    }
    let original: serde_json::Value = serde_json::from_str(strip_bom_str(&backup)).ok()?;
    let target: serde_json::Value = serde_json::from_str(strip_bom_str(rendered)).ok()?;
    // `preserve_order` makes the compact rendering key-order sensitive, so an
    // equal rendering means the documents differ only in insignificant bytes.
    let equivalent =
        serde_json::to_string(&original).ok()? == serde_json::to_string(&target).ok()?;
    equivalent.then_some(backup)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_crlf_file_receives_crlf_for_inserted_lines() {
        let base = b"a = 1\r\nb = 2\r\n";
        let rendered = "a = 1\r\n\n[owned]\nkey = true\nb = 2\r\n";

        assert_eq!(
            preserve_text_fidelity(base, rendered).as_bytes(),
            b"a = 1\r\n\r\n[owned]\r\nkey = true\r\nb = 2\r\n"
        );
    }

    #[test]
    fn test_lf_file_is_returned_unchanged() {
        let base = b"{\n  \"a\": 1\n}\n";
        let rendered = "{\n  \"a\": 1,\n  \"b\": 2\n}\n";

        assert_eq!(preserve_text_fidelity(base, rendered), rendered);
    }

    #[test]
    fn test_whole_document_rerender_adopts_crlf_and_bom() {
        let base = b"\xEF\xBB\xBF{\r\n  \"a\": 1\r\n}\r\n";
        let rendered = "{\n  \"a\": 1,\n  \"b\": 2\n}\n";

        assert_eq!(
            preserve_text_fidelity(base, rendered).as_bytes(),
            b"\xEF\xBB\xBF{\r\n  \"a\": 1,\r\n  \"b\": 2\r\n}\r\n"
        );
    }

    #[test]
    fn test_mixed_file_keeps_each_untouched_terminator_and_inserts_majority() {
        // Two CRLF terminators against one LF: CRLF is dominant.
        let base = b"one\r\ntwo\nthree\r\n";
        let rendered = "one\ntwo\nnew\nthree\n";

        assert_eq!(
            preserve_text_fidelity(base, rendered).as_bytes(),
            b"one\r\ntwo\nnew\r\nthree\r\n"
        );
    }

    #[test]
    fn test_mixed_file_tie_inserts_lf() {
        let base = b"one\r\ntwo\n";
        let rendered = "one\ntwo\nnew\n";

        assert_eq!(
            preserve_text_fidelity(base, rendered).as_bytes(),
            b"one\r\ntwo\nnew\n"
        );
    }

    #[test]
    fn test_removing_inserted_region_restores_mixed_original_bytes() {
        let original: &[u8] = b"\xEF\xBB\xBFone\r\ntwo\nthree\r\nfour";
        let installed = preserve_text_fidelity(original, "one\ntwo\nnew\nthree\nfour\n");
        assert_eq!(
            installed.as_bytes(),
            b"\xEF\xBB\xBFone\r\ntwo\nnew\r\nthree\r\nfour"
        );

        let restored = preserve_text_fidelity(installed.as_bytes(), "one\ntwo\nthree\nfour\n");

        assert_eq!(restored.as_bytes(), original);
    }

    #[test]
    fn test_former_final_line_without_newline_is_terminated_when_lines_follow() {
        let base = b"a = 1\r\nb = 2";
        let rendered = "a = 1\nb = 2\n\n[owned]\nkey = true\n";

        assert_eq!(
            preserve_text_fidelity(base, rendered).as_bytes(),
            b"a = 1\r\nb = 2\r\n\r\n[owned]\r\nkey = true\r\n"
        );
    }

    #[test]
    fn test_lone_carriage_return_is_content_not_a_terminator() {
        let base = b"a\rb\n";
        let rendered = "a\rb\nc\n";

        assert_eq!(preserve_text_fidelity(base, rendered), "a\rb\nc\n");
    }

    #[test]
    fn test_bom_is_never_added_or_duplicated() {
        assert_eq!(preserve_text_fidelity(b"{}\n", "{}\n"), "{}\n");
        assert_eq!(
            preserve_text_fidelity(b"\xEF\xBB\xBF{}\n", "\u{feff}{}\n").as_bytes(),
            b"\xEF\xBB\xBF{}\n"
        );
        assert_eq!(strip_bom(b"\xEF\xBB\xBF{}"), b"{}");
        assert_eq!(strip_bom(b"{}"), b"{}");
        assert_eq!(strip_bom_str("\u{feff}{}"), "{}");
    }

    #[test]
    fn test_non_utf8_base_leaves_rendering_unchanged() {
        assert_eq!(
            preserve_text_fidelity(&[0xff, 0xfe, b'\r', b'\n'], "{}\n"),
            "{}\n"
        );
    }

    #[test]
    fn test_pristine_backup_is_reused_only_for_untouched_equivalent_json_beside_config() {
        let directory = tempdir().expect("temporary directory");
        let config = directory.path().join("mcp.json");
        let backup = directory
            .path()
            .join("mcp.json.compresso-backup-2026-01-01T00-00-00Z");
        let original = "\u{feff}{\r\n    \"b\": 1,\n\t\"a\": [ 1,2 ]\r\n}";
        fs::write(&backup, original).expect("backup written");
        // What an install writes: the canonical rendering shaped to the backup.
        let installed = preserve_text_fidelity(
            original.as_bytes(),
            "{\n  \"b\": 1,\n  \"a\": [\n    1,\n    2\n  ],\n  \"owned\": true\n}\n",
        );
        let uninstalled = "{\n  \"b\": 1,\n  \"a\": [\n    1,\n    2\n  ]\n}\n";

        assert_eq!(
            pristine_json_backup(&config, &backup, None, installed.as_bytes(), uninstalled)
                .as_deref(),
            Some(original)
        );
        // A whitespace-only user edit since the install disables the reuse.
        let reformatted = installed.replace("  \"owned\"", "\t\"owned\"");
        assert_ne!(reformatted, installed);
        assert_eq!(
            pristine_json_backup(&config, &backup, None, reformatted.as_bytes(), uninstalled),
            None
        );
        // A changed value, a changed key order, and a backup outside the
        // config directory or naming scheme all fall back to the rendering.
        for different in ["{\"b\":2,\"a\":[1,2]}", "{\"a\":[1,2],\"b\":1}"] {
            assert_eq!(
                pristine_json_backup(&config, &backup, None, installed.as_bytes(), different),
                None
            );
        }
        let foreign = directory.path().join("other.json.compresso-backup-x");
        fs::write(&foreign, original).expect("foreign backup written");
        assert_eq!(
            pristine_json_backup(&config, &foreign, None, installed.as_bytes(), uninstalled),
            None
        );
        assert_eq!(
            pristine_json_backup(
                &config,
                &directory.path().join("mcp.json.compresso-backup-missing"),
                None,
                installed.as_bytes(),
                uninstalled
            ),
            None
        );
    }
}
