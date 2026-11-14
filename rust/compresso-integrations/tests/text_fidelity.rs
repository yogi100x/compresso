//! Byte-level line-ending and byte-order-mark fidelity across every adapter.
//!
//! Fixtures are built from inline byte literals: the repository normalizes
//! checked-in text to LF, so CRLF fixture files would not survive a checkout.

use std::fs;
use std::path::{Path, PathBuf};

use compresso_integrations::{
    ClaudeCodeDiscovery, ClaudeCodeIntegration, CodexAdapter, CodexPlatform, CodexVersionEvidence,
    CursorCliEvidence, CursorDiscovery, CursorIntegration, CursorPlatform, FileReceiptStore,
    InstallCtx, InstallReceipt, Integration, PlanApproval, PlanManager, Scope,
};
use tempfile::{TempDir, tempdir};

const BOM: &[u8] = b"\xEF\xBB\xBF";

const JSON_LINES: &[&str] = &[
    "{",
    "  \"theme\": \"night\",",
    "  \"mcpServers\": {",
    "    \"other\": {",
    "      \"command\": \"/opt/other\"",
    "    }",
    "  },",
    "  \"trailing\": [",
    "    1,",
    "    2",
    "  ]",
    "}",
];

const TOML_LINES: &[&str] = &[
    "# user comment",
    "model = \"gpt-5\"",
    "",
    "[mcp_servers.other]",
    "command = \"/opt/other\"",
    "",
    "[features]",
    "experimental = true # keep me",
];

#[derive(Clone, Copy, Debug)]
enum Client {
    ClaudeCode,
    Codex,
    Cursor,
}

impl Client {
    const ALL: [Self; 3] = [Self::ClaudeCode, Self::Codex, Self::Cursor];

    fn lines(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeCode | Self::Cursor => JSON_LINES,
            Self::Codex => TOML_LINES,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Endings {
    Lf,
    CrLf,
    /// Two of every three terminators are CRLF, so CRLF is dominant.
    MixedMostlyCrLf,
    /// Two of every three terminators are LF, so LF is dominant.
    MixedMostlyLf,
}

impl Endings {
    fn terminator(self, line: usize) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::MixedMostlyCrLf if line % 3 == 1 => "\n",
            Self::MixedMostlyCrLf => "\r\n",
            Self::MixedMostlyLf if line % 3 == 1 => "\r\n",
            Self::MixedMostlyLf => "\n",
        }
    }

