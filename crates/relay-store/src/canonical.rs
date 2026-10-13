//! Canonical JSON for Relay events (`docs/event-format.md` §2).
//!
//! INTEGRATED with `relay-core`: canonical byte serialization is owned by
//! [`relay_core::canonical::canonical_json`]. This module keeps only the
//! store-local [`Json`] value type (used to build payloads and projection
//! folds without `serde` derives) and converts to `serde_json::Value` for
//! canonicalization — there is exactly one canonical serializer in the
//! workspace, and the store's bytes always match the core's.
//!
//! Rules (normative, enforced by the core serializer): UTF-8 throughout; no
//! BOM; no insignificant whitespace; object keys sorted by Unicode code
//! point recursively; minimal string escaping with raw non-ASCII; exact
//! decimal integers; non-integer numbers (the `[0, 1]` confidence fields)
//! printed like JavaScript `Number.prototype.toString`.
//!
//! Readers must re-serialize after parsing and never hash raw input bytes;
//! [`Json::parse`] parses through `serde_json` into this value type, and
//! only [`Json::to_canonical_bytes`] output is ever hashed.

use std::collections::BTreeMap;

use crate::error::Result;
use crate::error::StoreError;

/// A canonical JSON value.
///
/// Numbers keep their integer/float distinction: integers that fit `i64`
/// (or a `u64` above `i64::MAX`) stay exact integers; everything else is a
/// binary64 float. Object keys are stored in a [`BTreeMap`] whose `String`
/// ordering is byte-wise over UTF-8 — exactly the code-point order the spec
/// requires.
#[derive(Debug, Clone)]
pub enum Json {
    /// `null`
    Null,
    /// `true` / `false`
    Bool(bool),
    /// An integer that fits in (and was written as) a signed 64-bit integer.
    Int(i64),
    /// An integer above `i64::MAX` written as an unsigned 64-bit integer.
    UInt(u64),
    /// A non-integer (or non-fitting) number, as an IEEE-754 binary64 value.
    Float(f64),
    /// A string value.
    Str(String),
    /// An array value.
    Array(Vec<Json>),
    /// An object value with canonically ordered keys.
    Object(BTreeMap<String, Json>),
}

impl PartialEq for Json {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Json::Null, Json::Null) => true,
            (Json::Bool(a), Json::Bool(b)) => a == b,
            (Json::Str(a), Json::Str(b)) => a == b,
            (Json::Array(a), Json::Array(b)) => a == b,
            (Json::Object(a), Json::Object(b)) => a == b,
            // Numbers compare by numeric value so that e.g. Int(2) equals
            // Float(2.0): both canonicalize to the same bytes ("2").
            (Json::Int(a), Json::Int(b)) => a == b,
            (Json::UInt(a), Json::UInt(b)) => a == b,
            (Json::Int(a), Json::UInt(b)) => *a >= 0 && (*a as u64) == *b,
            (Json::UInt(a), Json::Int(b)) => *b >= 0 && (*b as u64) == *a,
            (Json::Float(a), Json::Float(b)) => a == b,
            (Json::Int(a), Json::Float(b)) | (Json::Float(b), Json::Int(a)) => (*a as f64) == *b,
            (Json::UInt(a), Json::Float(b)) | (Json::Float(b), Json::UInt(a)) => (*a as f64) == *b,
            _ => false,
        }
    }
}

impl Eq for Json {}

