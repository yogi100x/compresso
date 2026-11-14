#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

fn mode(path: &Path) -> u32 {
    fs::metadata(path)
        .expect("metadata should load")
        .permissions()
        .mode()
        & 0o777
}

fn list_repositories(home: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_compresso"))
        .args(["repos", "list", "--json"])
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .output()
        .expect("compresso should run");
    assert!(
        output.status.success(),
        "repos list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_ordinary_command_tightens_a_store_left_readable_by_an_earlier_release() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let home = directory.path().join("home");
    fs::create_dir(&home).expect("home should be created");
    let data = home.join(".local/share/compresso");
    let database = data.join("compresso.db");

    list_repositories(&home);
    assert!(database.is_file(), "the first run should create the store");
    // What releases before the permission fix left behind.
    fs::set_permissions(&data, fs::Permissions::from_mode(0o755))
        .expect("directory mode should be set");
    fs::set_permissions(&database, fs::Permissions::from_mode(0o644))
        .expect("database mode should be set");

    list_repositories(&home);

    assert_eq!(mode(&data), 0o700);
    assert_eq!(mode(&database), 0o600);
}
