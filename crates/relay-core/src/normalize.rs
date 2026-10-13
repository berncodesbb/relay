//! Normalized cross-language comparison (`docs/event-format.md` §11).
//!
//! Compatibility fixtures compare TypeScript and Rust outputs. Normalization
//! applies before comparison and must preserve equality relationships:
//!
//! - **Timestamps**: RFC 3339 strings with `Z` collapse to `<ts>` (distinct
//!   source values may collapse).
//! - **Paths**: a configured workspace root is replaced by `<workspace>`.
//! - **IDs**: `evt_…`/`cor_…`/`ses_…`/`prj_…`/`art_…`/`run_…` values (and the
//!   legacy `session_…` spelling) map to `<id:kind-N>` placeholders through a
//!   per-fixture map so equal ids map to equal placeholders and different ids
//!   stay different.
//! - **Durations**: `durationMs` values collapse to `<ms>`.
//! - **Maps**: compare key-order-insensitively (parse, then compare canonical
//!   structures).
//! - Everything else compares exactly.

use serde_json::{Map, Value};
use std::collections::HashMap;

/// The identifier kinds §11 distinguishes.
const ID_PREFIXES: [&str; 7] = ["evt", "cor", "ses", "session", "prj", "art", "run"];

/// RFC 3339 UTC shape with `Z` (milliseconds optional when comparing loosely).
fn looks_like_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 20 {
        return false;
    }
    let digit = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_digit);
    digit(0)
        && digit(1)
        && digit(2)
        && digit(3)
        && bytes.get(4) == Some(&b'-')
        && digit(5)
        && digit(6)
        && bytes.get(7) == Some(&b'-')
        && digit(8)
        && digit(9)
        && bytes.get(10) == Some(&b'T')
        && digit(11)
        && digit(12)
        && bytes.get(13) == Some(&b':')
        && value.ends_with('Z')
}

/// Is `value` shaped like one of the prefixed ids §11 normalizes?
fn looks_like_id(value: &str) -> bool {
    let Some((prefix, _tail)) = value.split_once('_') else {
        return false;
    };
    ID_PREFIXES.contains(&prefix) && _tail.len() == 36
}

/// A per-fixture normalizer: equality-preserving placeholders for ids and a
/// configured workspace-root replacement.
#[derive(Debug, Clone, Default)]
pub struct Normalizer {
    id_map: HashMap<String, String>,
    id_counters: HashMap<&'static str, usize>,
    workspace_root: Option<String>,
}

impl Normalizer {
    /// A normalizer that only collapses timestamps and durations.
    pub fn new() -> Self {
        Normalizer::default()
    }

    /// A normalizer that additionally replaces the fixture workspace root with
    /// `<workspace>` wherever it appears.
    pub fn with_workspace_root(workspace_root: impl Into<String>) -> Self {
        Normalizer {
            id_map: HashMap::new(),
            id_counters: HashMap::new(),
            workspace_root: Some(workspace_root.into()),
        }
    }

    /// Normalize one JSON value (recursively).
    pub fn normalize(&mut self, value: &Value) -> Value {
        match value {
            Value::Null => Value::Null,
            Value::Bool(flag) => Value::Bool(*flag),
            Value::Number(number) => Value::Number(number.clone()),
            Value::String(text) => Value::String(self.normalize_string(text)),
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|item| self.normalize(item))
                    .collect::<Vec<_>>(),
            ),
            Value::Object(map) => {
                let mut out = Map::new();
                for (key, entry) in map {
                    if key == "durationMs" {
                        out.insert(key.clone(), Value::String("<ms>".to_string()));
                        continue;
                    }
                    out.insert(key.clone(), self.normalize(entry));
                }
                Value::Object(out)
            }
        }
    }

    /// Normalize a single string.
    pub fn normalize_string(&mut self, text: &str) -> String {
        if looks_like_id(text) {
            return self.map_id(text);
        }
        if looks_like_timestamp(text) {
            return "<ts>".to_string();
        }
        if let Some(root) = &self.workspace_root
            && text.contains(root.as_str())
        {
            return text.replace(root.as_str(), "<workspace>");
        }
        text.to_string()
    }

    fn map_id(&mut self, id: &str) -> String {
        if let Some(mapped) = self.id_map.get(id) {
            return mapped.clone();
        }
        let kind = id.split_once('_').map_or("id", |(prefix, _)| match prefix {
            "evt" => "evt",
            "cor" => "cor",
            "ses" | "session" => "ses",
            "prj" => "prj",
            "art" => "art",
            "run" => "run",
            _ => "id",
        });
        let counter = self.id_counters.entry(kind).or_insert(0);
        *counter += 1;
        let mapped = format!("<id:{kind}-{counter}>");
        self.id_map.insert(id.to_string(), mapped.clone());
        mapped
    }
}

/// Normalize a JSON document for comparison with the default rules (ids via a
/// fresh map, timestamps, durations; no workspace replacement).
pub fn normalize_value(value: &Value) -> Value {
    Normalizer::new().normalize(value)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_equal_ids_to_equal_placeholders_and_distinct_apart() {
        let mut normalizer = Normalizer::new();
        let a = "evt_00000000-0000-4000-8000-000000000001";
        let b = "evt_00000000-0000-4000-8000-000000000002";
        let session = "session_00000000-0000-4000-8000-000000000003";
        assert_eq!(
            normalizer.normalize_string(a),
            normalizer.normalize_string(a)
        );
        assert_ne!(
            normalizer.normalize_string(a),
            normalizer.normalize_string(b)
        );
        assert_eq!(normalizer.normalize_string(session), "<id:ses-1>");
    }

    #[test]
    fn collapses_timestamps_and_durations_and_replaces_the_workspace_root() {
        let mut normalizer = Normalizer::with_workspace_root("/tmp/relay-fixture-x");
        let value = json!({
            "at": "2026-10-13T00:00:00.000Z",
            "durationMs": 1234,
            "path": "/tmp/relay-fixture-x/.relay/artifacts/prd/v1-a.md",
            "kept": "plain text",
        });
        let normalized = normalizer.normalize(&value);
        assert_eq!(normalized["at"], json!("<ts>"));
        assert_eq!(normalized["durationMs"], json!("<ms>"));
        assert_eq!(
            normalized["path"],
            json!("<workspace>/.relay/artifacts/prd/v1-a.md")
        );
        assert_eq!(normalized["kept"], json!("plain text"));
    }

    #[test]
    fn normalize_value_is_idempotent_for_plain_json() {
        let value = json!({"b": [1, 2], "a": "x"});
        assert_eq!(normalize_value(&value), value);
    }
}
