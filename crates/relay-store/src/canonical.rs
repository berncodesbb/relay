//! Canonical JSON serialization for Relay events (spec: `docs/event-format.md` §2).
//!
//! INTEGRATION SEAM: relay-core will own canonical serialization and the
//! envelope types. Until `relay-core` publishes its `canonical` module, this
//! module provides the store-local implementation required by
//! `docs/event-format.md`. When relay-core lands, replace the internals of
//! this module with a re-export of relay-core's serializer and delete the
//! duplicate; every other module goes through [`Json`] /
//! [`canonical_bytes`], so the swap is confined to this file and
//! [`crate::envelope`].
//!
//! Rules implemented here (normative):
//!
//! 1. UTF-8 throughout; no BOM; no insignificant whitespace; no line breaks
//!    outside JSON string escapes.
//! 2. Object keys sorted lexicographically by Unicode code point (equivalent
//!    to byte-wise over UTF-8), recursively.
//! 3. Strings escaped minimally: `\"`, `\\`, and `\b`, `\f`, `\n`, `\r`, `\t`
//!    or `\u00xx` (lowercase hex) for the remaining C0 range. Non-ASCII is
//!    emitted as raw UTF-8, never `\u`-escaped.
//! 4. Integers serialize as exact decimal integers (no exponent, no `.0`,
//!    no leading zeros or `+` sign).
//! 5. Non-integer numbers (only the bounded `[0, 1]` confidence fields exist
//!    in Relay events) serialize as the shortest decimal that round-trips to
//!    the same IEEE-754 binary64 value, matching JavaScript
//!    `Number.prototype.toString` (decimal notation for exponents in
//!    `-6..20`, exponential outside; `0` and `1` print bare).
//! 6. `true`, `false`, `null` serialize as the JSON literals.
//!
//! Readers must re-serialize after parsing and never hash raw input bytes;
//! [`Json::parse`] therefore parses through `serde_json` into this value
//! type, and only [`Json::to_canonical_bytes`] output is ever hashed.

use std::collections::BTreeMap;

use crate::error::StoreError;

/// A canonical JSON value.
///
/// Numbers keep their integer/float distinction: integers that fit `i64` (or a
/// `u64` above `i64::MAX`) stay exact integers; everything else is a binary64
/// float. Object keys are stored in a [`BTreeMap`] whose `String` ordering is
/// byte-wise over UTF-8 — exactly the code-point order the spec requires.
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
    /// The input does not need to be canonical; canonical form is produced by
    /// [`Json::to_canonical_bytes`]. Integers that exceed binary64-exact
    /// range keep their exact integer form.
    pub fn parse(text: &str) -> Result<Json, StoreError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|error| StoreError::Canonical {
                reason: format!("input is not valid JSON: {error}"),
            })?;
        Json::from_serde(&value)
    }

    /// Convert a `serde_json` value into the canonical value type.
    fn from_serde(value: &serde_json::Value) -> Result<Json, StoreError> {
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

    /// Serialize to the canonical form, returning owned bytes.
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_canonical(&mut out);
        out
    }

    /// Serialize to the canonical form as a `String` (UTF-8 by construction).
    pub fn to_canonical_string(&self) -> String {
        // Canonical output is valid UTF-8: string contents are raw UTF-8 and
        // every other emitted byte is ASCII.
        String::from_utf8(self.to_canonical_bytes()).unwrap_or_default()
    }

    /// Write the canonical serialization into `out`.
    fn write_canonical(&self, out: &mut Vec<u8>) {
        match self {
            Json::Null => out.extend_from_slice(b"null"),
            Json::Bool(true) => out.extend_from_slice(b"true"),
            Json::Bool(false) => out.extend_from_slice(b"false"),
            Json::Int(value) => {
                out.extend_from_slice(value.to_string().as_bytes());
            }
            Json::UInt(value) => {
                out.extend_from_slice(value.to_string().as_bytes());
            }
            Json::Float(value) => {
                out.extend_from_slice(format_f64(*value).as_bytes());
            }
            Json::Str(value) => write_escaped(value, out),
            Json::Array(items) => {
                out.push(b'[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    item.write_canonical(out);
                }
                out.push(b']');
            }
            Json::Object(map) => {
                out.push(b'{');
                for (index, (key, item)) in map.iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    write_escaped(key, out);
                    out.push(b':');
                    item.write_canonical(out);
                }
                out.push(b'}');
            }
        }
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

/// Serialize a canonical value to bytes (convenience free function).
pub fn canonical_bytes(value: &Json) -> Vec<u8> {
    value.to_canonical_bytes()
}

