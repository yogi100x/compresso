//! Recall sources, their verification status, and bounded transcript discovery.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Upper bound on directory entries inspected while discovering one source.
const MAX_DISCOVERY_ENTRIES: usize = 50_000;
/// Deepest directory level searched below a Codex sessions root
/// (`YYYY/MM/DD/rollout-*.jsonl`).
const CODEX_MAX_DEPTH: usize = 4;

/// A coding client whose local session transcripts can be named for recall.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RecallSource {
    ClaudeCode,
    Codex,
    Cursor,
}

/// Whether a source's on-disk transcript format has been verified.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceSupport {
    /// The transcript location and line shape are verified and shape-probed.
    Supported,
    /// The format is unverified; the source is reported and never read.
    Unsupported { reason: &'static str },
}

impl RecallSource {
    /// Every source name accepted by the CLI and MCP boundaries.
    pub const ALL: [Self; 3] = [Self::ClaudeCode, Self::Codex, Self::Cursor];

    /// Stable kebab-case name shared with the integration commands.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
        }
    }

    /// Parses a stable source name.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|source| source.as_str() == value)
    }

    /// Reports whether this source's transcript format is verified.
    pub const fn support(self) -> SourceSupport {
        match self {
            Self::ClaudeCode | Self::Codex => SourceSupport::Supported,
            Self::Cursor => SourceSupport::Unsupported {
                reason: "Cursor session storage has no verified public transcript format; it is not read.",
            },
        }
    }

    /// Directory that holds this source's transcripts under `home`.
    pub(crate) fn transcript_root(self, home: &Path) -> Option<PathBuf> {
        match self {
            Self::ClaudeCode => Some(home.join(".claude").join("projects")),
            Self::Codex => Some(home.join(".codex").join("sessions")),
            Self::Cursor => None,
        }
    }
}

/// Transcript files found for one source, newest first.
pub(crate) struct Discovery {
    pub(crate) root_exists: bool,
    pub(crate) files: Vec<PathBuf>,
    /// True when more candidates existed than the caller's bounds admit.
    pub(crate) capped: bool,
}

/// Lists transcript files for `source` without following symbolic links.
pub(crate) fn discover(source: RecallSource, home: &Path, max_files: usize) -> Discovery {
    let Some(root) = source.transcript_root(home) else {
        return Discovery {
            root_exists: false,
            files: Vec::new(),
            capped: false,
        };
    };
    if !is_real_directory(&root) {
        return Discovery {
            root_exists: false,
            files: Vec::new(),
            capped: false,
        };
    }
    let mut walk = Walk {
        candidates: Vec::new(),
        remaining_entries: MAX_DISCOVERY_ENTRIES,
        capped: false,
    };
    match source {
        // `<root>/<encoded-cwd>/<session-id>.jsonl`
        RecallSource::ClaudeCode => walk.visit(&root, 1, &|name| name.ends_with(".jsonl")),
        // `<root>/[YYYY/MM/DD/]rollout-<timestamp>-<id>.jsonl`
        RecallSource::Codex => walk.visit(&root, CODEX_MAX_DEPTH, &|name| {
            name.starts_with("rollout-") && name.ends_with(".jsonl")
        }),
        RecallSource::Cursor => {}
    }
    let Walk {
        mut candidates,
        mut capped,
        ..
    } = walk;
    candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    if candidates.len() > max_files {
        candidates.truncate(max_files);
        capped = true;
    }
    Discovery {
        root_exists: true,
        files: candidates.into_iter().map(|(_, path)| path).collect(),
        capped,
    }
}

struct Walk {
    candidates: Vec<(SystemTime, PathBuf)>,
    remaining_entries: usize,
    capped: bool,
}

