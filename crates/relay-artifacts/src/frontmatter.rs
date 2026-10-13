//! YAML frontmatter for artifact Markdown files.
//!
//! Two halves, both byte-compatible with the TypeScript reference
//! implementation:
//!
//! * a **writer** reproducing exactly what the `yaml` npm package (v2,
//!   default options: `lineWidth: 80`, `minContentWidth: 20`) emits for the
//!   flat frontmatter mapping the TypeScript store writes — plain, folded,
//!   single-quoted, and double-quoted scalar styles, including its flow-line
//!   folding algorithm;
//! * a **reader** reproducing the store's `splitFrontmatter` behavior:
//!   split on the first two `---` lines, parse a flat YAML mapping (the
//!   subset the writer can emit), strip leading blank lines from the body.
//!
//! Folding and line limits are computed in UTF-16 code units to match
//! JavaScript string indexing; slices are decoded back with
//! `String::from_utf16_lossy`, which also matches JavaScript's lone-surrogate
//! → U+FFFD behavior on write.

use std::collections::HashSet;
use std::fmt::Write as _;

use relay_core::ArtifactKind;
use serde_json::{Map, Value};

use crate::error::InvalidArtifactFileError;

/// The `yaml` package default line width.
const LINE_WIDTH: usize = 80;
/// The `yaml` package default minimum content width.
const MIN_CONTENT_WIDTH: usize = 20;
/// Indentation of a block-mapping value continuation at the document root
/// (one `indentStep` of two spaces below the key's own indent of `''`).
const VALUE_INDENT: &str = "  ";

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// The frontmatter fields of a stored artifact version, in write order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frontmatter<'a> {
    /// Artifact id (`art_…`).
    pub id: &'a str,
    /// Artifact kind.
    pub kind: ArtifactKind,
    /// Version number (1-based).
    pub version: u64,
    /// Draft title (single line, at most 200 characters).
    pub title: &'a str,
    /// Lifecycle status (`draft`).
    pub status: &'a str,
    /// Creation timestamp (RFC 3339).
    pub created_at: &'a str,
    /// Last-update timestamp (RFC 3339).
    pub updated_at: &'a str,
}

impl Frontmatter<'_> {
    /// Render the YAML frontmatter exactly as `stringifyYaml(...).trimEnd()`
    /// in the TypeScript store.
    pub fn to_yaml(&self) -> String {
        let mut lines: Vec<String> = Vec::with_capacity(7);
        lines.push(scalar_line("id", self.id));
        lines.push(scalar_line("kind", self.kind.as_str()));
        lines.push(format!("version: {}", self.version));
        lines.push(scalar_line("title", self.title));
        lines.push(scalar_line("status", self.status));
        lines.push(scalar_line("createdAt", self.created_at));
        lines.push(scalar_line("updatedAt", self.updated_at));
        lines.join("\n")
    }
}

/// Render one `key: value` line for a plain-safe-or-quoted string scalar.
fn scalar_line(key: &str, value: &str) -> String {
    format!("{key}: {}", stringify_scalar_value(key, value))
}

/// Render a full artifact file: frontmatter delimiters, YAML block, blank
/// line, then the rendered body. Byte-identical to the TypeScript
/// `` `---\n${yaml}\n---\n\n${body}` `` composition.
pub fn render_artifact_file(frontmatter: &Frontmatter<'_>, body: &str) -> String {
    format!("---\n{}\n---\n\n{body}", frontmatter.to_yaml())
}

/// Stringify one scalar value the way `yaml`'s `stringifyString` does for a
/// block-mapping value at the document root (implicit key `false`, `inFlow`
/// `false`, plain style by default).
fn stringify_scalar_value(key: &str, value: &str) -> String {
    // `indentAtStart` is `"<key>:".length + 1` (the separator space).
    let indent_at_start = key.chars().count() + 2;
    // Control characters (and unpaired surrogates, impossible in Rust strings)
    // force double quotes before any other style decision.
    if value
        .chars()
        .any(|c| matches!(c, '\u{00}'..='\u{08}' | '\u{0b}'..='\u{1f}' | '\u{7f}'..='\u{9f}'))
    {
        return double_quoted(value, indent_at_start);
    }
    // plainString: quote when the plain style is not allowed or the plain
    // form would parse back as a non-string YAML type; otherwise fold.
    if plain_not_allowed(value) || resolves_as_non_string(value) {
        return quoted_string(value, indent_at_start);
    }
    fold_flow_lines(value, FoldMode::Flow, indent_at_start)
}

