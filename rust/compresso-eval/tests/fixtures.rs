use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const CATEGORIES: [&str; 6] = [
    "adversarial",
    "code",
    "conversations",
    "debugging",
    "logs",
    "provider-payloads",
];

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn manifest_paths(root: &Path) -> Vec<PathBuf> {
    let mut manifests = Vec::new();
    for category in fs::read_dir(root).expect("fixtures directory must be readable") {
        let category = category.expect("fixture category entry must be readable");
        if !category
            .file_type()
            .expect("entry type must be readable")
            .is_dir()
        {
            continue;
        }
        for fixture in fs::read_dir(category.path()).expect("fixture category must be readable") {
            let fixture = fixture.expect("fixture entry must be readable");
            if !fixture
                .file_type()
                .expect("entry type must be readable")
                .is_dir()
            {
                continue;
            }
            let manifest = fixture.path().join("fixture.toml");
            if manifest.is_file() {
                manifests.push(manifest);
            }
        }
    }
    manifests.sort();
    manifests
}

fn assert_iso_8601_millis_utc(timestamp: &str) {
    let bytes = timestamp.as_bytes();
    assert_eq!(bytes.len(), 24, "timestamp must use millisecond UTC form");
    for position in [4, 7] {
        assert_eq!(bytes[position], b'-');
    }
    assert_eq!(bytes[10], b'T');
    for position in [13, 16] {
        assert_eq!(bytes[position], b':');
    }
    assert_eq!(bytes[19], b'.');
    assert_eq!(bytes[23], b'Z');
    for (position, byte) in bytes.iter().enumerate() {
        if ![4, 7, 10, 13, 16, 19, 23].contains(&position) {
            assert!(byte.is_ascii_digit(), "timestamp has a non-digit component");
        }
    }
}

#[test]
fn test_fixture_tomls_parse() {
    let root = fixtures_root();
    let manifests = manifest_paths(&root);
    assert!(manifests.len() >= CATEGORIES.len());

    let mut represented_categories = BTreeSet::new();
    for manifest_path in manifests {
        let source = fs::read_to_string(&manifest_path).expect("fixture.toml must be UTF-8");
        let manifest: toml::Value = source.parse().expect("fixture.toml must parse");
        let table = manifest
            .as_table()
            .expect("fixture.toml root must be a table");

        let fixture_dir = manifest_path.parent().expect("manifest must have a parent");
        let category_dir = fixture_dir.parent().expect("fixture must have a category");
        let expected_name = fixture_dir
            .file_name()
            .and_then(|value| value.to_str())
            .expect("fixture directory must be UTF-8");
        let expected_category = category_dir
            .file_name()
            .and_then(|value| value.to_str())
            .expect("category directory must be UTF-8");

        assert_eq!(
            table.get("name").and_then(toml::Value::as_str),
            Some(expected_name)
        );
        assert_eq!(
            table.get("category").and_then(toml::Value::as_str),
            Some(expected_category)
        );
        represented_categories.insert(expected_category.to_owned());
        assert!(
            table
                .get("description")
                .and_then(toml::Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        );
        assert!(table.get("eligible").is_some_and(toml::Value::is_bool));
        assert!(matches!(
            table.get("mode").and_then(toml::Value::as_str),
            Some("safe" | "passthrough")
        ));

        let budget = table
            .get("budget")
            .and_then(toml::Value::as_table)
            .expect("[budget] is required");
        assert!(
            budget
                .get("max_tokens")
                .is_some_and(toml::Value::is_integer)
        );
        assert!(
            budget
                .get("max_latency_ms")
                .is_some_and(toml::Value::is_integer)
        );

        let input = table
            .get("input")
            .and_then(toml::Value::as_table)
            .expect("[input] is required");
        let forms = usize::from(input.contains_key("graph"))
            + usize::from(input.contains_key("repo"))
            + usize::from(input.contains_key("files") || input.contains_key("nodes"));
        assert_eq!(forms, 1, "fixture must declare exactly one input form");
        if let Some(relative_path) = input.get("graph").and_then(toml::Value::as_str) {
            assert!(fixture_dir.join(relative_path).is_file());
        }
        if let Some(relative_path) = input.get("repo").and_then(toml::Value::as_str) {
            assert!(fixture_dir.join(relative_path).is_dir());
        }
        if let Some(files) = input.get("files").and_then(toml::Value::as_array) {
            assert!(!files.is_empty());
            for file in files {
                let relative_path = file
                    .get("path")
                    .and_then(toml::Value::as_str)
                    .expect("each [[input.files]] entry needs a path");
                assert!(fixture_dir.join(relative_path).is_file());
            }
        }

        let expectations = table
            .get("expectations")
            .and_then(toml::Value::as_table)
            .expect("[expectations] is required");
        assert!(
            expectations
                .get("min_reduction_pct")
                .is_some_and(|value| value.is_float() || value.is_integer())
        );
    }

    assert_eq!(
        represented_categories,
        CATEGORIES.into_iter().map(str::to_owned).collect()
    );
}

#[test]
fn test_repeated_log_fixture_shape() {
    let path = fixtures_root().join("logs/repeated-npm-install/input.txt");
    let source = fs::read_to_string(path).expect("repeated log input must be UTF-8");
    let lines: Vec<_> = source.lines().collect();
    assert_eq!(lines.len(), 500);

    let normalized: BTreeSet<_> = lines
        .into_iter()
        .map(|line| {
            let (timestamp, message) = line
                .split_once(' ')
                .expect("each line must start with an ISO-8601 timestamp");
            assert_iso_8601_millis_utc(timestamp);
            message
        })
        .collect();
    assert_eq!(normalized.len(), 1);
}
