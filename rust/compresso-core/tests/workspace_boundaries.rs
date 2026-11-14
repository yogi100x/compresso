use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const EXPECTED_CRATES: [&str; 11] = [
    "compresso-agent",
    "compresso-atlas",
    "compresso-cli",
    "compresso-core",
    "compresso-eval",
    "compresso-integrations",
    "compresso-mcp",
    "compresso-proxy",
    "compresso-recall",
    "compresso-store",
    "compresso-updater",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root should exist")
}

fn read_manifest(crate_name: &str) -> toml::Value {
    let path = workspace_root()
        .join("rust")
        .join(crate_name)
        .join("Cargo.toml");
    let manifest = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    toml::from_str(&manifest)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}

fn dependency_names(value: &toml::Value) -> BTreeSet<String> {
    fn visit(value: &toml::Value, names: &mut BTreeSet<String>) {
        let Some(table) = value.as_table() else {
            return;
        };

        for (key, child) in table {
            if matches!(
                key.as_str(),
                "dependencies" | "dev-dependencies" | "build-dependencies"
            ) && let Some(dependencies) = child.as_table()
            {
                names.extend(dependencies.keys().cloned());
            }
            visit(child, names);
        }
    }

    let mut names = BTreeSet::new();
    visit(value, &mut names);
    names
}

#[test]
fn test_workspace_contains_exactly_the_slice_crates() {
    let rust_dir = workspace_root().join("rust");
    let actual = fs::read_dir(rust_dir)
        .expect("rust workspace directory should exist")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("Cargo.toml").is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>();
    let expected = EXPECTED_CRATES.map(str::to_owned).into_iter().collect();

    assert_eq!(actual, expected);
}

#[test]
fn test_no_internal_deps_in_core() {
    let dependencies = dependency_names(&read_manifest("compresso-core"));
    let internal = dependencies
        .iter()
        .filter(|name| name.starts_with("compresso-"))
        .collect::<Vec<_>>();

    assert!(
        internal.is_empty(),
        "compresso-core must have no internal dependencies, found {internal:?}"
    );
}

#[test]
fn test_store_never_imports_atlas() {
    let dependencies = dependency_names(&read_manifest("compresso-store"));

    assert!(!dependencies.contains("compresso-atlas"));
}

#[test]
fn test_local_boundary_crates_have_no_network_io_dependencies() {
    let forbidden = ["hyper", "reqwest", "ureq"];

    for crate_name in [
        "compresso-core",
        "compresso-atlas",
        "compresso-store",
        "compresso-mcp",
    ] {
        let dependencies = dependency_names(&read_manifest(crate_name));
        let found = forbidden
            .iter()
            .filter(|name| dependencies.contains(**name))
            .collect::<Vec<_>>();
        assert!(
            found.is_empty(),
            "{crate_name} must not declare network-I/O dependencies, found {found:?}"
        );
    }
}

#[test]
fn test_stub_crates_name_their_future_epics() {
    for (crate_name, epic) in [
        ("compresso-agent", "E8"),
        ("compresso-proxy", "E11"),
        ("compresso-updater", "E12"),
    ] {
        let path = workspace_root()
            .join("rust")
            .join(crate_name)
            .join("src/lib.rs");
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        assert!(
            source.contains(epic),
            "{} must name its future epic {epic}",
            path.display()
        );
    }
}