/// The `plainString` "not allowed" test (single-line values; the multiline
/// branches of the original are unreachable because titles cannot contain
/// newlines).
fn plain_not_allowed(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    // First character must not be an indicator.
    if matches!(
        first,
        '\n' | '\t'
            | ' '
            | ','
            | '['
            | ']'
            | '{'
            | '}'
            | '#'
            | '&'
            | '*'
            | '!'
            | '|'
            | '>'
            | '\''
            | '"'
            | '%'
            | '@'
            | '`'
    ) {
        return true;
    }
    let second = chars.next();
    // A bare `-`/`?`, or `-`/`?` followed by a space/tab.
    if (first == '-' || first == '?')
        && (second.is_none() || matches!(second, Some(' ') | Some('\t')))
    {
        return true;
    }
    // `: ` (or `:\t`) anywhere, a space/tab before a newline, a space/tab
    // before `#`, or a trailing space/tab/colon.
    let chars: Vec<char> = value.chars().collect();
    for window in chars.windows(2) {
        let (a, b) = (window[0], window[1]);
        if (a == ':' || a == '\n') && (b == ' ' || b == '\t') {
            return true;
        }
        if (a == ' ' || a == '\t') && b == '\n' {
            return true;
        }
        if (a == '\n' || a == '\t' || a == ' ') && b == '#' {
            return true;
        }
    }
    matches!(chars[chars.len() - 1], ' ' | '\t' | ':' | '\n')
}

/// Whether the plain spelling of `value` would resolve as a non-string type
/// under the YAML core schema (the `actualString` tag test in `plainString`).
fn resolves_as_non_string(value: &str) -> bool {
    // The null tag test /^(?:~|[Nn]ull|NULL)?$/ also matches the empty string.
    value.is_empty()
        || value == "~"
        || value == "null"
        || value == "Null"
        || value == "NULL"
        || matches!(
            resolve_plain_scalar(value),
            PlainResolution::Bool(_) | PlainResolution::Integer(_) | PlainResolution::Float(_)
        )
}

/// `quotedString`: pick single quotes only when the value contains a double
/// quote but no single quote (the package's `singleQuote` default is null,
/// which falls back to double quotes).
fn quoted_string(value: &str, indent_at_start: usize) -> String {
    let has_double = value.contains('"');
    let has_single = value.contains('\'');
    if has_double && !has_single {
        single_quoted(value, indent_at_start)
    } else {
        double_quoted(value, indent_at_start)
    }
}

fn single_quoted(value: &str, indent_at_start: usize) -> String {
    let quoted = format!("'{}'", value.replace('\'', "''"));
    fold_flow_lines(&quoted, FoldMode::Flow, indent_at_start)
}

/// `doubleQuotedString` for single-line values: `JSON.stringify` (with the
/// package's escape shortening) followed by quoted-mode folding. The
/// multi-line `\n`-splitting branch is unreachable for frontmatter values.
fn double_quoted(value: &str, indent_at_start: usize) -> String {
    let escaped = json_escape_with_shortening(value);
    fold_flow_lines(&escaped, FoldMode::Quoted, indent_at_start)
}

/// `JSON.stringify` escaping plus the `yaml` package's `\u00XX` shortening
/// (`\0`, `\a`, `\v`, `\e`, `\xXX`).
fn json_escape_with_shortening(value: &str) -> String {
    let json = json_escape(value);
    // Walk the escaped text and shorten `\uXXXX` sequences. Other escapes
    // (`\"`, `\\`, `\b`, `\f`, `\t`, `\n`, `\r`) pass through.
    let bytes = json.as_bytes();
    let mut out = String::with_capacity(json.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 1 < bytes.len() && bytes[index + 1] == b'u' {
            let hex = &json[index + 2..index + 6];
            match hex {
                "0000" => out.push_str("\\0"),
                "0007" => out.push_str("\\a"),
                "000b" => out.push_str("\\v"),
                "001b" => out.push_str("\\e"),
                "0085" => out.push_str("\\N"),
                "00a0" => out.push_str("\\_"),
                "2028" => out.push_str("\\L"),
                "2029" => out.push_str("\\P"),
                _ if hex.starts_with("00") => {
                    let _ = write!(out, "\\x{}", &hex[2..]);
                }
                _ => out.push_str(&json[index..index + 6]),
            }
            index += 6;
        } else {
            let ch = json[index..].chars().next();
            match ch {
                Some(c) => {
                    out.push(c);
                    index += c.len_utf8();
                }
                None => break,
            }
        }
    }
    out
}