    fn dominant(self) -> &'static str {
        match self {
            Self::Lf | Self::MixedMostlyLf => "\n",
            Self::CrLf | Self::MixedMostlyCrLf => "\r\n",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Encoding {
    endings: Endings,
    bom: bool,
    final_newline: bool,
}

const ENCODINGS: &[Encoding] = &[
    Encoding {
        endings: Endings::Lf,
        bom: false,
        final_newline: true,
    },
    Encoding {
        endings: Endings::CrLf,
        bom: false,
        final_newline: true,
    },
    Encoding {
        endings: Endings::CrLf,
        bom: true,
        final_newline: true,
    },
    Encoding {
        endings: Endings::Lf,
        bom: true,
        final_newline: false,
    },
    Encoding {
        endings: Endings::MixedMostlyCrLf,
        bom: false,
        final_newline: true,
    },
    Encoding {
        endings: Endings::MixedMostlyLf,
        bom: true,
        final_newline: true,
    },
    Encoding {
        endings: Endings::MixedMostlyCrLf,
        bom: true,
        final_newline: false,
    },
];

fn encode(lines: &[&str], encoding: Encoding) -> Vec<u8> {
    let mut bytes = Vec::new();
    if encoding.bom {
        bytes.extend_from_slice(BOM);
    }
    for (index, line) in lines.iter().enumerate() {
        bytes.extend_from_slice(line.as_bytes());
        if index + 1 < lines.len() || encoding.final_newline {
            bytes.extend_from_slice(encoding.endings.terminator(index).as_bytes());
        }
    }
    bytes
}

/// Splits bytes into `(content, terminator)` pairs without normalizing.
fn split_lines(bytes: &[u8]) -> Vec<(&[u8], &'static str)> {
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .map(|line| {
            if let Some(content) = line.strip_suffix(b"\r\n") {
                (content, "\r\n")
            } else if let Some(content) = line.strip_suffix(b"\n") {
                (content, "\n")
            } else {
                (line, "")
            }
        })
        .collect()
}

struct Harness {
    directory: TempDir,
    client: Client,
    adapter: Box<dyn Integration>,
    config_path: PathBuf,
    compresso_binary: PathBuf,
}

impl Harness {
    fn new(client: Client) -> Self {
        let directory = tempdir().expect("temporary integration root");
        let home = directory.path().join("home");
        let bin = directory.path().join("bin");
        fs::create_dir_all(&home).expect("temporary home created");
        let compresso_binary = bin.join(executable_name("compresso"));
        write_executable(&compresso_binary);

        let (adapter, config_path): (Box<dyn Integration>, _) = match client {
            Client::ClaudeCode => {
                write_executable(&bin.join(executable_name("claude")));
                let adapter = ClaudeCodeIntegration::new(
                    home.clone(),
                    ClaudeCodeDiscovery::new([bin.clone()], Some("Claude Code 1.2.3".to_owned())),
                )
                .with_compresso_version("0.1.0");
                let canonical_home = fs::canonicalize(&home).expect("canonical temporary home");
                (Box::new(adapter), canonical_home.join(".claude.json"))
            }
            Client::Codex => {
                let client_binary = bin.join(executable_name("codex"));
                write_executable(&client_binary);
                let evidence = CodexVersionEvidence::from_output(&client_binary, "codex 1.2.3")
                    .expect("Codex version evidence");
                let adapter = CodexAdapter::with_platform(
                    &home,
                    vec![bin.clone()],
                    Some(evidence),
                    "0.1.0",
                    codex_platform(),
                )
                .expect("Codex adapter");
                let config_path = adapter.config_path();
                (Box::new(adapter), config_path)
            }
            Client::Cursor => {
                let client_binary = bin.join(executable_name("cursor"));
                write_executable(&client_binary);
                let adapter = CursorIntegration::new(
                    &home,
                    CursorDiscovery::new(
                        cursor_platform(),
                        [CursorCliEvidence::new(&client_binary, "Cursor 1.2.3")],
                        [],
                    ),
                )
                .with_compresso_version("0.1.0");
                let config_path = adapter
                    .config_path(&Scope::User)
                    .expect("safe Cursor config path");
                (Box::new(adapter), config_path)
            }
        };
        fs::create_dir_all(config_path.parent().expect("config parent"))
            .expect("config parent created");

        Self {
            directory,
            client,
            adapter,
            config_path,
            compresso_binary,
        }
    }

    fn with_config(client: Client, bytes: &[u8]) -> Self {
        let harness = Self::new(client);
        fs::write(&harness.config_path, bytes).expect("original config written");
        harness
    }

    fn receipts(&self) -> FileReceiptStore {
        FileReceiptStore::new(self.directory.path().join("state/receipts.json"))
    }

    fn install(&self) -> InstallReceipt {
        let manager = PlanManager::new("text-fidelity-install").expect("install manager");
        let plan = self
            .adapter
            .plan_install(
                &manager,
                &InstallCtx {
                    compresso_bin: self.compresso_binary.clone(),
                    scope: Scope::User,
                },
            )
            .unwrap_or_else(|error| panic!("{:?} install plan failed: {error}", self.client));
        self.adapter
            .install(&manager, &PlanApproval::from(&plan), &self.receipts())
            .unwrap_or_else(|error| panic!("{:?} install failed: {error}", self.client))
    }

    fn uninstall(&self, receipt: &InstallReceipt) {
        let manager = PlanManager::new("text-fidelity-uninstall").expect("uninstall manager");
        let plan = self
            .adapter
            .plan_uninstall(&manager, receipt)
            .unwrap_or_else(|error| panic!("{:?} uninstall plan failed: {error}", self.client));
        let report = self
            .adapter
            .uninstall(&manager, &PlanApproval::from(&plan), &self.receipts())
            .unwrap_or_else(|error| panic!("{:?} uninstall failed: {error}", self.client));
        assert!(
            report.retained_keys.is_empty(),
            "{:?} retained {:?}",
            self.client,
            report.retained_keys
        );
    }

    fn config(&self) -> Vec<u8> {
        fs::read(&self.config_path).expect("config readable")
    }
}

#[test]
fn test_install_then_uninstall_restores_byte_identical_original_for_every_encoding() {
    for client in Client::ALL {
        for encoding in ENCODINGS {
            let original = encode(client.lines(), *encoding);
            let harness = Harness::with_config(client, &original);

            let receipt = harness.install();
            let installed = harness.config();
            assert_ne!(installed, original, "{client:?} {encoding:?} installed");
            assert_eq!(
                fs::read(&receipt.backup_path).expect("backup readable"),
                original,
                "{client:?} {encoding:?} backup"
            );

            harness.uninstall(&receipt);

            assert_eq!(
                harness.config(),
                original,
                "{client:?} {encoding:?} restoration"
            );
        }
    }
}

#[test]
fn test_install_preserves_bom_untouched_terminators_and_inserts_dominant_ending() {
    for client in Client::ALL {
        for encoding in ENCODINGS {
            let original = encode(client.lines(), *encoding);
            let harness = Harness::with_config(client, &original);

            harness.install();
            let installed = harness.config();

            assert_eq!(
                installed.starts_with(BOM),
                encoding.bom,
                "{client:?} {encoding:?} byte-order mark"
            );
            let body = installed.strip_prefix(BOM).unwrap_or(&installed);
            assert!(
                !body.starts_with(BOM),
                "{client:?} {encoding:?} duplicated byte-order mark"
            );
            assert!(
                String::from_utf8_lossy(body).contains("compresso"),
                "{client:?} {encoding:?} owned entry"
            );

            // Every original line survives with its exact terminator, in
            // order; every other line was written by Compresso and uses the
            // dominant ending. The final line only inherits a missing
            // terminator when it is still the final line.
            let original_body = original.strip_prefix(BOM).unwrap_or(&original);
            let mut expected = split_lines(original_body).into_iter().peekable();
            let installed_lines = split_lines(body);
            let last = installed_lines.len() - 1;
            for (index, (content, terminator)) in installed_lines.into_iter().enumerate() {
                match expected.peek() {
                    Some((original_content, original_terminator))
                        if *original_content == content =>
                    {
                        if !original_terminator.is_empty() || index == last {
                            assert_eq!(
                                terminator, *original_terminator,
                                "{client:?} {encoding:?} untouched line {index}"
                            );
                        } else {
                            assert_eq!(
                                terminator,
                                encoding.endings.dominant(),
                                "{client:?} {encoding:?} formerly final line {index}"
                            );
                        }
                        expected.next();
                    }
                    _ => assert_eq!(
                        terminator,
                        encoding.endings.dominant(),
                        "{client:?} {encoding:?} written line {index}: {:?}",
                        String::from_utf8_lossy(content)
                    ),
                }
            }
            // JSON insertion rewrites the preceding line to add a comma, so
            // at most that one original line is absent from the result.
            assert!(
                expected.count() <= 1,
                "{client:?} {encoding:?} lost original lines"
            );
        }
    }
}

#[test]
fn test_uninstall_restores_non_canonical_json_formatting_byte_for_byte() {
    let original = b"\xEF\xBB\xBF{\r\n\t\"theme\" : \"night\",\n    \"mcpServers\": { \"other\": {\"command\":\"/opt/other\"} },\r\n\r\n  \"list\": [1,2 , 3]\r\n}";
    for client in [Client::ClaudeCode, Client::Cursor] {
        let harness = Harness::with_config(client, original);

        let receipt = harness.install();
        let installed = harness.config();
        assert!(installed.starts_with(BOM), "{client:?} byte-order mark");
        // Four CRLF against one LF: every re-rendered line is CRLF, and the
        // unchanged closing line still has no trailing newline.
        let lines = split_lines(&installed);
        let (closing, rendered) = lines.split_last().expect("installed lines");
        assert!(
            rendered.iter().all(|(_, terminator)| *terminator == "\r\n"),
            "{client:?} dominant line ending"
        );
        assert_eq!(*closing, (b"}".as_slice(), ""), "{client:?} final line");
        harness.uninstall(&receipt);

        assert_eq!(harness.config(), original, "{client:?} restoration");
    }
}

#[test]
fn test_uninstall_never_reverts_a_whitespace_only_user_edit_to_the_backup() {
    let original = b"{\r\n\t\"theme\": \"night\"\r\n}\r\n";
    for client in [Client::ClaudeCode, Client::Cursor] {
        let harness = Harness::with_config(client, original);
        let receipt = harness.install();
        let installed = String::from_utf8(harness.config()).expect("installed UTF-8");
        let reformatted = installed.replace("  \"theme\"", "      \"theme\"");
        assert_ne!(reformatted, installed, "{client:?} user reformat");
        fs::write(&harness.config_path, &reformatted).expect("user reformat written");

        harness.uninstall(&receipt);

        let restored = harness.config();
        assert_ne!(restored, original, "{client:?} backup must not be restored");
        assert!(
            split_lines(&restored)
                .iter()
                .all(|(_, terminator)| *terminator == "\r\n"),
            "{client:?} line endings"
        );
        assert!(
            !String::from_utf8_lossy(&restored).contains("compresso"),
            "{client:?} owned entry removed"
        );
    }
}

#[test]
fn test_uninstall_after_unrelated_user_edit_keeps_bom_and_line_endings() {
    for client in Client::ALL {
        let original = encode(
            client.lines(),
            Encoding {
                endings: Endings::CrLf,
                bom: true,
                final_newline: true,
            },
        );
        let harness = Harness::with_config(client, &original);
        let receipt = harness.install();
        let (before, after) = match client {
            Client::ClaudeCode | Client::Cursor => ("\"night\"", "\"day\""),
            Client::Codex => ("\"gpt-5\"", "\"gpt-6\""),
        };
        let edited = String::from_utf8(harness.config())
            .expect("installed UTF-8")
            .replace(before, after);
        fs::write(&harness.config_path, &edited).expect("user edit written");

        harness.uninstall(&receipt);

        let expected = String::from_utf8(original)
            .expect("original UTF-8")
            .replace(before, after);
        assert_eq!(
            harness.config(),
            expected.as_bytes(),
            "{client:?} restoration after an unrelated edit"
        );
    }
}

#[test]
fn test_created_config_uses_lf_without_bom_on_every_platform() {
    for client in Client::ALL {
        let harness = Harness::new(client);

        harness.install();
        let created = harness.config();

        assert!(!created.starts_with(BOM), "{client:?} byte-order mark");
        assert!(!created.contains(&b'\r'), "{client:?} carriage return");
        assert!(created.ends_with(b"\n"), "{client:?} final newline");
    }
}

fn write_executable(path: &Path) {
    fs::create_dir_all(path.parent().expect("executable parent"))
        .expect("executable parent created");
    fs::write(path, b"synthetic integration executable").expect("executable written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .expect("executable permissions");
    }
}

fn executable_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_owned()
    }
}

fn codex_platform() -> CodexPlatform {
    if cfg!(target_os = "macos") {
        CodexPlatform::MacOs
    } else if cfg!(target_os = "windows") {
        CodexPlatform::Windows
    } else {
        CodexPlatform::Linux
    }
}

fn cursor_platform() -> CursorPlatform {
    if cfg!(target_os = "macos") {
        CursorPlatform::MacOs
    } else if cfg!(target_os = "windows") {
        CursorPlatform::Windows
    } else {
        CursorPlatform::Linux
    }
}
