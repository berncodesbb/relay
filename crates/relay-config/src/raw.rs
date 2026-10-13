//! Order-preserving raw YAML values.
//!
//! The TypeScript reference parses each configuration layer with the `yaml`
//! npm package and validates the resulting plain JavaScript value with Zod.
//! This module is the Rust equivalent of that first half: a YAML document is
//! decoded into [`RawValue`], which keeps mapping entries in document order
//! (JavaScript object key order) and rejects duplicate mapping keys the same
//! way the `yaml` package does (`Map keys must be unique`).
//!
//! Scalar resolution follows YAML 1.2 core-schema behavior shared by the
//! `yaml` package and `serde_yaml`: `yes`/`on` stay strings, `0o17` and `0xff`
//! resolve to integers, quoted numbers stay strings, and empty values and `~`
//! are null.
//!
//! [`RawValue`] is a configuration-input type only: it never leaves this
//! crate's parsing boundary as part of a validated configuration.

use indexmap::IndexMap;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::fmt;

/// An order-preserving YAML mapping (the analogue of a parsed JavaScript
/// object).
pub type RawMapping = IndexMap<String, RawValue>;

/// A raw, unvalidated YAML value in the shape the TypeScript reference hands
/// to its schema validator.
#[derive(Clone, Debug, PartialEq)]
pub enum RawValue {
    /// A YAML null (`~`, `null`, or an empty value).
    Null,
    /// A YAML boolean.
    Bool(bool),
    /// An integer YAML scalar.
    Int(i64),
    /// A floating-point YAML scalar.
    Float(f64),
    /// A string YAML scalar (quoted or plain).
    String(String),
    /// A YAML sequence.
    Sequence(Vec<RawValue>),
    /// A YAML mapping in document order.
    Mapping(RawMapping),
}

/// Error raised while parsing YAML text; mirrors the `yaml` package's parse
/// failures, which the loader reports as `Cannot parse YAML in <path>: ...`.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct YamlError(String);

impl RawValue {
    /// Parse a YAML document.
    ///
    /// An empty or whitespace-only document parses to null, exactly like the
    /// `yaml` npm package; the configuration loader is what turns an empty
    /// *layer file* into an empty object.
    pub fn parse_yaml(text: &str) -> Result<RawValue, YamlError> {
        serde_yaml::from_str::<RawValue>(text).map_err(|error| YamlError(error.to_string()))
    }

    /// The value's JavaScript `typeof`-style name as Zod prints it in
    /// `Invalid input: expected <t>, received <name>` diagnostics.
    pub fn type_name(&self) -> &'static str {
        match self {
            RawValue::Null => "null",
            RawValue::Bool(_) => "boolean",
            RawValue::Int(_) | RawValue::Float(_) => "number",
            RawValue::String(_) => "string",
            RawValue::Sequence(_) => "array",
            RawValue::Mapping(_) => "object",
        }
    }

    /// The mapping contents, or `None` for non-mappings.
    pub fn as_mapping(&self) -> Option<&RawMapping> {
        match self {
            RawValue::Mapping(mapping) => Some(mapping),
            _ => None,
        }
    }

    /// Look up a mapping entry.
    pub fn get(&self, key: &str) -> Option<&RawValue> {
        self.as_mapping().and_then(|mapping| mapping.get(key))
    }
}

/// Formats a float the way JavaScript stringifies an object key.
fn js_number(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value.is_infinite() {
        if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
    } else {
        format!("{value}")
    }
}

/// A mapping key: scalars are stringified the way a parsed JavaScript object
/// coerces them (`1` becomes `"1"`, `true` becomes `"true"`); complex keys are
/// rejected because the TypeScript reference cannot express them either.
struct RawKey(String);

impl<'de> Deserialize<'de> for RawKey {
    fn deserialize<D>(deserializer: D) -> Result<RawKey, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RawKeyVisitor;

        impl<'de> Visitor<'de> for RawKeyVisitor {
            type Value = RawKey;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a scalar mapping key")
            }

            fn visit_unit<E>(self) -> Result<RawKey, E> {
                Ok(RawKey("null".to_string()))
            }

            fn visit_none<E>(self) -> Result<RawKey, E> {
                Ok(RawKey("null".to_string()))
            }

            fn visit_bool<E>(self, value: bool) -> Result<RawKey, E> {
                Ok(RawKey(value.to_string()))
            }

            fn visit_i64<E>(self, value: i64) -> Result<RawKey, E> {
                Ok(RawKey(value.to_string()))
            }

