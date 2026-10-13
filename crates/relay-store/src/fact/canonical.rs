//! Canonical JSON for the fact format (`docs/plans/fact-format-proposal.md`).
//!
//! **One canonicalizer in the workspace** (lead ruling): the implementation
//! lives in [`relay_artifacts::canonical`] and every layer commits bytes
//! through it. This module keeps the fact layer's [`Json`] value type (used
//! to build payloads and projection folds without serde derives) and
//! delegates serialization to the shared implementation, so fact-record
//! checksums, artifact content digests, and receipt commitments are all
//! computed over the same canonical bytes.
//!
//! See `relay_artifacts::canonical` for the normative rules and the
//! supported number subset (plain-notation finite binary64 floats,
//! `u64`-bounded exact integers).

use crate::canonical::Json;
use crate::error::Result;
use crate::error::StoreError;

/// Serialize `value` to the canonical fact form as a `String`.
pub fn serialize(value: &Json) -> Result<String> {
    if let Json::Float(number) = value
        && !number.is_finite()
    {
        // The shared canonicalizer works on `serde_json::Value`, which
        // cannot carry non-finite numbers (they would silently become
        // `null`); the fact layer's value type can, so fail closed here.
        return Err(StoreError::Canonical {
            reason: format!("non-finite number {number} cannot be serialized canonically"),
        });
    }
    relay_artifacts::canonical_json(&value.to_serde()).map_err(|error| StoreError::Canonical {
        reason: error.to_string(),
    })
}

/// Serialize `value` to the canonical fact form as owned bytes.
pub fn serialize_bytes(value: &Json) -> Result<Vec<u8>> {
    serialize(value).map(String::into_bytes)
}

/// Parse JSON text into the canonical value type (accepts any valid JSON;
/// canonical form is produced by [`serialize`]).
pub fn parse(text: &str) -> Result<Json> {
    Json::parse(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(text: &str) -> Result<String> {
        serialize(&parse(text)?)
    }

    #[test]
    fn delegates_key_ordering_and_escaping() -> Result<()> {
        assert_eq!(
            canonical(r#"{"b":1,"a":{"d":2,"c":3}}"#)?,
            r#"{"a":{"c":3,"d":2},"b":1}"#
        );
        assert_eq!(canonical("\"héllo → 🚀\"")?, "\"héllo → 🚀\"");
        assert_eq!(
            serialize(&Json::Str("x\u{0001}".to_string()))?,
            "\"x\\u0001\""
        );
        Ok(())
    }

    #[test]
    fn delegates_the_number_subset() -> Result<()> {
        assert_eq!(
            canonical(r#"{"n":42,"big":9007199254740993}"#)?,
            r#"{"big":9007199254740993,"n":42}"#
        );
        assert_eq!(serialize(&Json::UInt(u64::MAX))?, "18446744073709551615");
        assert_eq!(serialize(&Json::Float(1e21))?, "1000000000000000000000");
        assert_eq!(serialize(&Json::Float(1e-7))?, "0.0000001");
        assert_eq!(serialize(&Json::Float(2.0))?, "2");
        assert_eq!(serialize(&Json::Float(-0.0))?, "-0");
        assert_eq!(canonical("5e-1")?, "0.5");
        Ok(())
    }

    #[test]
    fn non_finite_fails_closed_before_delegation() -> Result<()> {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(serialize(&Json::Float(value)).is_err());
        }
        Ok(())
    }

    #[test]
    fn round_trips_through_value_equality() -> Result<()> {
        let value = Json::object([
            ("type", Json::string("goal.declared")),
            ("payload", Json::object([("confidence", Json::Float(0.85))])),
            (
                "tags",
                Json::Array(vec![Json::string("a→b"), Json::Bool(true)]),
            ),
        ]);
        let text = serialize(&value)?;
        let reparsed = parse(&text)?;
        assert_eq!(reparsed, value);
        assert_eq!(serialize(&reparsed)?, text);
        Ok(())
    }

    #[test]
    fn agrees_with_the_shared_content_digest() -> Result<()> {
        // The fact layer's bytes and relay-artifacts' content digest are
        // computed over the same canonical form.
        use sha2::Digest;
        let value = Json::object([("b", Json::int(1)), ("a", Json::string("x"))]);
        let bytes = serialize_bytes(&value)?;
        let digest = relay_artifacts::content_digest(&value.to_serde()).map_err(|error| {
            StoreError::Canonical {
                reason: error.to_string(),
            }
        })?;
        assert_eq!(digest, hex::encode(sha2::Sha256::digest(&bytes)));
        Ok(())
    }
}