impl Json {
    /// Parse JSON text into a canonical value.
    ///
    /// The input does not need to be canonical; canonical form is produced
    /// by [`Json::to_canonical_bytes`].
    pub fn parse(text: &str) -> Result<Json> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|error| StoreError::Canonical {
                reason: format!("input is not valid JSON: {error}"),
            })?;
        Json::from_serde(&value)
    }

    /// Convert a `serde_json` value into the canonical value type.
    pub fn from_serde(value: &serde_json::Value) -> Result<Json> {
        match value {
            serde_json::Value::Null => Ok(Json::Null),
            serde_json::Value::Bool(flag) => Ok(Json::Bool(*flag)),
            serde_json::Value::Number(number) => {
                if let Some(int) = number.as_i64() {
                    Ok(Json::Int(int))
                } else if let Some(uint) = number.as_u64() {
                    Ok(Json::UInt(uint))
                } else if let Some(float) = number.as_f64() {
                    Ok(Json::Float(float))
                } else {
                    Err(StoreError::Canonical {
                        reason: format!("number {number} cannot be represented by this reader"),
                    })
                }
            }
            serde_json::Value::String(text) => Ok(Json::Str(text.clone())),
            serde_json::Value::Array(items) => {
                let mut converted = Vec::with_capacity(items.len());
                for item in items {
                    converted.push(Json::from_serde(item)?);
                }
                Ok(Json::Array(converted))
            }
            serde_json::Value::Object(map) => {
                let mut converted = BTreeMap::new();
                for (key, item) in map {
                    converted.insert(key.clone(), Json::from_serde(item)?);
                }
                Ok(Json::Object(converted))
            }
        }
    }

    /// Convert into a `serde_json` value for canonicalization by relay-core.
    pub fn to_serde(&self) -> serde_json::Value {
        match self {
            Json::Null => serde_json::Value::Null,
            Json::Bool(flag) => serde_json::Value::Bool(*flag),
            Json::Int(value) => serde_json::Value::Number((*value).into()),
            Json::UInt(value) => serde_json::Value::Number((*value).into()),
            Json::Float(value) => serde_json::Number::from_f64(*value)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            Json::Str(text) => serde_json::Value::String(text.clone()),
            Json::Array(items) => {
                serde_json::Value::Array(items.iter().map(Json::to_serde).collect())
            }
            Json::Object(map) => serde_json::Value::Object(
                map.iter().map(|(k, v)| (k.clone(), v.to_serde())).collect(),
            ),
        }
    }

    /// Serialize to the canonical form, returning owned bytes. The bytes
    /// come from [`relay_core::canonical::canonical_json`].
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        relay_core::canonical::canonical_json(&self.to_serde()).into_bytes()
    }

    /// Serialize to the canonical form as a `String`.
    pub fn to_canonical_string(&self) -> String {
        relay_core::canonical::canonical_json(&self.to_serde())
    }

    /// Look up a key in an object value; returns `None` for non-objects or
    /// missing keys.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(map) => map.get(key),
            _ => None,
        }
    }

    /// Return the string content when this value is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(text) => Some(text),
            _ => None,
        }
    }

    /// Return the value as a non-negative integer when it is an integer.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Int(value) if *value >= 0 => Some(*value as u64),
            Json::UInt(value) => Some(*value),
            _ => None,
        }
    }

    /// Return the value as an `i64` when it is an integer in range.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Int(value) => Some(*value),
            Json::UInt(value) => (*value <= i64::MAX as u64).then_some(*value as i64),
            _ => None,
        }
    }

    /// Return the value as a boolean when it is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(flag) => Some(*flag),
            _ => None,
        }
    }

    /// Return the float value when this value is a number.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Int(value) => Some(*value as f64),
            Json::UInt(value) => Some(*value as f64),
            Json::Float(value) => Some(*value),
            _ => None,
        }
    }

    /// Build an object from an iterator of key/value pairs.
    pub fn object<K: Into<String>, I: IntoIterator<Item = (K, Json)>>(entries: I) -> Json {
        Json::Object(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// Build a string value.
    pub fn string(text: impl Into<String>) -> Json {
        Json::Str(text.into())
    }

    /// Build an integer value.
    pub fn int(value: i64) -> Json {
        Json::Int(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_keys_recursively() -> Result<()> {
        let parsed = Json::parse(r#"{"b":1,"a":{"d":2,"c":3}}"#)?;
        assert_eq!(parsed.to_canonical_string(), r#"{"a":{"c":3,"d":2},"b":1}"#);
        Ok(())
    }

    #[test]
    fn minimal_escaping_only() -> Result<()> {
        let parsed = Json::parse(r#""a\"b\\c\nd""#)?;
        assert_eq!(parsed.to_canonical_string(), r#""a\"b\\c\nd""#);
        // Control characters in string values are escaped with lowercase
        // \u00xx (or the named escapes); serde_json correctly refuses to
        // parse raw control bytes, so build the value directly.
        let control = Json::Str("x\u{0001}".to_string());
        assert_eq!(control.to_canonical_string(), "\"x\\u0001\"");
        let tab = Json::Str("t\u{0009}ab".to_string());
        assert_eq!(tab.to_canonical_string(), "\"t\\tab\"");
        Ok(())
    }

    #[test]
    fn non_ascii_stays_raw_utf8() -> Result<()> {
        let parsed = Json::parse("\"héllo → 🚀\"")?;
        assert_eq!(parsed.to_canonical_string(), "\"héllo → 🚀\"");
        Ok(())
    }

    #[test]
    fn integers_stay_exact() -> Result<()> {
        let parsed = Json::parse(r#"{"n":42,"big":9007199254740993}"#)?;
        // 2^53+1 must not round-trip through f64.
        assert_eq!(
            parsed.to_canonical_string(),
            r#"{"big":9007199254740993,"n":42}"#
        );
        assert_eq!(Json::parse("2.0")?.to_canonical_string(), "2");
        Ok(())
    }

    #[test]
    fn floats_match_js_to_string() -> Result<()> {
        // Hand-computed vectors: JS Number.prototype.toString output,
        // exercised through the relay-core serializer.
        let vectors: &[(f64, &str)] = &[
            (0.0, "0"),
            (1.0, "1"),
            (0.5, "0.5"),
            (0.25, "0.25"),
            (0.1, "0.1"),
            (0.3, "0.3"),
            (0.7, "0.7"),
            (0.85, "0.85"),
            (123.456, "123.456"),
            (100.0, "100"),
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
        ];
        for (input, expected) in vectors {
            let rendered = Json::Float(*input).to_canonical_string();
            assert_eq!(&rendered, expected, "input {input}");
        }
        assert_eq!(Json::Float(-0.5).to_canonical_string(), "-0.5");
        Ok(())
    }

    #[test]
    fn floats_match_js_to_string_sub_tenth_plain() -> Result<()> {
        // Permanent regression guard for the relay-core canonical float
        // layout (fixed): serde_json renders these in plain notation, so
        // the decimal point must come from the leading zeros of the full
        // digit string (0.001 → "0.001", not "0.1"), and the sign must be
        // carried separately in exponential notation (-1e-7, not "-.1e-7").
        let vectors: &[(f64, &str)] = &[
            (0.001, "0.001"),
            (0.0001, "0.0001"),
            (0.00001, "0.00001"),
            (0.000001, "0.000001"),
        ];
        for (input, expected) in vectors {
            assert_eq!(
                &Json::Float(*input).to_canonical_string(),
                expected,
                "input {input}"
            );
        }
        assert_eq!(&Json::Float(-1e-7).to_canonical_string(), "-1e-7");
        Ok(())
    }

    #[test]
    fn float_parse_reserialize_matches_js() -> Result<()> {
        // 5e-1 is not canonical; re-serialization must produce 0.5.
        assert_eq!(Json::parse("5e-1")?.to_canonical_string(), "0.5");
        assert_eq!(Json::parse("0.100")?.to_canonical_string(), "0.1");
        Ok(())
    }

    #[test]
    fn number_eq_across_representations() -> Result<()> {
        assert_eq!(Json::parse("2.0")?, Json::parse("2")?);
        assert_ne!(Json::parse("0.1")?, Json::parse("0.2")?);
        Ok(())
    }

    #[test]
    fn canonical_bytes_match_core_directly() -> Result<()> {
        // Cross-check the delegated serializer against relay-core itself.
        let value = Json::object([
            ("b", Json::int(1)),
            ("a", Json::string("x→y")),
            ("c", Json::Float(0.25)),
        ]);
        let direct = relay_core::canonical::canonical_json(&value.to_serde());
        assert_eq!(value.to_canonical_string(), direct);
        assert_eq!(direct, r#"{"a":"x→y","b":1,"c":0.25}"#);
        Ok(())
    }
}