/// Escape and write a string per §2.3: `\"`, `\\`, the five named control
/// escapes, `\u00xx` (lowercase hex) for the rest of the C0 range, and raw
/// UTF-8 for everything else.
fn write_escaped(value: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for &byte in value.as_bytes() {
        match byte {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0C => out.extend_from_slice(b"\\f"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x00..=0x1F => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.extend_from_slice(b"\\u00");
                out.push(HEX[(byte >> 4) as usize]);
                out.push(HEX[(byte & 0x0F) as usize]);
            }
            _ => out.push(byte),
        }
    }
    out.push(b'"');
}

/// Format a float exactly like JavaScript `Number.prototype.toString`.
///
/// Rust's `{:e}` formatter yields the shortest round-tripping digits
/// (`1.2345e-7`); this function re-frames those digits using the JS notation
/// rules: decimal notation when the decimal exponent is in `-6..20`,
/// otherwise `d.dddde±X`. `0.0` and `-0.0` both print as `0`.
pub fn format_f64(value: f64) -> String {
    if value == 0.0 {
        return "0".to_string();
    }
    let negative = value < 0.0;
    let magnitude = value.abs();
    // Shortest digits + exponent, e.g. "1.2345e-7" or "5e-1".
    let exp_form = format!("{magnitude:e}");
    let (mantissa, exponent_text) = match exp_form.split_once('e') {
        Some(parts) => parts,
        None => return exp_form,
    };
    let exponent: i32 = exponent_text.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let digit_count = digits.len() as i32;

    let mut rendered = String::new();
    if negative {
        rendered.push('-');
    }
    if (-6..21).contains(&exponent) {
        // Decimal notation.
        if exponent >= 0 {
            let int_digits = exponent + 1;
            if digit_count <= int_digits {
                rendered.push_str(&digits);
                for _ in digit_count..int_digits {
                    rendered.push('0');
                }
            } else {
                rendered.push_str(&digits[..int_digits as usize]);
                rendered.push('.');
                rendered.push_str(&digits[int_digits as usize..]);
            }
        } else {
            rendered.push_str("0.");
            for _ in 0..(-exponent - 1) {
                rendered.push('0');
            }
            rendered.push_str(&digits);
        }
    } else {
        // JS exponential notation: one leading digit, optional fraction,
        // 'e', sign, bare exponent.
        rendered.push_str(&digits[..1]);
        if digit_count > 1 {
            rendered.push('.');
            rendered.push_str(&digits[1..]);
        }
        rendered.push('e');
        if exponent < 0 {
            rendered.push('-');
        } else {
            rendered.push('+');
        }
        rendered.push_str(&exponent.abs().to_string());
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_canonical_roundtrip(text: &str) -> Result<String, crate::error::StoreError> {
        Json::parse(text).map(|v| v.to_canonical_string())
    }

    #[test]
    fn sorts_keys_recursively() -> Result<(), Box<dyn std::error::Error>> {
        let parsed = Json::parse(r#"{"b":1,"a":{"d":2,"c":3}}"#)?;
        assert_eq!(parsed.to_canonical_string(), r#"{"a":{"c":3,"d":2},"b":1}"#);
        Ok(())
    }

    #[test]
    fn minimal_escaping_only() -> Result<(), Box<dyn std::error::Error>> {
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
    fn non_ascii_stays_raw_utf8() -> Result<(), Box<dyn std::error::Error>> {
        let parsed = Json::parse("\"héllo → 🚀\"")?;
        assert_eq!(parsed.to_canonical_string(), "\"héllo → 🚀\"");
        Ok(())
    }

    #[test]
    fn integers_stay_exact() -> Result<(), Box<dyn std::error::Error>> {
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
    fn floats_match_js_to_string() {
        // Hand-computed vectors: JS Number.prototype.toString output.
        let vectors: &[(f64, &str)] = &[
            (0.0, "0"),
            (1.0, "1"),
            (0.5, "0.5"),
            (0.25, "0.25"),
            (0.1, "0.1"),
            (0.001, "0.001"),
            (0.0001, "0.0001"),
            (0.00001, "0.00001"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (0.3, "0.3"),
            (0.7, "0.7"),
            (0.85, "0.85"),
            (123.456, "123.456"),
            (100.0, "100"),
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
        ];
        for (input, expected) in vectors {
            assert_eq!(&format_f64(*input), expected, "input {input}");
        }
        assert_eq!(format_f64(-0.5), "-0.5");
        assert_eq!(format_f64(-1e-7), "-1e-7");
    }

    #[test]
    fn float_parse_reserialize_matches_js() -> Result<(), Box<dyn std::error::Error>> {
        // 5e-1 is not canonical; re-serialization must produce 0.5.
        assert_eq!(parse_canonical_roundtrip("5e-1")?, "0.5");
        assert_eq!(parse_canonical_roundtrip("0.100")?, "0.1");
        Ok(())
    }

    #[test]
    fn number_eq_across_representations() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(Json::parse("2.0")?, Json::parse("2")?);
        assert_ne!(Json::parse("0.1")?, Json::parse("0.2")?);
        Ok(())
    }
}
