//! Cross-language compatibility suite: the TypeScript-generated fixture
//! corpus under `tests/fixtures/` is the oracle for the Rust domain core.
//!
//! For every case directory:
//!
//! - each `events.jsonl` line must already be canonically serialized and
//!   correctly hash-chained (`docs/event-format.md` §2–§3);
//! - valid cases must decode (two-stage), replay through `relay-core`, and
//!   produce a state whose §11-normalized canonical form equals the committed
//!   `expected-state.json`;
//! - invalid cases must fail with the error class and message substring
//!   recorded in `meta.json` (captured from the TypeScript pipeline, except
//!   the spec-only `newer-envelope-version` case whose failure is defined by
//!   §1 and asserted only against the Rust reader).
//!
//! Fixture root: `../../tests/fixtures` relative to this crate, overridable
//! with `RELAY_FIXTURES_DIR`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use relay_core::canonical::canonical_json;
use relay_core::events::{EventEnvelope, RelayEvent, validate_stored_event};
use relay_core::normalize::normalize_value;
use relay_core::reducer::replay_project;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn fixtures_root() -> PathBuf {
    if let Ok(from_env) = std::env::var("RELAY_FIXTURES_DIR") {
        return PathBuf::from(from_env);
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .canonicalize()
        .expect("tests/fixtures must exist next to the workspace")
}

struct CaseMeta {
    name: String,
    valid: bool,
    envelope_version: u64,
    expect_error_name: Option<String>,
    expect_error: Option<String>,
    spec_only_failure: bool,
}

fn read_meta(case_dir: &Path) -> CaseMeta {
    let text = fs::read_to_string(case_dir.join("meta.json")).expect("meta.json readable");
    let value: Value = serde_json::from_str(&text).expect("meta.json is valid JSON");
    CaseMeta {
        name: value["name"].as_str().unwrap_or_default().to_string(),
        valid: value["valid"].as_bool().expect("meta.valid is a boolean"),
        envelope_version: value["envelopeVersion"].as_u64().unwrap_or(1),
        expect_error_name: value["expectErrorName"].as_str().map(str::to_string),
        expect_error: value["expectError"].as_str().map(str::to_string),
        spec_only_failure: value["specOnlyFailure"].as_bool().unwrap_or(false),
    }
}

fn case_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(root.join("cases"))
        .expect("tests/fixtures/cases must exist")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    assert!(dirs.len() >= 20, "expected at least 20 fixture cases");
    dirs
}

