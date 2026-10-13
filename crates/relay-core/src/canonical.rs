//! Canonical JSON serialization (`docs/event-format.md` §2).
//!
//! Canonical bytes are the single source of truth for JSONL lines and hash
//! inputs, so this module must produce byte-identical output to the
//! TypeScript implementation in `scripts/lib/canonical-json.mjs`:
//!
//! 1. UTF-8 throughout; no BOM; no insignificant whitespace.
//! 2. Object keys sorted lexicographically by Unicode code point (equivalently
//!    byte-wise over UTF-8), recursively.
//! 3. Strings escaped minimally: `"` as `\"`, backslash as `\\`, and control
//!    characters as `\b`, `\f`, `\n`, `\r`, `\t` or `\u00xx` (lowercase hex)
//!    for the remaining C0 range; non-ASCII characters are emitted as raw
//!    UTF-8, never `\u`-escaped.
//! 4. Integers serialize as exact decimal integers (no exponent, no `.0`
//!    suffix, no leading zeros or `+` sign).
//! 5. The only non-integer numbers in Relay events are `confidence` values
//!    bounded to [0, 1]; they serialize exactly like JavaScript's
//!    `Number.prototype.toString` (shortest decimal that round-trips to the
//!    same IEEE-754 binary64 value; exactly 0 and 1 as `0`/`1`).
//! 6. `true`, `false`, `null` serialize as the JSON literals.

use serde_json::Value;

/// Serialize a parsed JSON value to canonical bytes.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => write_number(out, number),
        Value::String(text) => write_string(out, text),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            // serde_json's default map is already byte-sorted; sort explicitly
            // so the result never depends on map implementation details.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_string(out, key);
                out.push(':');
                write_value(out, &map[*key]);
            }
            out.push('}');
        }
    }
}

fn write_number(out: &mut String, number: &serde_json::Number) {
    if let Some(text) = number.as_i64() {
        push_integer(out, text as i128);
    } else if let Some(text) = number.as_u64() {
        push_integer(out, text as i128);
    } else if let Some(float) = number.as_f64() {
        out.push_str(&js_number_to_string(float));
    } else {
        // serde_json never produces other number classes; fail loudly rather
        // than silently producing non-canonical bytes.
        out.push_str("null");
    }
}

fn push_integer(out: &mut String, value: i128) {
    out.push_str(&value.to_string());
}

fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let code = c as u32;
                out.push_str("\\u00");
                out.push(HEX[((code >> 4) & 0xf) as usize] as char);
                out.push(HEX[(code & 0xf) as usize] as char);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Format a finite float exactly like JavaScript's `Number.prototype.toString`.
///
/// JavaScript prints integral values without a fraction part, plain decimal
/// notation for magnitudes down to 1e-6, and exponential notation outside
/// [1e-6, 1e21]. The shortest round-tripping digits are obtained from
/// serde_json's float formatter (Ryu) and re-laid-out under those rules.
pub fn js_number_to_string(value: f64) -> String {
    if !value.is_finite() {
        // JSON cannot carry NaN/Infinity; canonical output never sees them.
        return "null".to_string();
    }
    if value == 0.0 {
        return "0".to_string();
    }
    if value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_992.0 {
        // Integral and within the exact-integer range: print as an integer
        // (this also renders -0.0 as "0", matching JavaScript).
        let as_int = value as i64;
        return as_int.to_string();
    }
    let (digits, point) = shortest_digits(value);
    lay_out_digits(&digits, point)
}

