//! The workspace's single canonical-JSON implementation and the content
//! digest built on it (lead ruling, fact-format §4).
//!
//! Every layer that commits bytes must commit to the **same** bytes:
//! `relay-executor` computes receipt content digests, `relay-artifacts`
//! publishes blobs, and the fact store checksums records — all through
//! these functions. `relay-store::fact::canonical` delegates here.
//!
//! # Rules
//!
//! 1. UTF-8 throughout; no BOM; no insignificant whitespace; no line
//!    breaks outside JSON string escapes.
//! 2. Object keys are sorted lexicographically by Unicode code point,
//!    recursively.
//! 3. Strings are escaped minimally: `"` as `\"`, backslash as `\\`, and
//!    control characters as `\b`, `\f`, `\n`, `\r`, `\t`, or `\u00xx`
//!    (lowercase hex) for the remaining C0 range. Non-ASCII characters are
//!    emitted as raw UTF-8, never `\u`-escaped.
//! 4. `true`, `false`, `null` serialize as the JSON literals.
//!
//! # Supported number subset
//!
//! - **Integers** (`i64`, and `u64` values above `i64::MAX`): exact decimal
//!   digits, no exponent, no leading zeros, no `+` sign. Integers up to
//!   `u64::MAX` round-trip exactly; anything larger parses as a float.
//! - **Non-integers**: finite IEEE-754 binary64 values, serialized as the
//!   shortest decimal that round-trips to the same binary64 value, always
//!   in plain positional notation (never exponential). `1e21` therefore
//!   serializes as `1000000000000000000000`, `1e-7` as `0.0000001`, `2.0`
//!   as `2`, and `-0.0` as `-0`.
//! - Non-finite values cannot be represented in `serde_json::Value`; the
//!   defensive [`CanonicalError::NotFinite`] exists so callers that build
//!   numbers outside `serde_json` still fail closed instead of silently
//!   rewriting them.
//! - Domains needing exact decimals (money-like values) must encode them
//!   as integers with a documented scale or as strings.
//!
//! Readers must re-serialize after parsing and never hash raw input bytes.

use sha2::Digest;
use sha2::Sha256;

/// Error raised when a value cannot be canonically serialized.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CanonicalError {
    /// A number in the value is not a finite binary64 (defensive: values
    /// built through `serde_json` cannot carry non-finite numbers).
    #[error("non-finite number {value} cannot be serialized canonically")]
    NotFinite {
        /// The offending value's debug form.
        value: String,
    },
}

/// Serialize `value` to the canonical JSON form (see the module rules).
pub fn canonical_json(value: &serde_json::Value) -> Result<String, CanonicalError> {
    let mut out = String::new();
    write_value(&mut out, value)?;
    Ok(out)
}

/// Serialize `value` to the canonical JSON form as owned bytes.
pub fn canonical_json_bytes(value: &serde_json::Value) -> Result<Vec<u8>, CanonicalError> {
    canonical_json(value).map(String::into_bytes)
}

/// SHA-256 over [`canonical_json`] — 64 lowercase hex characters. This is
/// the workspace's content digest: artifact-version digests, publication
/// blob naming (`<version>-<digest16>.blob`), and receipt commitments all
/// use it.
pub fn content_digest(value: &serde_json::Value) -> Result<String, CanonicalError> {
    Ok(hex::encode(Sha256::digest(
        canonical_json_bytes(value)?.as_slice(),
    )))
}

