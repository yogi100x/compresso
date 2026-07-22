use std::fs;
use std::path::{Path, PathBuf};

use compresso_core::{NodeKind, TokenProvider, counter_for};
use compresso_eval::FixtureSet;
use serde::Deserialize;
use tempfile::TempDir;

const QUALITY_PATHS: [&str; 15] = [
    "adversarial/fence-bomb",
    "adversarial/malformed-json-payload",
    "adversarial/prompt-injection-repo",
    "code/mini-monorepo",
    "code/outline-large-file",
    "conversations/long-session-prune",
    "conversations/superseded-decisions",
    "conversations/tool-output-dupes",
    "debugging/compiler-output-rust",
    "debugging/test-failure-context",
    "logs/cargo-build-noise",
    "logs/repeated-npm-install",
    "logs/stack-trace-dupes",
    "provider-payloads/anthropic-counts",
    "provider-payloads/openai-unknown-fields",
];
const BENCHMARK_PATHS: [&str; 2] = [
    "benchmarks/json-2000-elements/input.json",
    "benchmarks/source-200kb/input.ts",
];
const SCALE_BENCHMARK_NAMES: [&str; 3] = ["input-1mib.log", "input-4mib.log", "input-8mib.log"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusPins {
    schema_version: u32,
    hash_algorithm: String,
    quality: Vec<QualityPin>,
    benchmark: Vec<BenchmarkPin>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QualityPin {
    path: String,
    eligible: bool,
    file_count: usize,
    size_bytes: u64,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BenchmarkPin {
    path: String,
    size_bytes: u64,
    tokens_o200k: usize,
    generator: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScaleBenchmarkManifest {
    generator: String,
    variants: Vec<ScaleBenchmarkPin>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScaleBenchmarkPin {
    name: String,
    bytes: u64,
    sha256: String,
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn load_pins(root: &Path) -> CorpusPins {
    let path = root.join("corpus.toml");
    let source = fs::read_to_string(&path).expect("fixtures/corpus.toml must be UTF-8");
    toml::from_str(&source).expect("fixtures/corpus.toml must parse strictly")
}

fn collect_files(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", directory.display()))
        .map(|entry| entry.expect("fixture entry must be readable"))
        .collect::<Vec<_>>();
    entries.sort_by_key(fs::DirEntry::file_name);

    for entry in entries {
        let file_type = entry
            .file_type()
            .expect("fixture entry type must be readable");
        let path = entry.path();
        if file_type.is_dir() {
            collect_files(root, &path, files);
        } else if file_type.is_file() {
            files.push(
                path.strip_prefix(root)
                    .expect("fixture file must remain under its root")
                    .to_path_buf(),
            );
        } else {
            panic!("fixture assets must be regular files: {}", path.display());
        }
    }
}

fn tree_snapshot(directory: &Path) -> Result<(usize, u64, String), String> {
    if !directory.is_dir() {
        return Err(format!(
            "missing canonical fixture directory {}",
            directory.display()
        ));
    }
    let mut files = Vec::new();
    collect_files(directory, directory, &mut files);
    files.sort();

    let mut bytes = Vec::new();
    let mut size_bytes = 0_u64;
    for relative in &files {
        let relative = relative
            .to_str()
            .ok_or_else(|| format!("non-UTF-8 fixture path {relative:?}"))?
            .replace('\\', "/");
        let content = fs::read(directory.join(&relative))
            .map_err(|error| format!("failed to read {relative}: {error}"))?;
        let content_len = u64::try_from(content.len()).map_err(|error| error.to_string())?;
        size_bytes = size_bytes
            .checked_add(content_len)
            .ok_or_else(|| "fixture byte size overflow".to_owned())?;
        bytes.extend_from_slice(relative.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(content_len.to_string().as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&content);
        bytes.push(0);
    }

    Ok((files.len(), size_bytes, sha256_hex(&bytes)))
}

fn lint_quality_pin(root: &Path, pin: &QualityPin) -> Result<(), String> {
    let actual = tree_snapshot(&root.join(&pin.path))?;
    if actual != (pin.file_count, pin.size_bytes, pin.sha256.clone()) {
        return Err(format!(
            "canonical fixture {} drifted: expected ({}, {}, {}), got ({}, {}, {})",
            pin.path, pin.file_count, pin.size_bytes, pin.sha256, actual.0, actual.1, actual.2,
        ));
    }
    Ok(())
}

fn lint_benchmark_pin(root: &Path, pin: &BenchmarkPin) -> Result<(), String> {
    let path = root.join(&pin.path);
    let bytes = fs::read(&path)
        .map_err(|error| format!("missing canonical benchmark {}: {error}", path.display()))?;
    let size_bytes = u64::try_from(bytes.len()).map_err(|error| error.to_string())?;
    let hash = sha256_hex(&bytes);
    if (size_bytes, hash.as_str()) != (pin.size_bytes, pin.sha256.as_str()) {
        return Err(format!("canonical benchmark {} drifted", pin.path));
    }
    let source = std::str::from_utf8(&bytes)
        .map_err(|error| format!("benchmark {} is not UTF-8: {error}", pin.path))?;
    let tokens = counter_for(TokenProvider::OpenAiO200k).count(source);
    if tokens != pin.tokens_o200k {
        return Err(format!(
            "canonical benchmark {} token count drifted: expected {}, got {tokens}",
            pin.path, pin.tokens_o200k
        ));
    }
    Ok(())
}

#[test]
fn test_canonical_quality_corpus_paths_hashes_and_sizes() {
    let root = fixtures_root();
    let pins = load_pins(&root);
    assert_eq!(pins.schema_version, 1);
    assert_eq!(pins.hash_algorithm, "sha256");
    assert_eq!(pins.quality.len(), QUALITY_PATHS.len());
    assert_eq!(
        pins.quality
            .iter()
            .map(|pin| pin.path.as_str())
            .collect::<Vec<_>>(),
        QUALITY_PATHS
    );
    for pin in &pins.quality {
        lint_quality_pin(&root, pin).unwrap_or_else(|error| panic!("{error}"));
    }
}

#[test]
fn test_delete_or_rename_canonical_fixture_fails_lint() {
    let temporary = TempDir::new().expect("temporary directory should be created");
    let fixture = temporary.path().join("logs/example");
    fs::create_dir_all(&fixture).expect("fixture directory should be created");
    fs::write(fixture.join("fixture.toml"), "name = \"example\"\n")
        .expect("fixture file should be written");
    let (file_count, size_bytes, sha256) = tree_snapshot(&fixture).expect("snapshot should work");
    let pin = QualityPin {
        path: "logs/example".to_owned(),
        eligible: false,
        file_count,
        size_bytes,
        sha256,
    };

    fs::rename(&fixture, temporary.path().join("logs/renamed")).expect("fixture should be renamed");
    assert!(lint_quality_pin(temporary.path(), &pin).is_err());
    fs::rename(temporary.path().join("logs/renamed"), &fixture)
        .expect("fixture should be restored");
    fs::remove_dir_all(&fixture).expect("fixture should be deleted");
    assert!(lint_quality_pin(temporary.path(), &pin).is_err());
}

#[test]
fn test_fixture_hash_drift_fails_lint_even_when_size_is_unchanged() {
    let temporary = TempDir::new().expect("temporary directory should be created");
    let fixture = temporary.path().join("logs/example");
    fs::create_dir_all(&fixture).expect("fixture directory should be created");
    fs::write(fixture.join("input.txt"), "alpha\n").expect("fixture file should be written");
    let (file_count, size_bytes, sha256) = tree_snapshot(&fixture).expect("snapshot should work");
    let pin = QualityPin {
        path: "logs/example".to_owned(),
        eligible: false,
        file_count,
        size_bytes,
        sha256,
    };

    fs::write(fixture.join("input.txt"), "omega\n").expect("fixture should be changed");
    assert!(lint_quality_pin(temporary.path(), &pin).is_err());
}

#[test]
fn test_gate_population_is_exact_and_benchmarks_are_excluded() {
    let root = fixtures_root();
    let pins = load_pins(&root);
    let set = FixtureSet::load(&root).expect("canonical quality fixtures should load");

    assert_eq!(QUALITY_PATHS.len(), 15);
    assert_eq!(set.fixtures.len(), 15);
    assert_eq!(
        set.fixtures
            .iter()
            .filter(|fixture| fixture.eligible)
            .count(),
        9
    );
    assert_eq!(pins.quality.iter().filter(|pin| pin.eligible).count(), 9);
    for fixture in &set.fixtures {
        assert!(!fixture.directory.starts_with(root.join("benchmarks")));
        let path = fixture
            .directory
            .strip_prefix(&root)
            .expect("fixture must remain under corpus root")
            .to_string_lossy()
            .replace('\\', "/");
        let pin = pins
            .quality
            .iter()
            .find(|pin| pin.path == path)
            .expect("every loaded fixture must be canonically pinned");
        assert_eq!(pin.eligible, fixture.eligible);
    }
}

#[test]
fn test_benchmark_paths_hashes_sizes_tokens_and_generators() {
    let root = fixtures_root();
    let pins = load_pins(&root);
    let benchmark_root = root.join("benchmarks");
    let mut benchmark_files = Vec::new();
    collect_files(&benchmark_root, &benchmark_root, &mut benchmark_files);
    benchmark_files.sort();
    assert_eq!(
        benchmark_files,
        [
            PathBuf::from("generate.mjs"),
            PathBuf::from("json-2000-elements/input.json"),
            PathBuf::from("mib-rebuild-stream/generate.mjs"),
            PathBuf::from("mib-rebuild-stream/input-1mib.log"),
            PathBuf::from("mib-rebuild-stream/input-4mib.log"),
            PathBuf::from("mib-rebuild-stream/input-8mib.log"),
            PathBuf::from("mib-rebuild-stream/manifest.json"),
            PathBuf::from("source-200kb/input.ts"),
        ]
    );
    assert_eq!(pins.benchmark.len(), BENCHMARK_PATHS.len());
    assert_eq!(
        pins.benchmark
            .iter()
            .map(|pin| pin.path.as_str())
            .collect::<Vec<_>>(),
        BENCHMARK_PATHS
    );
    for pin in &pins.benchmark {
        assert_eq!(pin.generator, "node fixtures/benchmarks/generate.mjs");
        lint_benchmark_pin(&root, pin).unwrap_or_else(|error| panic!("{error}"));
    }

    let json =
        fs::read_to_string(root.join(BENCHMARK_PATHS[0])).expect("JSON benchmark must be UTF-8");
    let elements: Vec<serde_json::Value> =
        serde_json::from_str(&json).expect("JSON benchmark must parse");
    assert_eq!(elements.len(), 2_000);
    assert_eq!(
        fs::metadata(root.join(BENCHMARK_PATHS[1]))
            .expect("source benchmark must exist")
            .len(),
        200 * 1024
    );
}

#[test]
fn test_scale_benchmark_manifest_sizes_and_hashes() {
    let root = fixtures_root().join("benchmarks/mib-rebuild-stream");
    let manifest_source =
        fs::read_to_string(root.join("manifest.json")).expect("scale manifest must be UTF-8");
    let manifest: ScaleBenchmarkManifest =
        serde_json::from_str(&manifest_source).expect("scale manifest must parse strictly");

    assert_eq!(
        manifest.generator,
        "node fixtures/benchmarks/mib-rebuild-stream/generate.mjs"
    );
    assert_eq!(manifest.variants.len(), SCALE_BENCHMARK_NAMES.len());
    assert_eq!(
        manifest
            .variants
            .iter()
            .map(|variant| variant.name.as_str())
            .collect::<Vec<_>>(),
        SCALE_BENCHMARK_NAMES
    );

    for (variant, expected_bytes) in
        manifest
            .variants
            .iter()
            .zip([1_u64 << 20, 4_u64 << 20, 8_u64 << 20])
    {
        let bytes = fs::read(root.join(&variant.name)).expect("scale payload must exist");
        assert_eq!(variant.bytes, expected_bytes);
        assert_eq!(u64::try_from(bytes.len()).unwrap(), expected_bytes);
        assert_eq!(sha256_hex(&bytes), variant.sha256);
    }
}

#[test]
fn test_canonical_quality_fixture_shapes() {
    let root = fixtures_root();
    let cargo_log = fs::read_to_string(root.join("logs/cargo-build-noise/build.log"))
        .expect("Cargo build fixture must be UTF-8");
    assert!((190..=220).contains(&cargo_log.lines().count()));
    assert_eq!(cargo_log.matches("warning: unused variable").count(), 12);
    assert_eq!(cargo_log.matches("error[E0599]").count(), 1);

    let rustc_output = fs::read_to_string(root.join("debugging/compiler-output-rust/input.txt"))
        .expect("Rust compiler fixture must be UTF-8");
    assert_eq!(rustc_output.matches("warning: unused variable").count(), 30);
    assert_eq!(rustc_output.matches("error[E0308]").count(), 1);

    let outline = fs::read_to_string(root.join("code/outline-large-file/input.ts"))
        .expect("outline fixture must be UTF-8");
    assert_eq!(outline.lines().count(), 900);

    let set = FixtureSet::load(&root).expect("canonical quality fixtures should load");
    let long_session = set
        .fixtures
        .iter()
        .find(|fixture| fixture.name == "long-session-prune")
        .expect("long-session-prune must be committed");
    assert_eq!(long_session.graph.nodes.len(), 42);
    for index in 1..20 {
        let id = format!("user-{index}");
        let node = long_session
            .graph
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap_or_else(|| panic!("{id} must exist"));
        assert_eq!(node.kind, NodeKind::Summary, "{id} must be archived");
        assert!(!node.protected, "{id} must remain eligible for pruning");
    }
    let current_user = long_session
        .graph
        .nodes
        .iter()
        .find(|node| node.id == "user-20")
        .expect("the current user request must exist");
    assert_eq!(current_user.kind, NodeKind::UserInstruction);
    assert!(current_user.protected);
    assert_eq!(
        long_session
            .graph
            .edges
            .iter()
            .filter(|edge| edge.kind == compresso_core::EdgeKind::RespondsTo)
            .count(),
        20,
        "all historical and current request/response pairs must stay atomic",
    );
    let tokens = long_session
        .graph
        .nodes
        .iter()
        .map(|node| node.token_count)
        .sum::<usize>();
    assert!((10_000..=14_000).contains(&tokens), "got {tokens} tokens");
}

fn sha256_hex(input: &[u8]) -> String {
    sha256(input)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn sha256(input: &[u8]) -> [u8; 32] {
    const INITIAL: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    const ROUND: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    let bit_length = u64::try_from(input.len())
        .unwrap_or(u64::MAX / 8)
        .saturating_mul(8);
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());

    let mut state = INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, bytes) in chunk.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes(
                bytes
                    .try_into()
                    .expect("a SHA-256 word always contains four bytes"),
            );
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for (&word, &constant) in words.iter().zip(&ROUND) {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let temporary1 = h
                .wrapping_add(sum1)
                .wrapping_add(choice)
                .wrapping_add(constant)
                .wrapping_add(word);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temporary2 = sum0.wrapping_add(majority);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temporary1);
            d = c;
            c = b;
            b = a;
            a = temporary1.wrapping_add(temporary2);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut digest = [0_u8; 32];
    for (chunk, value) in digest.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&value.to_be_bytes());
    }
    digest
}

#[test]
fn test_sha256_reference_vector() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