            fn visit_u64<E>(self, value: u64) -> Result<RawKey, E> {
                Ok(RawKey(value.to_string()))
            }

            fn visit_i128<E>(self, value: i128) -> Result<RawKey, E> {
                Ok(RawKey(js_number(value as f64)))
            }

            fn visit_u128<E>(self, value: u128) -> Result<RawKey, E> {
                Ok(RawKey(js_number(value as f64)))
            }

            fn visit_f64<E>(self, value: f64) -> Result<RawKey, E> {
                Ok(RawKey(js_number(value)))
            }

            fn visit_str<E>(self, value: &str) -> Result<RawKey, E> {
                Ok(RawKey(value.to_string()))
            }

            fn visit_string<E>(self, value: String) -> Result<RawKey, E> {
                Ok(RawKey(value))
            }

            fn visit_seq<A>(self, _seq: A) -> Result<RawKey, A::Error>
            where
                A: SeqAccess<'de>,
            {
                Err(de::Error::custom(
                    "complex YAML mapping keys are not supported",
                ))
            }

            fn visit_map<A>(self, _map: A) -> Result<RawKey, A::Error>
            where
                A: MapAccess<'de>,
            {
                Err(de::Error::custom(
                    "complex YAML mapping keys are not supported",
                ))
            }
        }

        deserializer.deserialize_any(RawKeyVisitor)
    }
}

impl<'de> Deserialize<'de> for RawValue {
    fn deserialize<D>(deserializer: D) -> Result<RawValue, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RawValueVisitor;

        impl<'de> Visitor<'de> for RawValueVisitor {
            type Value = RawValue;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("any YAML value")
            }

            fn visit_unit<E>(self) -> Result<RawValue, E> {
                Ok(RawValue::Null)
            }

            fn visit_none<E>(self) -> Result<RawValue, E> {
                Ok(RawValue::Null)
            }

            fn visit_some<D>(self, deserializer: D) -> Result<RawValue, D::Error>
            where
                D: Deserializer<'de>,
            {
                RawValue::deserialize(deserializer)
            }

            fn visit_bool<E>(self, value: bool) -> Result<RawValue, E> {
                Ok(RawValue::Bool(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<RawValue, E> {
                Ok(RawValue::Int(value))
            }

            fn visit_u64<E>(self, value: u64) -> Result<RawValue, E> {
                Ok(match i64::try_from(value) {
                    Ok(small) => RawValue::Int(small),
                    Err(_) => RawValue::Float(value as f64),
                })
            }

            fn visit_i128<E>(self, value: i128) -> Result<RawValue, E> {
                // JavaScript numbers are doubles; the `yaml` package yields a
                // float for scalars outside the safe integer range too.
                i64::try_from(value).map_or_else(
                    |_| Ok(RawValue::Float(value as f64)),
                    |v| Ok(RawValue::Int(v)),
                )
            }

            fn visit_u128<E>(self, value: u128) -> Result<RawValue, E> {
                Ok(match u64::try_from(value) {
                    Ok(small) => match i64::try_from(small) {
                        Ok(tiny) => RawValue::Int(tiny),
                        Err(_) => RawValue::Float(small as f64),
                    },
                    Err(_) => RawValue::Float(value as f64),
                })
            }

            fn visit_f64<E>(self, value: f64) -> Result<RawValue, E> {
                Ok(RawValue::Float(value))
            }

            fn visit_str<E>(self, value: &str) -> Result<RawValue, E> {
                Ok(RawValue::String(value.to_string()))
            }

            fn visit_string<E>(self, value: String) -> Result<RawValue, E> {
                Ok(RawValue::String(value))
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<RawValue, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element::<RawValue>()? {
                    items.push(item);
                }
                Ok(RawValue::Sequence(items))
            }

            fn visit_map<A>(self, mut map: A) -> Result<RawValue, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut mapping = RawMapping::new();
                while let Some((key, value)) = map.next_entry::<RawKey, RawValue>()? {
                    if mapping.insert(key.0, value).is_some() {
                        // The `yaml` npm package rejects duplicate keys with
                        // `Map keys must be unique`; silently taking the last
                        // entry would change configuration semantics.
                        return Err(de::Error::custom(
                            "Map keys must be unique (duplicate mapping key)",
                        ));
                    }
                }
                Ok(RawValue::Mapping(mapping))
            }
        }

        deserializer.deserialize_any(RawValueVisitor)
    }
}