/// `JSON.stringify` string escaping (no non-ASCII escaping; Rust strings
/// cannot contain lone surrogates).
fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
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
    out
}

/// Fold mode: `'flow'` (plain and single-quoted) or `'quoted'`
/// (double-quoted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FoldMode {
    /// Fold plain and single-quoted scalars.
    Flow,
    /// Fold double-quoted scalars (escape-aware).
    Quoted,
}

/// Port of the `yaml` package's `foldFlowLines` for `FOLD_FLOW` and
/// `FOLD_QUOTED` (the `FOLD_BLOCK` mode is unreachable for single-line
/// values), with `indent: "  "`, `lineWidth: 80`, `minContentWidth: 20`.
///
/// Index arithmetic uses UTF-16 code units to match JavaScript exactly.
fn fold_flow_lines(text: &str, mode: FoldMode, indent_at_start: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let indent_len = VALUE_INDENT.len();
    let mut min_content_width = MIN_CONTENT_WIDTH;
    if LINE_WIDTH < min_content_width {
        min_content_width = 0;
    }
    let end_step = std::cmp::max(1 + min_content_width, 1 + LINE_WIDTH - indent_len);
    if units.len() <= end_step {
        return text.to_string();
    }
    let mut folds: Vec<usize> = Vec::new();
    let mut escaped_folds: HashSet<usize> = HashSet::new();
    let mut end: usize = LINE_WIDTH - indent_len;
    if indent_at_start > LINE_WIDTH - std::cmp::max(2, min_content_width) {
        folds.push(0);
    } else {
        end = LINE_WIDTH - indent_at_start;
    }
    let mut split: Option<usize> = None;
    let mut prev: Option<u16> = None;
    let mut i: isize = -1;
    loop {
        i += 1;
        let idx = i as usize;
        if idx >= units.len() {
            break;
        }
        let mut ch = units[idx];
        if ch == 0 {
            // JS `for (...; ch = text[i];)` stops at NUL.
            break;
        }
        let mut esc_start: isize = -1;
        let mut esc_end: isize = -1;
        if mode == FoldMode::Quoted && ch == u16::from(b'\\') {
            esc_start = i;
            let next = units.get(idx + 1).copied().unwrap_or(0);
            i += match u32::from(next) {
                0x78 => 3, // x
                0x75 => 5, // u
                0x55 => 9, // U
                _ => 1,
            };
            esc_end = i;
        }
        if ch == u16::from(b'\n') {
            end = (i + indent_len as isize + end_step as isize) as usize;
            split = None;
        } else {
            if ch == u16::from(b' ')
                && prev.is_some_and(|p| {
                    p != u16::from(b' ') && p != u16::from(b'\n') && p != u16::from(b'\t')
                })
                && let Some(&next) = units.get(idx + 1)
                && next != u16::from(b' ')
                && next != u16::from(b'\n')
                && next != u16::from(b'\t')
            {
                split = Some(idx);
            }
            if idx >= end {
                if let Some(fold) = split.take() {
                    folds.push(fold);
                    end = fold + end_step;
                } else if mode == FoldMode::Quoted {
                    // Consume a trailing whitespace run (as JS does), then
                    // force a fold just before the overflow point.
                    while prev == Some(u16::from(b' ')) || prev == Some(u16::from(b'\t')) {
                        prev = Some(ch);
                        i += 1;
                        ch = units.get(i as usize).copied().unwrap_or(0);
                    }
                    // With our constants the forced fold index is always
                    // non-negative (the opening quote guarantees escapes do
                    // not start at index 0 and `end >= 60`); saturate to keep
                    // the arithmetic panic-free in debug builds.
                    let j = if i > esc_end + 1 {
                        (i - 2).max(0) as usize
                    } else {
                        (esc_start - 1).max(0) as usize
                    };
                    if escaped_folds.contains(&j) {
                        return text.to_string();
                    }
                    folds.push(j);
                    escaped_folds.insert(j);
                    end = j.saturating_add(end_step);
                    split = None;
                }
            }
        }
        prev = Some(ch);
    }
    if folds.is_empty() {
        return text.to_string();
    }
    let mut out: Vec<u16> = units[..folds[0]].to_vec();
    for (position, &fold) in folds.iter().enumerate() {
        let limit = folds.get(position + 1).copied().unwrap_or(units.len());
        if fold == 0 {
            out.clear();
            out.push(u16::from(b'\n'));
            out.extend(VALUE_INDENT.encode_utf16());
            out.extend_from_slice(&units[..limit]);
        } else {
            if mode == FoldMode::Quoted && escaped_folds.contains(&fold) {
                out.push(units[fold]);
                out.push(u16::from(b'\\'));
            }
            out.push(u16::from(b'\n'));
            out.extend(VALUE_INDENT.encode_utf16());
            let start = std::cmp::min(fold + 1, units.len());
            out.extend_from_slice(&units[start..std::cmp::max(start, limit)]);
        }
    }
    String::from_utf16_lossy(&out)
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// A parsed artifact file: frontmatter mapping plus body.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedArtifactFile {
    /// The YAML frontmatter mapping (flat, scalar values).
    pub frontmatter: Map<String, Value>,
    /// The rendered Markdown body (leading blank lines stripped).
    pub body: String,
}