fn write_value(out: &mut String, value: &serde_json::Value) -> Result<(), CanonicalError> {
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(true) => out.push_str("true"),
        serde_json::Value::Bool(false) => out.push_str("false"),
        serde_json::Value::Number(number) => write_number(out, number)?,
        serde_json::Value::String(text) => write_string(out, text),
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(out, item)?;
            }
            out.push(']');
        }
        serde_json::Value::Object(map) => {
            out.push('{');
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_string(out, key);
                out.push(':');
                write_value(out, item)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn write_number(out: &mut String, number: &serde_json::Number) -> Result<(), CanonicalError> {
    if let Some(int) = number.as_i64() {
        out.push_str(&int.to_string());
        return Ok(());
    }
    if let Some(uint) = number.as_u64() {
        out.push_str(&uint.to_string());
        return Ok(());
    }
    let float = number.as_f64().ok_or_else(|| CanonicalError::NotFinite {
        value: number.to_string(),
    })?;
    if !float.is_finite() {
        return Err(CanonicalError::NotFinite {
            value: float.to_string(),
        });
    }
    // Rust's float `Display` is the shortest round-trip decimal and never
    // switches to exponential notation — exactly the plain-positional
    // rule. `-0.0` prints as "-0".
    out.push_str(&float.to_string());
    Ok(())
}

/// Minimal string escaping with raw non-ASCII.
fn write_string(out: &mut String, text: &str) {
    use std::fmt::Write as _;
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn canonical(text: &str) -> Result<String, CanonicalError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|error| CanonicalError::NotFinite {
                value: error.to_string(),
            })?;
        canonical_json(&value)
    }

    #[test]
    fn sorts_keys_recursively() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            canonical(r#"{"b":1,"a":{"d":2,"c":3}}"#)?,
            r#"{"a":{"c":3,"d":2},"b":1}"#
        );
        let value = json!({"é": 1, "z": 2, "a": 3});
        assert_eq!(canonical_json(&value)?, r#"{"a":3,"z":2,"é":1}"#);
        Ok(())
    }

    #[test]
    fn insertion_order_does_not_change_the_canonical_form() -> Result<(), Box<dyn std::error::Error>>
    {
        // Two JSON texts carrying the same mapping in different key orders
        // (nested included) canonicalize identically: canonicalization is a
        // function of the value, never of how it was written or built.
        let forward = r#"{"title":"Brief","version":2,"meta":{"z":1,"a":{"y":true,"x":null}}}"#;
        let shuffled = r#"{"meta":{"a":{"x":null,"y":true},"z":1},"version":2,"title":"Brief"}"#;
        assert_eq!(canonical(forward)?, canonical(shuffled)?);
        // And constructions built in different orders agree.
        let built_a = json!({"b": [1, {"d": 2, "c": 3}], "a": 0});
        let built_b = json!({"a": 0, "b": [1, {"c": 3, "d": 2}]});
        assert_eq!(canonical_json(&built_a)?, canonical_json(&built_b)?);
        Ok(())
    }

    #[test]
    fn minimal_escaping_only() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(canonical(r#""a\"b\\c\nd""#)?, r#""a\"b\\c\nd""#);
        let control = serde_json::Value::String("x\u{0001}".to_string());
        assert_eq!(canonical_json(&control)?, "\"x\\u0001\"");
        let tab = serde_json::Value::String("t\u{0009}ab".to_string());
        assert_eq!(canonical_json(&tab)?, "\"t\\tab\"");
        let del = serde_json::Value::String("unit\u{007f}del".to_string());
        assert_eq!(canonical_json(&del)?, "\"unit\u{7f}del\"");
        Ok(())
    }

    #[test]
    fn non_ascii_stays_raw_utf8() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(canonical("\"héllo → 🚀\"")?, "\"héllo → 🚀\"");
        Ok(())
    }

    #[test]
    fn integers_stay_exact() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            canonical(r#"{"n":42,"big":9007199254740993}"#)?,
            r#"{"big":9007199254740993,"n":42}"#
        );
        assert_eq!(canonical_json(&json!(u64::MAX))?, "18446744073709551615");
        assert_eq!(canonical_json(&json!(i64::MIN))?, "-9223372036854775808");
        assert_eq!(canonical("2.0")?, "2");
        Ok(())
    }

    #[test]
    fn floats_are_plain_notation_shortest_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let vectors: &[(f64, &str)] = &[
            (0.0, "0"),
            (1.0, "1"),
            (0.5, "0.5"),
            (0.1, "0.1"),
            (0.3, "0.3"),
            (123.456, "123.456"),
            (100.0, "100"),
            // Plain positional notation, never exponential.
            (1e21, "1000000000000000000000"),
            (1e-7, "0.0000001"),
        ];
        for (input, expected) in vectors {
            assert_eq!(
                &canonical_json(&serde_json::Value::from(*input))?,
                expected,
                "input {input}"
            );
        }
        let negative_zero = serde_json::Value::from(-0.0_f64);
        assert_eq!(canonical_json(&negative_zero)?, "-0");
        assert_eq!(canonical_json(&serde_json::Value::from(-0.5))?, "-0.5");
        Ok(())
    }

    #[test]
    fn parse_reserialize_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
        let inputs = [
            r#"{"a":[1,2,{"b":null}],"c":true}"#,
            r#"[1e21,0.0000001,5e-1]"#,
        ];
        for input in inputs {
            let once = canonical(input)?;
            let twice = canonical(&once)?;
            assert_eq!(once, twice, "input {input}");
        }
        assert_eq!(canonical("5e-1")?, "0.5");
        assert_eq!(canonical("0.100")?, "0.1");
        Ok(())
    }

    #[test]
    fn content_digest_is_sha256_over_the_canonical_form() -> Result<(), Box<dyn std::error::Error>>
    {
        let value = json!({"b": 1, "a": "x"});
        let digest = content_digest(&value)?;
        assert_eq!(digest.len(), 64);
        assert!(
            digest
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        // Equal values with different key order digest identically.
        let shuffled = json!({"a": "x", "b": 1});
        assert_eq!(content_digest(&shuffled)?, digest);
        // And the digest is sha256 of exactly the canonical bytes.
        let canonical = canonical_json(&value)?;
        assert_eq!(digest, hex::encode(Sha256::digest(canonical.as_bytes())));
        Ok(())
    }
}