impl Walk {
    fn visit(&mut self, directory: &Path, remaining_depth: usize, accept: &dyn Fn(&str) -> bool) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            if self.remaining_entries == 0 {
                self.capped = true;
                return;
            }
            self.remaining_entries -= 1;
            let path = entry.path();
            // `symlink_metadata` never follows links, so a link planted inside a
            // transcript directory cannot redirect recall elsewhere.
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                if remaining_depth > 0 {
                    self.visit(&path, remaining_depth - 1, accept);
                }
            } else if metadata.is_file() && entry.file_name().to_str().is_some_and(accept) {
                let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                self.candidates.push((modified, path));
            }
        }
    }
}

fn is_real_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_names_round_trip() {
        for source in RecallSource::ALL {
            assert_eq!(RecallSource::parse(source.as_str()), Some(source));
        }
        assert_eq!(RecallSource::parse("claude-desktop"), None);
        assert_eq!(RecallSource::parse(""), None);
    }

    #[test]
    fn test_cursor_is_reported_unsupported_and_has_no_root() {
        assert!(matches!(
            RecallSource::Cursor.support(),
            SourceSupport::Unsupported { .. }
        ));
        assert_eq!(
            RecallSource::Cursor.transcript_root(Path::new("/home/example")),
            None
        );
    }

    #[test]
    fn test_discover_lists_only_matching_regular_files() {
        let home = tempfile::tempdir().expect("temporary home should be created");
        let project = home.path().join(".claude/projects/-work-repo");
        fs::create_dir_all(project.join("nested")).expect("project directory should be created");
        fs::write(project.join("a.jsonl"), "{}\n").expect("transcript should be written");
        fs::write(project.join("notes.txt"), "x").expect("other file should be written");
        fs::write(project.join("nested/deep.jsonl"), "{}\n").expect("deep file should be written");
        let day = home.path().join(".codex/sessions/2026/01/02");
        fs::create_dir_all(&day).expect("codex directory should be created");
        fs::write(day.join("rollout-2026-01-02T00-00-00-abc.jsonl"), "{}\n")
            .expect("rollout should be written");
        fs::write(day.join("history.jsonl"), "{}\n").expect("other jsonl should be written");

        let claude = discover(RecallSource::ClaudeCode, home.path(), 10);
        let codex = discover(RecallSource::Codex, home.path(), 10);
        let cursor = discover(RecallSource::Cursor, home.path(), 10);

        assert_eq!(claude.files, vec![project.join("a.jsonl")]);
        assert_eq!(
            codex.files,
            vec![day.join("rollout-2026-01-02T00-00-00-abc.jsonl")]
        );
        assert!(claude.root_exists && codex.root_exists);
        assert!(!cursor.root_exists && cursor.files.is_empty());
    }

    #[test]
    fn test_discover_caps_file_count() {
        let home = tempfile::tempdir().expect("temporary home should be created");
        let project = home.path().join(".claude/projects/-work-repo");
        fs::create_dir_all(&project).expect("project directory should be created");
        for index in 0..5 {
            fs::write(project.join(format!("{index}.jsonl")), "{}\n")
                .expect("transcript should be written");
        }

        let discovery = discover(RecallSource::ClaudeCode, home.path(), 2);

        assert_eq!(discovery.files.len(), 2);
        assert!(discovery.capped);
    }

    #[cfg(unix)]
    #[test]
    fn test_discover_never_follows_symlinks() {
        let home = tempfile::tempdir().expect("temporary home should be created");
        let outside = tempfile::tempdir().expect("outside directory should be created");
        fs::write(outside.path().join("leak.jsonl"), "{}\n").expect("outside file should exist");
        let projects = home.path().join(".claude/projects");
        fs::create_dir_all(projects.join("-work-repo")).expect("projects should be created");
        std::os::unix::fs::symlink(outside.path(), projects.join("linked-dir"))
            .expect("directory link should be created");
        std::os::unix::fs::symlink(
            outside.path().join("leak.jsonl"),
            projects.join("-work-repo/linked.jsonl"),
        )
        .expect("file link should be created");

        let discovery = discover(RecallSource::ClaudeCode, home.path(), 10);

        assert!(discovery.files.is_empty());
    }
}