fn parse_lines(text: &str) -> Vec<Value> {
    text.split('\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).expect("each line is valid JSON"))
        .collect()
}

/// Decode + validate + replay one case's log; `Err` carries the domain error.
fn replay_case(
    lines: &[Value],
) -> Result<relay_core::project_state::ProjectState, relay_core::CoreError> {
    let mut events: Vec<RelayEvent> = Vec::with_capacity(lines.len());
    for line in lines {
        let envelope: EventEnvelope =
            serde_json::from_value(line.clone()).expect("envelope decodes");
        events.push(validate_stored_event(&envelope)?);
    }
    replay_project(&events)
}

#[test]
fn every_fixture_case_replays_or_fails_exactly_as_typescript_recorded() {
    let root = fixtures_root();
    for case_dir in case_dirs(&root) {
        let meta = read_meta(&case_dir);
        let log_text = fs::read_to_string(case_dir.join("events.jsonl")).expect("events.jsonl");
        let lines = parse_lines(&log_text);

        // Canonical serialization + hash chain, verified independently of the
        // domain decoder. The newer-envelope-version fixture carries
        // envelopeVersion 2 by design; verify_canonical_log pins version 1, so
        // check the canonical/hash portion against its declared version.
        assert_canonical_and_chained(&log_text, lines.as_slice(), &meta);

        if meta.valid {
            let state = replay_case(&lines).unwrap_or_else(|error| {
                panic!("{}: valid case failed to replay: {error}", meta.name)
            });
            let expected_text = fs::read_to_string(case_dir.join("expected-state.json"))
                .expect("expected-state.json");
            let expected: Value =
                serde_json::from_str(&expected_text).expect("expected-state is JSON");
            let actual = serde_json::to_value(&state).expect("state serializes");

            // Strict comparison first: the corpus is deterministic, so the
            // canonical forms must be byte-identical. Only cases whose meta
            // declares §11 normalization (the end-to-end run) fall back to the
            // §11 comparison, which must still preserve equality of ids and
            // timestamps that the exporter mapped deterministically.
            if canonical_json(&expected) != canonical_json(&actual) {
                let meta_text = fs::read_to_string(case_dir.join("meta.json")).expect("meta.json");
                let meta_value: Value = serde_json::from_str(&meta_text).expect("meta is JSON");
                let declares_normalization = meta_value["normalization"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty());
                assert!(
                    declares_normalization,
                    "{}: replayed state diverges from the TypeScript oracle and the case \
                     does not declare §11 normalization",
                    meta.name
                );
                let normalized_expected = normalize_value(&expected);
                let normalized_actual = normalize_value(&actual);
                assert_eq!(
                    canonical_json(&normalized_expected),
                    canonical_json(&normalized_actual),
                    "{}: §11-normalized state still diverges from the TypeScript oracle",
                    meta.name
                );
            }

            // Fixture-format integrity: valid cases also pin the runtime views
            // and the session key used to build them.
            for view in ["expected-status.json", "expected-why.json"] {
                assert!(
                    case_dir.join(view).exists(),
                    "{}: missing {view} in a valid case",
                    meta.name
                );
            }
        } else {
            let failure = replay_case(&lines).err().unwrap_or_else(|| {
                panic!("{}: invalid case replayed successfully in Rust", meta.name)
            });
            let expected_name = meta.expect_error_name.as_deref().unwrap_or_else(|| {
                panic!("{}: invalid case meta lacks expectErrorName", meta.name)
            });
            let expected_message = meta
                .expect_error
                .as_deref()
                .unwrap_or_else(|| panic!("{}: invalid case meta lacks expectError", meta.name));
            assert_eq!(
                failure.error_name(),
                expected_name,
                "{}: wrong error class ({})",
                meta.name,
                failure
            );
            assert!(
                failure.to_string().contains(expected_message),
                "{}: error message \"{}\" does not contain \"{}\"",
                meta.name,
                failure,
                expected_message
            );
            if meta.spec_only_failure {
                // The TypeScript reference accepts this log; only the Rust
                // reader implements the §1 envelope-version fail-closed rule.
                assert_eq!(expected_name, "InvalidEventEnvelopeError");
            }
        }
    }
}

/// Verify §2/§3 invariants directly on the raw parsed lines: each line is
/// byte-canonical, each hash matches, and the previous-hash chain is
/// contiguous with `null` at sequence 1.
fn assert_canonical_and_chained(log_text: &str, lines: &[Value], meta: &CaseMeta) {
    if !log_text.is_empty() {
        assert!(
            log_text.ends_with('\n'),
            "{}: log must end with a newline",
            meta.name
        );
    }
    let mut previous_hash: Option<String> = None;
    for (index, line) in lines.iter().enumerate() {
        let rendered = canonical_json(line);
        let raw = log_text.split('\n').nth(index).unwrap_or_default();
        assert_eq!(
            rendered,
            raw,
            "{}: line {} is not canonically serialized",
            meta.name,
            index + 1
        );
        let map = line.as_object().expect("each line is an object");
        assert_eq!(
            map.get("envelopeVersion").and_then(Value::as_u64),
            Some(meta.envelope_version),
            "{}: line {} envelopeVersion mismatch",
            meta.name,
            index + 1
        );
        let without_hash = {
            let mut cloned = line.clone();
            cloned.as_object_mut().unwrap().remove("hash");
            cloned
        };
        let computed = relay_core::events::compute_envelope_hash(&without_hash);
        let stored = map.get("hash").and_then(Value::as_str).unwrap_or_default();
        assert_eq!(
            computed,
            stored,
            "{}: line {} hash mismatch",
            meta.name,
            index + 1
        );
        assert_eq!(
            map.get("previousHash").and_then(Value::as_str),
            previous_hash.as_deref(),
            "{}: line {} previousHash mismatch",
            meta.name,
            index + 1
        );
        previous_hash = Some(computed);
    }
}

#[test]
fn manifest_lists_every_case_directory() {
    let root = fixtures_root();
    let manifest_text = fs::read_to_string(root.join("manifest.json")).expect("manifest.json");
    let manifest: Value = serde_json::from_str(&manifest_text).expect("manifest is JSON");
    let mut listed: Vec<&str> = manifest["caseNames"]
        .as_array()
        .expect("caseNames array")
        .iter()
        .map(|entry| entry.as_str().unwrap_or_default())
        .collect();
    listed.sort_unstable();
    let mut actual: Vec<String> = case_dirs(&root)
        .into_iter()
        .map(|dir| {
            dir.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    actual.sort();
    assert_eq!(
        listed,
        actual.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert_eq!(
        manifest["eventTypesCovered"].as_array().map(Vec::len),
        Some(24),
        "all 24 event types must be covered"
    );
}

#[test]
fn corpus_event_types_are_exactly_the_domain_set() {
    let root = fixtures_root();
    let manifest_text = fs::read_to_string(root.join("manifest.json")).expect("manifest.json");
    let manifest: Value = serde_json::from_str(&manifest_text).expect("manifest is JSON");
    let covered: Vec<String> = manifest["eventTypesCovered"]
        .as_array()
        .expect("eventTypesCovered array")
        .iter()
        .map(|entry| entry.as_str().unwrap_or_default().to_string())
        .collect();
    let domain: Vec<&str> = relay_core::events::RELAY_EVENT_TYPES
        .iter()
        .map(|event_type| event_type.as_str())
        .collect();
    assert_eq!(covered, domain);
}

#[test]
fn newer_envelope_version_fails_closed_per_spec() {
    // Direct, focused assertion for the spec-only fixture: a log written by a
    // newer store format must be rejected with upgrade guidance even though
    // the TypeScript reference (which has no envelopeVersion field) replays it.
    let root = fixtures_root();
    let case_dir = root.join("cases").join("newer-envelope-version");
    let meta = read_meta(&case_dir);
    assert!(!meta.valid);
    assert!(meta.spec_only_failure);
    let log_text = fs::read_to_string(case_dir.join("events.jsonl")).expect("events.jsonl");
    let lines = parse_lines(&log_text);
    let failure = match replay_case(&lines) {
        Ok(state) => panic!("envelopeVersion 2 must fail the Rust reader, replayed: {state:?}"),
        Err(error) => error,
    };
    assert_eq!(failure.error_name(), "InvalidEventEnvelopeError");
    assert!(failure.to_string().contains("envelopeVersion"), "{failure}");
    assert!(failure.to_string().contains("newer Relay"), "{failure}");
}

#[test]
fn legacy_pre_role_model_events_still_decode_and_replay() {
    let root = fixtures_root();
    let case_dir = root.join("cases").join("model-events-legacy-no-role");
    let log_text = fs::read_to_string(case_dir.join("events.jsonl")).expect("events.jsonl");
    let lines = parse_lines(&log_text);
    let state = replay_case(&lines).expect("legacy model events must replay");
    assert_eq!(state.stats().model_calls, 1);
    assert_eq!(
        state.stats().last_model_error.as_deref(),
        Some("rate-limit: throttled after retries")
    );
}

#[test]
fn replay_is_deterministic_for_every_valid_case() {
    // Fold each valid log twice through the pure reducer; §11-normalized
    // canonical states must be identical (the reducer has no hidden state).
    let root = fixtures_root();
    for case_dir in case_dirs(&root) {
        let meta = read_meta(&case_dir);
        if !meta.valid {
            continue;
        }
        let log_text = fs::read_to_string(case_dir.join("events.jsonl")).expect("events.jsonl");
        let lines = parse_lines(&log_text);
        let first = replay_case(&lines).expect("valid case replays");
        let second = replay_case(&lines).expect("valid case replays again");
        let first_value = serde_json::to_value(&first).expect("state serializes");
        let second_value = serde_json::to_value(&second).expect("state serializes");
        assert_eq!(
            canonical_json(&normalize_value(&first_value)),
            canonical_json(&normalize_value(&second_value)),
            "{}: replay is not deterministic",
            meta.name
        );
    }
}