/// Shortest round-trip decimal digits and decimal-point position for `value`
/// (point = number of digits before the decimal point; <= 0 for |value| < 1),
/// derived from serde_json's (Ryu) float rendering.
fn shortest_digits(value: f64) -> (String, i32) {
    let rendered = serde_json::to_string(&value).unwrap_or_else(|_| "0".to_string());
    // Ryu output is either plain ("123.456", "0.001") or exponential ("1e-7").
    if let Some(epos) = rendered.find(['e', 'E']) {
        let mantissa = &rendered[..epos];
        let exponent: i32 = rendered[epos + 1..].parse().unwrap_or(0);
        let clean: String = mantissa.chars().filter(|c| *c != '.').collect();
        let digits = clean.trim_start_matches('0').to_string();
        let digits = if digits.is_empty() {
            "0".to_string()
        } else {
            digits
        };
        // Mantissa d.ddd → point = exponent + 1.
        (digits, exponent + 1)
    } else if let Some(dot) = rendered.find('.') {
        let int_part = &rendered[..dot];
        let frac = &rendered[dot + 1..];
        let digits: String = format!("{int_part}{frac}");
        let digits = digits.trim_start_matches('0').to_string();
        let leading_zeros = int_part.len() as i32 - int_part.trim_start_matches('0').len() as i32;
        let digits = if digits.is_empty() {
            "0".to_string()
        } else {
            digits
        };
        (digits, int_part.len() as i32 - leading_zeros)
    } else {
        let point = rendered.len() as i32;
        (rendered, point)
    }
}

/// Lay out `digits` with decimal point at `point` per ECMAScript
/// `Number::toString` rules (decimal notation for point in (-6, 21]).
fn lay_out_digits(digits: &str, point: i32) -> String {
    let k = digits.len() as i32;
    let mut out = String::new();
    if point > 21 || point <= -6 {
        // Exponential notation: d.ddde±(point-1)
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        let exponent = point - 1;
        if exponent >= 0 {
            out.push('+');
        } else {
            out.push('-');
        }
        out.push_str(&exponent.abs().to_string());
        return out;
    }
    if point <= 0 {
        out.push_str("0.");
        for _ in 0..(-point) {
            out.push('0');
        }
        out.push_str(digits);
        return out;
    }
    if point >= k {
        out.push_str(digits);
        for _ in 0..(point - k) {
            out.push('0');
        }
    } else {
        out.push_str(&digits[..point as usize]);
        out.push('.');
        out.push_str(&digits[point as usize..]);
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round_trip(text: &str) -> String {
        let value: Value = serde_json::from_str(text).unwrap();
        canonical_json(&value)
    }

    #[test]
    fn sorts_keys_recursively_and_strips_whitespace() {
        let value = json!({"b": 1, "a": {"z": [1, 2], "y": null}});
        assert_eq!(
            canonical_json(&value),
            r#"{"a":{"y":null,"z":[1,2]},"b":1}"#
        );
    }

    #[test]
    fn escapes_minimally_like_the_spec() {
        let value = json!("quote\" back\\ nl\n tab\t u\x01 é 😀 /");
        assert_eq!(
            canonical_json(&value),
            "\"quote\\\" back\\\\ nl\\n tab\\t u\\u0001 é 😀 /\""
        );
    }

    #[test]
    fn renders_integers_exactly() {
        assert_eq!(round_trip("42"), "42");
        assert_eq!(round_trip("0"), "0");
        assert_eq!(round_trip("-3"), "-3");
    }

    #[test]
    fn renders_confidence_values_like_javascript() {
        // These are the exact confidence values used across the fixture corpus.
        for (source, expected) in [
            ("0.98", "0.98"),
            ("0.95", "0.95"),
            ("0.9", "0.9"),
            ("0.6", "0.6"),
            ("0.5", "0.5"),
            ("0.3", "0.3"),
            ("0.2", "0.2"),
            ("0.1", "0.1"),
            ("0.000001", "0.000001"), // 1e-6: still plain decimal in JS
            ("0.0000001", "1e-7"),    // below 1e-6: JS switches to exponent
            ("1.0", "1"),
        ] {
            assert_eq!(round_trip(source), expected, "source {source}");
        }
    }

    #[test]
    fn handles_large_integers_and_negative_zero() {
        assert_eq!(js_number_to_string(-0.0), "0");
        assert_eq!(js_number_to_string(400_000.0), "400000");
        assert_eq!(js_number_to_string(1e21), "1e+21");
        assert_eq!(js_number_to_string(1e20), "100000000000000000000");
    }
}