/// Split an artifact file into frontmatter and body, porting the TypeScript
/// store's `splitFrontmatter` diagnostics exactly.
pub fn split_frontmatter(
    content: &str,
    stored_path: &str,
) -> Result<ParsedArtifactFile, InvalidArtifactFileError> {
    let lines: Vec<&str> = content.split('\n').collect();
    if lines.first().copied().unwrap_or_default().trim() != "---" {
        return Err(InvalidArtifactFileError {
            path: stored_path.to_string(),
            detail: "missing opening \"---\" frontmatter delimiter".to_string(),
        });
    }
    let close_index = lines.iter().skip(1).position(|line| line.trim() == "---");
    let Some(offset) = close_index else {
        return Err(InvalidArtifactFileError {
            path: stored_path.to_string(),
            detail: "missing closing \"---\" frontmatter delimiter".to_string(),
        });
    };
    let close_index = offset + 1;
    let yaml_text = lines[1..close_index].join("\n");
    let mapping = parse_yaml_mapping(&yaml_text).map_err(|detail| InvalidArtifactFileError {
        path: stored_path.to_string(),
        detail: format!("invalid YAML frontmatter: {detail}"),
    })?;
    let Some(frontmatter) = mapping else {
        return Err(InvalidArtifactFileError {
            path: stored_path.to_string(),
            detail: "frontmatter is not a YAML mapping".to_string(),
        });
    };
    let body = lines[close_index + 1..]
        .join("\n")
        .trim_start_matches('\n')
        .to_string();
    Ok(ParsedArtifactFile { frontmatter, body })
}

/// How a plain (unquoted) scalar resolves under the YAML core schema.
#[derive(Debug, Clone, Copy, PartialEq)]
enum PlainResolution {
    /// Stays a string.
    String,
    /// Resolves as a boolean.
    Bool(bool),
    /// Resolves as an integer.
    Integer(i64),
    /// Resolves as a float.
    Float(f64),
}

/// Resolve a plain scalar's type with the core-schema tag regexes.
fn resolve_plain_scalar(text: &str) -> PlainResolution {
    // bool: /^(?:[Tt]rue|TRUE|[Ff]alse|FALSE)$/
    match text {
        "true" | "True" | "TRUE" => return PlainResolution::Bool(true),
        "false" | "False" | "FALSE" => return PlainResolution::Bool(false),
        _ => {}
    }
    if let Some(value) = parse_core_integer(text) {
        return PlainResolution::Integer(value);
    }
    if let Some(value) = parse_core_float(text) {
        return PlainResolution::Float(value);
    }
    PlainResolution::String
}

