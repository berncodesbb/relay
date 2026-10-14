//! Golden view tests: for every valid fixture case under
//! `tests/fixtures/cases/*`, replaying `events.jsonl` through `relay-core`
//! and building the status/why views must reproduce the committed
//! `expected-status.json` / `expected-why.json` canonical goldens exactly.
//! `sessionKey` comes from the case's `meta.json`, mirroring the retired
//! TypeScript exporter (`scripts/export-compatibility-fixtures.mjs`, removed
//! from the tree 2026-10-14); the committed goldens are now frozen evidence.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use relay_core::canonical::canonical_json;
use relay_core::events::{EventEnvelope, validate_stored_event};
use relay_core::reducer::replay_project;
use relay_ui::{build_status_view, build_why_view};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .canonicalize()
        .expect("tests/fixtures must exist next to the workspace")
}

fn case_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(root.join("cases"))
        .expect("tests/fixtures/cases must exist")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn decode_and_replay(case_dir: &Path) -> (Vec<relay_core::RelayEvent>, relay_core::ProjectState) {
    let text = fs::read_to_string(case_dir.join("events.jsonl")).unwrap();
    let mut events = Vec::new();
    for line in text.split('\n').filter(|line| !line.is_empty()) {
        let envelope: EventEnvelope = serde_json::from_str(line).unwrap();
        events.push(validate_stored_event(&envelope).unwrap());
    }
    let state = replay_project(&events).unwrap();
    (events, state)
}

fn session_key(case_dir: &Path) -> Option<String> {
    let meta: Value =
        serde_json::from_str(&fs::read_to_string(case_dir.join("meta.json")).unwrap()).unwrap();
    meta["sessionKey"].as_str().map(str::to_string)
}

#[test]
fn every_valid_case_matches_the_committed_status_and_why_goldens() {
    let root = fixtures_root();
    let mut checked = 0;
    for case_dir in case_dirs(&root) {
        let meta: Value =
            serde_json::from_str(&fs::read_to_string(case_dir.join("meta.json")).unwrap()).unwrap();
        if !meta["valid"].as_bool().unwrap_or(false) {
            continue;
        }
        let (events, state) = decode_and_replay(&case_dir);
        let session = session_key(&case_dir);

        let status = build_status_view(&state, &events, session.as_deref());
        let expected_status = fs::read_to_string(case_dir.join("expected-status.json")).unwrap();
        assert_eq!(
            canonical_json(&serde_json::to_value(&status).unwrap()),
            expected_status.trim_end(),
            "status view diverged for case {}",
            case_dir.display()
        );

        let why = build_why_view(&state, &events);
        let expected_why = fs::read_to_string(case_dir.join("expected-why.json")).unwrap();
        assert_eq!(
            canonical_json(&serde_json::to_value(&why).unwrap()),
            expected_why.trim_end(),
            "why view diverged for case {}",
            case_dir.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 10,
        "expected at least 10 valid cases, got {checked}"
    );
}
