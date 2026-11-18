#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use compresso_integrations::{
    ClaudeCodeDiscovery, ClaudeCodeIntegration, CodexAdapter, CodexPlatform,
    CodexVerificationEvidence, CursorApplicationEvidence, CursorCliEvidence, CursorDiscovery,
    CursorIntegration, CursorLiveProbeEvidence, CursorPlatform, InstallCtx, Integration,
    MemoryReceiptStore, PlanApproval, PlanManager, Scope,
};
use serde_json::Value;
use tempfile::tempdir;
use toml_edit::DocumentMut;

const EXPECTED_MCP_TOOLS: [&str; 11] = [
    "compresso_repo_map",
    "compresso_search",
    "compresso_read",
    "compresso_outline",
    "compresso_unfold",
    "compresso_context",
    "compresso_impact",
    "compresso_trace",
    "compresso_compress",
    "compresso_stats",
    "compresso_explain",
];

struct InstalledClient {
    name: &'static str,
    config_path: PathBuf,
    command: PathBuf,
    installed_bytes: Vec<u8>,
}

#[test]
fn test_package_upgrade_preserves_client_config_and_uses_new_engine() {
    let directory = tempdir().expect("temporary directory should be created");
    let home = directory.path().join("home");
    let versions = directory.path().join("versions");
    let launchers = directory.path().join("launchers");
    fs::create_dir_all(&home).expect("home directory should be created");
    fs::create_dir_all(&versions).expect("versions directory should be created");
    fs::create_dir_all(&launchers).expect("launcher directory should be created");

    let first_engine = versions.join("compresso-0.1.0");
    let second_engine = versions.join("compresso-0.2.0");
    write_posix_engine(&first_engine, "engine-v1");
    write_posix_engine(&second_engine, "engine-v2");
    let stable_launcher = launchers.join("compresso");
    symlink(&first_engine, &stable_launcher).expect("stable launcher should be created");

    let claude = ClaudeCodeIntegration::new(
        home.clone(),
        ClaudeCodeDiscovery::new(Vec::<PathBuf>::new(), None),
    );
    let codex = CodexAdapter::with_platform(
        home.clone(),
        Vec::new(),
        None,
        "0.1.0",
        CodexPlatform::Linux,
    )
    .expect("Codex adapter should be created");
    let cursor = CursorIntegration::new(
        home.clone(),
        CursorDiscovery::new(
            CursorPlatform::Linux,
            Vec::<CursorCliEvidence>::new(),
            Vec::<CursorApplicationEvidence>::new(),
        ),
    );

    install(&claude, &stable_launcher, "claude-launcher-upgrade");
    install(&codex, &stable_launcher, "codex-launcher-upgrade");
    install(&cursor, &stable_launcher, "cursor-launcher-upgrade");

    let claude_config = claude
        .config_path(&Scope::User)
        .expect("Claude Code config path should resolve");
    let codex_config = codex.config_path();
    let cursor_config = cursor
        .config_path(&Scope::User)
        .expect("Cursor config path should resolve");
    let clients = [
        installed_json_client("Claude Code", claude_config),
        installed_codex_client(codex_config),
        installed_json_client("Cursor", cursor_config),
    ];

    for client in &clients {
        assert_eq!(
            client.command, stable_launcher,
            "{} should record the stable launcher path",
            client.name
        );
        assert_eq!(
            run_engine(&client.command),
            "engine-v1\n",
            "{} should initially invoke engine v1",
            client.name
        );
    }

    fs::remove_file(&stable_launcher).expect("old stable launcher should be removed");
    symlink(&second_engine, &stable_launcher).expect("stable launcher should be upgraded");

    for client in &clients {
        assert_eq!(
            fs::read(&client.config_path).expect("client config should remain readable"),
            client.installed_bytes,
            "{} config should remain byte-identical across upgrade",
            client.name
        );
        assert_eq!(
            run_engine(&client.command),
            "engine-v2\n",
            "{} should invoke engine v2 through its unchanged command",
            client.name
        );
    }

    assert!(claude.verify(&Scope::User).verified);
    let codex_bytes = fs::read(codex.config_path()).expect("Codex config should be readable");
    let codex_evidence = CodexVerificationEvidence::mcp_list(&stable_launcher, &codex_bytes, true)
        .expect("Codex verification evidence should bind to the upgraded launcher");
    assert!(
        codex
            .with_verification_evidence(codex_evidence)
            .verify(&Scope::User)
            .verified
    );
    let cursor_evidence = CursorLiveProbeEvidence::new(
        &stable_launcher,
        true,
        EXPECTED_MCP_TOOLS.into_iter().map(str::to_owned),
    );
    assert!(
        cursor
            .with_live_probe_evidence(cursor_evidence)
            .verify(&Scope::User)
            .verified
    );
}

fn install(adapter: &dyn Integration, launcher: &Path, session: &str) {
    let manager = PlanManager::new(session).expect("plan manager should be created");
    let receipts = MemoryReceiptStore::default();
    let plan = adapter
        .plan_install(
            &manager,
            &InstallCtx {
                compresso_bin: launcher.to_path_buf(),
                scope: Scope::User,
                session_recall: false,
            },
        )
        .expect("install plan should be created");
    adapter
        .install(&manager, &PlanApproval::from(&plan), &receipts)
        .expect("install should apply");
}

fn installed_json_client(name: &'static str, config_path: PathBuf) -> InstalledClient {
    let installed_bytes = fs::read(&config_path).expect("JSON config should be readable");
    let document: Value =
        serde_json::from_slice(&installed_bytes).expect("client config should be valid JSON");
    let command = document["mcpServers"]["compresso"]["command"]
        .as_str()
        .map(PathBuf::from)
        .expect("client command should be a string");
    InstalledClient {
        name,
        config_path,
        command,
        installed_bytes,
    }
}

fn installed_codex_client(config_path: PathBuf) -> InstalledClient {
    let installed_bytes = fs::read(&config_path).expect("Codex config should be readable");
    let document = String::from_utf8(installed_bytes.clone())
        .expect("Codex config should be UTF-8")
        .parse::<DocumentMut>()
        .expect("Codex config should be valid TOML");
    let command = document["mcp_servers"]["compresso"]["command"]
        .as_str()
        .map(PathBuf::from)
        .expect("Codex command should be a string");
    InstalledClient {
        name: "Codex",
        config_path,
        command,
        installed_bytes,
    }
}

fn write_posix_engine(path: &Path, engine: &str) {
    fs::write(path, format!("#!/bin/sh\nprintf '%s\\n' '{engine}'\n"))
        .expect("POSIX engine should be written");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .expect("POSIX engine should be executable");
}

fn run_engine(path: &Path) -> String {
    let output = Command::new(path)
        .output()
        .expect("configured launcher should execute");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    String::from_utf8(output.stdout).expect("engine output should be UTF-8")
}