/// int: /^[-+]?[0-9]+$/, intOct: /^0o[0-7]+$/, intHex: /^0x[0-9a-fA-F]+$/.
fn parse_core_integer(text: &str) -> Option<i64> {
    let unsigned = strip_sign(text)?;
    if let Some(octal) = unsigned.strip_prefix("0o") {
        if !octal.is_empty() && octal.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
            return i64::from_str_radix(octal, 8).ok();
        }
        return None;
    }
    if let Some(hex) = unsigned.strip_prefix("0x") {
        if !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return i64::from_str_radix(hex, 16).ok();
        }
        return None;
    }
    if is_digits(unsigned) {
        return unsigned.parse::<i64>().ok();
    }
    None
}

/// float: /^[-+]?(?:\.[0-9]+|[0-9]+\.[0-9]*)$/,
/// floatExp: /^[-+]?(?:\.[0-9]+|[0-9]+(?:\.[0-9]*)?)[eE][-+]?[0-9]+$/,
/// floatNaN: /^(?:[-+]?\.(?:inf|Inf|INF)|\.nan|\.NaN|\.NAN)$/ (the
/// NaN/infinity spellings cannot round-trip through JSON numbers and stay
/// strings here; the writer always quotes them anyway).
fn parse_core_float(text: &str) -> Option<f64> {
    let unsigned = strip_sign(text)?;
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(position) => (&unsigned[..position], Some(&unsigned[position + 1..])),
        None => (unsigned, None),
    };
    let mantissa_kind = if let Some(digits) = mantissa.strip_prefix('.') {
        if is_digits(digits) {
            Mantissa::DotDigits
        } else {
            return None;
        }
    } else if let Some((whole, fraction)) = mantissa.split_once('.') {
        if is_digits(whole) && fraction.bytes().all(|b| b.is_ascii_digit()) {
            Mantissa::DotDigits
        } else {
            return None;
        }
    } else if is_digits(mantissa) {
        Mantissa::WholeOnly
    } else {
        return None;
    };
    match exponent {
        None => match mantissa_kind {
            // Without an exponent the mantissa must contain a dot to be a
            // float (an integer-only mantissa is the int tag instead).
            Mantissa::WholeOnly => None,
            Mantissa::DotDigits => text.parse::<f64>().ok(),
        },
        Some(exponent) => {
            let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
            if is_digits(digits) {
                text.parse::<f64>().ok()
            } else {
                None
            }
        }
    }
}

enum Mantissa {
    WholeOnly,
    DotDigits,
}

fn strip_sign(text: &str) -> Option<&str> {
    match text.as_bytes().first() {
        Some(b'-') | Some(b'+') => Some(&text[1..]),
        _ => Some(text),
    }
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// Parse the flat YAML mapping subset emitted by the writer. Returns `Ok(None)`
/// for an empty document (which the caller reports as "not a YAML mapping").
fn parse_yaml_mapping(text: &str) -> Result<Option<Map<String, Value>>, String> {
    let mut map = Map::new();
    let mut lines = text.split('\n').peekable();
    // Skip leading blank lines.
    while let Some(line) = lines.peek() {
        if line.trim().is_empty() {
            lines.next();
        } else {
            break;
        }
    }
    if lines.peek().is_none() {
        return Ok(None);
    }
    while let Some(line) = lines.next() {
        if line.trim().is_empty() {
            // Blank line: the writer never emits one inside frontmatter; treat
            // it as a terminator and require nothing else to follow.
            if lines.peek().is_some_and(|rest| !rest.trim().is_empty()) {
                return Err(format!(
                    "unexpected content after blank line ({:?})",
                    line.trim_end()
                ));
            }
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent != 0 {
            return Err(format!("unexpected indentation ({:?})", line.trim_end()));
        }
        let trimmed = line.trim_start();
        let Some((key, first_segment)) = trimmed.split_once(':') else {
            return Err(format!("expected \"key: value\" ({:?})", trimmed));
        };
        let key = key.trim_end();
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!("unsupported key syntax ({key:?})"));
        }
        let first_segment = first_segment.strip_prefix(' ').unwrap_or(first_segment);
        // Gather indented continuation lines belonging to this value.
        let mut segments: Vec<String> = vec![first_segment.trim_end().to_string()];
        while let Some(next) = lines.peek() {
            let next_trim = next.trim();
            if next_trim.is_empty() {
                break;
            }
            let next_indent = next.len() - next_trim_start_len(next);
            if next_indent <= indent {
                break;
            }
            segments.push(next_trim.to_string());
            lines.next();
        }
        if map.contains_key(key) {
            return Err(format!("map keys must be unique (duplicate key {key:?})"));
        }
        map.insert(key.to_string(), parse_scalar_value(&segments)?);
    }
    Ok(Some(map))
}

fn next_trim_start_len(line: &str) -> usize {
    line.trim_start().len()
}

/// Join the folded segments of one scalar (folding turns the line break into a
/// single space) and interpret the scalar.
fn parse_scalar_value(segments: &[String]) -> Result<Value, String> {
    let joined = segments
        .iter()
        .filter(|segment| !segment.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        return Ok(Value::Null);
    }
    if let Some(single) = joined.strip_prefix('\'') {
        let inner = single
            .strip_suffix('\'')
            .ok_or_else(|| format!("unterminated single-quoted scalar ({joined:?})"))?;
        return Ok(Value::String(inner.replace("''", "'")));
    }
    if let Some(double) = joined.strip_prefix('"') {
        let inner = double
            .strip_suffix('"')
            .ok_or_else(|| format!("unterminated double-quoted scalar ({joined:?})"))?;
        return Ok(Value::String(unescape_double_quoted(inner)?));
    }
    Ok(match resolve_plain_scalar(&joined) {
        PlainResolution::Bool(flag) => Value::Bool(flag),
        PlainResolution::Integer(value) => Value::Number(serde_json::Number::from(value)),
        PlainResolution::Float(value) => serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        PlainResolution::String => {
            if joined == "~" || joined == "null" || joined == "Null" || joined == "NULL" {
                Value::Null
            } else {
                Value::String(joined)
            }
        }
    })
}

/// Process the escape set the writer can emit inside double-quoted scalars.
fn unescape_double_quoted(inner: &str) -> Result<String, String> {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        let escape = chars.next().ok_or("trailing backslash in quoted scalar")?;
        match escape {
            '0' => out.push('\u{00}'),
            'a' => out.push('\u{07}'),
            'b' => out.push('\u{08}'),
            't' => out.push('\t'),
            'n' => out.push('\n'),
            'v' => out.push('\u{0b}'),
            'f' => out.push('\u{0c}'),
            'r' => out.push('\r'),
            'e' => out.push('\u{1b}'),
            ' ' => out.push(' '),
            '"' => out.push('"'),
            '/' => out.push('/'),
            '\\' => out.push('\\'),
            'N' => out.push('\u{85}'),
            '_' => out.push('\u{a0}'),
            'L' => out.push('\u{2028}'),
            'P' => out.push('\u{2029}'),
            'x' => {
                let hex: String = chars.by_ref().take(2).collect();
                let code = u32::from_str_radix(&hex, 16)
                    .map_err(|_| format!("invalid \\x escape (\\x{hex})"))?;
                out.push(char::from_u32(code).ok_or("invalid \\x escape code point")?);
            }
            'u' => {
                let hex: String = chars.by_ref().take(4).collect();
                let code = u32::from_str_radix(&hex, 16)
                    .map_err(|_| format!("invalid \\u escape (\\u{hex})"))?;
                out.push(char::from_u32(code).ok_or("invalid \\u escape code point")?);
            }
            'U' => {
                let hex: String = chars.by_ref().take(8).collect();
                let code = u32::from_str_radix(&hex, 16)
                    .map_err(|_| format!("invalid \\U escape (\\U{hex})"))?;
                out.push(char::from_u32(code).ok_or("invalid \\U escape code point")?);
            }
            other => return Err(format!("unsupported escape (\\{other})")),
        }
    }
    Ok(out)
}
