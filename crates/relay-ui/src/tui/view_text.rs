//! Defensive view-to-text rendering for the terminal interface panels.
//! Ported from `src/tui/app.tsx` (retired TypeScript tree, removed from the
//! repository 2026-10-14): `renderViewText`, `renderInspectBody`,
//! `renderRecord`, `orderEntries`, `humanizeKey`, and the preferred-key
//! orders): controller views render as humanized `Key: value` blocks with
//! arrays joined inline and nested records indented, ordering preferred keys
//! first.
//!
//! Views are represented as [`Loose`] records — insertion-ordered key/value
//! trees — rather than serialized JSON, so key order is part of the data and
//! never depends on a JSON map implementation.

use crate::views::{StatusView, WhyView};

/// Preferred key order for the `/status` panel (normalized: lowercase,
/// alphanumeric only), from `STATUS_KEY_ORDER`.
pub const STATUS_KEY_ORDER: [&str; 10] = [
    "goal",
    "mode",
    "stage",
    "progress",
    "activework",
    "blockers",
    "artifacts",
    "changedfiles",
    "nextaction",
    "autonomy",
];

/// Preferred key order for the `/why` panel, from `WHY_KEY_ORDER`.
pub const WHY_KEY_ORDER: [&str; 8] = [
    "decision",
    "question",
    "recommendation",
    "reason",
    "evidence",
    "alternatives",
    "mode",
    "autonomy",
];

/// Preferred key order for the `/inspect` panel, from `INSPECT_KEY_ORDER`.
pub const INSPECT_KEY_ORDER: [&str; 8] = [
    "title",
    "kind",
    "version",
    "status",
    "path",
    "updatedat",
    "markdown",
    "body",
];

/// The `/help` command reference shown in the pause-help panel.
pub const HELP_TEXT: &str = "\
commands:
  /status                 where things stand
  /why                    last routing or transition decision
  /inspect <artifact>     read an artifact markdown
  /mode <mode>            override the active workflow mode
  /auto <bound>           off | next-checkpoint | until:<mode> | workflow
  /pause [reason]         stop automatic advancement
  /resume                 continue
  /approve [artifact]     approve an artifact
  /revise <kind> <why>    send an approved artifact back
  /help                   this list
  /quit                   leave the session
anything else is sent to relay as conversational input.";

/// A loose, insertion-ordered view value. `format_scalar` semantics match the
/// reference: strings render when non-empty, numbers and booleans as text,
/// everything else as `None`.
#[derive(Debug, Clone, PartialEq)]
pub enum Loose {
    /// `null` / absent.
    Null,
    /// A boolean.
    Bool(bool),
    /// An integral number.
    Int(i64),
    /// A string.
    Str(String),
    /// An array.
    Array(Vec<Loose>),
    /// An insertion-ordered record.
    Record(Vec<(String, Loose)>),
}

impl Loose {
    /// Build a record from key/value pairs.
    pub fn record(entries: Vec<(&str, Loose)>) -> Loose {
        Loose::Record(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    /// A string value.
    pub fn str(text: impl Into<String>) -> Loose {
        Loose::Str(text.into())
    }

    /// Look a record key up (first match, like the JS `in`/index reads).
    pub fn get(&self, key: &str) -> Option<&Loose> {
        match self {
            Loose::Record(entries) => entries
                .iter()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }
}

/// Convert a status view into its loose panel representation, preserving the
/// view's field order.
pub fn loose_status(view: &StatusView) -> Loose {
    Loose::record(vec![
        ("goal", Loose::str(view.goal.clone())),
        ("mode", Loose::str(view.mode.as_str())),
        ("autonomy", Loose::str(view.autonomy.clone())),
        (
            "paused",
            match &view.paused {
                None => Loose::Null,
                Some(paused) => Loose::record(vec![
                    ("by", Loose::str(paused.by.clone())),
                    ("reason", Loose::str(paused.reason.clone())),
                    (
                        "question",
                        match &paused.question {
                            None => Loose::Null,
                            Some(question) => Loose::str(question.clone()),
                        },
                    ),
                    (
                        "options",
                        Loose::Array(
                            paused
                                .options
                                .iter()
                                .map(|option| Loose::str(option.clone()))
                                .collect(),
                        ),
                    ),
                ]),
            },
        ),
        (
            "progress",
            Loose::record(vec![
                ("stage", Loose::Int(view.progress.stage as i64)),
                ("total", Loose::Int(view.progress.total as i64)),
            ]),
        ),
        (
            "artifacts",
            Loose::Array(
                view.artifacts
                    .iter()
                    .map(|artifact| {
                        Loose::record(vec![
                            ("artifactId", Loose::str(artifact.artifact_id.clone())),
                            ("kind", Loose::str(artifact.kind.clone())),
                            ("version", Loose::Int(artifact.version)),
                            ("status", Loose::str(artifact.status.clone())),
                            ("title", Loose::str(artifact.title.clone())),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "staleArtifacts",
            Loose::Array(
                view.stale_artifacts
                    .iter()
                    .map(|id| Loose::str(id.clone()))
                    .collect(),
            ),
        ),
        (
            "changedFiles",
            Loose::Array(
                view.changed_files
                    .iter()
                    .map(|file| Loose::str(file.clone()))
                    .collect(),
            ),
        ),
        (
            "blockers",
            Loose::Array(
                view.blockers
                    .iter()
                    .map(|blocker| Loose::str(blocker.clone()))
                    .collect(),
            ),
        ),
        ("nextAction", Loose::str(view.next_action.clone())),
        (
            "stats",
            Loose::record(vec![
                ("events", Loose::Int(view.stats.events)),
                ("modelCalls", Loose::Int(view.stats.model_calls)),
                ("toolCalls", Loose::Int(view.stats.tool_calls)),
            ]),
        ),
    ])
}

/// Convert a why view into its loose panel representation.
pub fn loose_why(view: &WhyView) -> Loose {
    Loose::record(vec![
        ("mode", Loose::str(view.mode.as_str())),
        (
            "lastOutcome",
            match &view.last_outcome {
                None => Loose::Null,
                Some(outcome) => Loose::record(vec![
                    ("outcome", Loose::str(outcome.outcome.clone())),
                    ("reason", Loose::str(outcome.reason.clone())),
                    (
                        "evidence",
                        Loose::Array(
                            outcome
                                .evidence
                                .iter()
                                .map(|item| Loose::str(item.clone()))
                                .collect(),
                        ),
                    ),
                    ("at", Loose::str(outcome.at.clone())),
                ]),
            },
        ),
        (
            "recentTransitions",
            Loose::Array(
                view.recent_transitions
                    .iter()
                    .map(|transition| {
                        Loose::record(vec![
                            (
                                "from",
                                match transition.from {
                                    None => Loose::Null,
                                    Some(mode) => Loose::str(mode.as_str()),
                                },
                            ),
                            ("to", Loose::str(transition.to.as_str())),
                            ("reason", Loose::str(transition.reason.clone())),
                            ("at", Loose::str(transition.at.clone())),
                            ("source", Loose::str(transition.source.clone())),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "rejectedTransitions",
            Loose::Array(
                view.rejected_transitions
                    .iter()
                    .map(|rejected| {
                        Loose::record(vec![
                            ("from", Loose::str(rejected.from.as_str())),
                            ("to", Loose::str(rejected.to.as_str())),
                            ("reason", Loose::str(rejected.reason.clone())),
                            ("at", Loose::str(rejected.at.clone())),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "routing",
            Loose::Array(
                view.routing
                    .iter()
                    .map(|row| {
                        Loose::record(vec![
                            ("provider", Loose::str(row.provider.clone())),
                            ("model", Loose::str(row.model.clone())),
                            ("purpose", Loose::str(row.purpose.clone())),
                            (
                                "role",
                                match &row.role {
                                    None => Loose::Null,
                                    Some(role) => Loose::str(role.clone()),
                                },
                            ),
                            ("at", Loose::str(row.at.clone())),
                            ("ok", Loose::Bool(row.ok)),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "staleArtifacts",
            Loose::Array(
                view.stale_artifacts
                    .iter()
                    .map(|id| Loose::str(id.clone()))
                    .collect(),
            ),
        ),
        (
            "pausedReason",
            match &view.paused_reason {
                None => Loose::Null,
                Some(reason) => Loose::str(reason.clone()),
            },
        ),
    ])
}

/// The loose representation of an inspect result (`markdown`/`body` first,
/// per the reference's key probe order: markdown, body, content, text, then
/// the field order).
pub fn loose_inspect(frontmatter_pairs: &[(String, Loose)], body: &str, path: &str) -> Loose {
    let mut entries = vec![
        (
            "markdown".to_string(),
            if body.is_empty() {
                Loose::Null
            } else {
                Loose::str(body)
            },
        ),
        ("path".to_string(), Loose::str(path)),
    ];
    for (key, value) in frontmatter_pairs {
        entries.push((key.clone(), value.clone()));
    }
    Loose::Record(entries)
}

/// Normalize a key the way `normalizeKey` does: lowercase, alphanumeric only.
fn normalize_key(key: &str) -> String {
    key.to_lowercase()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect()
}

/// `humanizeKey`: split camelCase and separator runs, capitalize the first
/// letter.
fn humanize_key(key: &str) -> String {
    let mut spaced = String::new();
    let mut previous_lower_or_digit = false;
    for ch in key.chars() {
        if ch.is_ascii_uppercase() {
            if previous_lower_or_digit {
                spaced.push(' ');
            }
            spaced.push(ch);
            previous_lower_or_digit = false;
        } else if ch == '_' || ch == '-' {
            if !spaced.ends_with(' ') && !spaced.is_empty() {
                spaced.push(' ');
            }
            previous_lower_or_digit = false;
        } else {
            spaced.push(ch);
            previous_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    let mut characters = spaced.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

fn format_scalar(value: &Loose) -> Option<String> {
    match value {
        Loose::Null => None,
        Loose::Bool(flag) => Some(flag.to_string()),
        Loose::Int(number) => Some(number.to_string()),
        Loose::Str(text) => {
            if text.is_empty() {
                None
            } else {
                Some(text.clone())
            }
        }
        _ => None,
    }
}

/// Port of `renderRecord`: humanized `Key: value` lines with scalar arrays
/// joined inline and nested records indented up to depth 2.
fn render_record(record: &[(String, Loose)], indent: &str, depth: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for (key, value) in record {
        let label = humanize_key(key);
        match value {
            Loose::Array(items) => {
                if items.is_empty() {
                    lines.push(format!("{indent}{label}: none"));
                    continue;
                }
                let scalars: Vec<Option<String>> = items.iter().map(format_scalar).collect();
                if scalars.iter().all(|item| item.is_some()) {
                    let joined = scalars
                        .into_iter()
                        .map(|item| item.unwrap_or_default())
                        .collect::<Vec<String>>()
                        .join(", ");
                    lines.push(format!("{indent}{label}: {joined}"));
                    continue;
                }
                lines.push(format!("{indent}{label}:"));
                for item in items {
                    match item {
                        Loose::Record(nested) if depth < 2 => {
                            let inner = render_record(nested, &format!("{indent}  "), depth + 1);
                            lines.push(format!("{indent}  -"));
                            lines.extend(inner);
                        }
                        other => {
                            let scalar = format_scalar(other)
                                .unwrap_or_else(|| "(unserializable value)".to_string());
                            lines.push(format!("{indent}  - {scalar}"));
                        }
                    }
                }
            }
            Loose::Record(nested) => {
                if depth < 2 {
                    let inner = render_record(nested, &format!("{indent}  "), depth + 1);
                    if !inner.is_empty() {
                        lines.push(format!("{indent}{label}:"));
                        lines.extend(inner);
                    }
                    continue;
                }
                // Depth 3+: serialize the record compactly.
                let serialized = loose_to_compact_json(value);
                lines.push(format!("{indent}{label}: {serialized}"));
            }
            other => {
                if let Some(scalar) = format_scalar(other) {
                    lines.push(format!("{indent}{label}: {scalar}"));
                }
            }
        }
    }
    lines
}

/// Minimal compact JSON rendering for records nested past the depth cap
/// (the reference falls back to `JSON.stringify` there).
fn loose_to_compact_json(value: &Loose) -> String {
    match value {
        Loose::Null => "null".to_string(),
        Loose::Bool(flag) => flag.to_string(),
        Loose::Int(number) => number.to_string(),
        Loose::Str(text) => format!("{text:?}"),
        Loose::Array(items) => {
            let joined: Vec<String> = items.iter().map(loose_to_compact_json).collect();
            format!("[{}]", joined.join(","))
        }
        Loose::Record(entries) => {
            let joined: Vec<String> = entries
                .iter()
                .map(|(key, item)| format!("{key:?}:{}", loose_to_compact_json(item)))
                .collect();
            format!("{{{}}}", joined.join(","))
        }
    }
}

/// Port of `orderEntries`: preferred (normalized) keys first in preference
/// order, remaining keys in insertion order.
fn order_entries<'a>(
    record: &'a [(String, Loose)],
    preferred_keys: &[&str],
) -> Vec<(&'a String, &'a Loose)> {
    let mut ranked: Vec<(&String, &Loose)> = Vec::new();
    let mut remaining: Vec<(&String, &Loose)> = record.iter().map(|(k, v)| (k, v)).collect();
    for preferred in preferred_keys {
        for (key, value) in &remaining {
            if &normalize_key(key) == preferred {
                ranked.push((key, value));
            }
        }
        remaining.retain(|(key, _)| &normalize_key(key) != preferred);
    }
    ranked.extend(remaining);
    ranked
}

/// Render a controller view defensively to text; `(no data)` when the view
/// carries nothing printable.
pub fn render_view_text(view: &Loose, preferred_keys: &[&str]) -> String {
    match view {
        Loose::Str(text) if !text.is_empty() => return text.clone(),
        Loose::Record(entries) => {
            let ordered = order_entries(entries, preferred_keys);
            let lines = render_record(&ordered_into_owned(ordered), "  ", 0);
            if !lines.is_empty() {
                return lines.join("\n");
            }
        }
        _ => {}
    }
    "(no data)".to_string()
}

fn ordered_into_owned<'a>(ordered: Vec<(&'a String, &'a Loose)>) -> Vec<(String, Loose)> {
    ordered
        .into_iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Render an inspect result: the markdown body when available, else its
/// fields. `(nothing to inspect)` for absent results.
pub fn render_inspect_body(result: Option<&Loose>) -> String {
    let Some(value) = result else {
        return "(nothing to inspect)".to_string();
    };
    if let Loose::Str(text) = value {
        if !text.is_empty() {
            return text.clone();
        }
        return "(nothing to inspect)".to_string();
    }
    if let Loose::Record(entries) = value {
        for key in ["markdown", "body", "content", "text"] {
            let found =
                entries
                    .iter()
                    .find_map(|(candidate, item)| match (candidate.as_str(), item) {
                        (matched, Loose::Str(text)) if matched == key && !text.is_empty() => {
                            Some(text)
                        }
                        _ => None,
                    });
            if let Some(text) = found {
                return text.clone();
            }
        }
        return render_view_text(value, &INSPECT_KEY_ORDER);
    }
    "(nothing to inspect)".to_string()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::views::{StatusPause, StatusProgress, StatusStats};

    #[test]
    fn humanizes_camel_case_and_separator_keys() {
        assert_eq!(humanize_key("nextAction"), "Next Action");
        assert_eq!(humanize_key("changed_files"), "Changed files");
        assert_eq!(humanize_key("artifactId"), "Artifact Id");
        assert_eq!(humanize_key("goal"), "Goal");
    }

    #[test]
    fn renders_status_view_text_with_preferred_key_order() {
        let view = StatusView {
            goal: "ship a sailing weather CLI".to_string(),
            mode: relay_core::WorkflowMode::Discovery,
            autonomy: "next-checkpoint".to_string(),
            paused: None,
            progress: StatusProgress { stage: 1, total: 7 },
            artifacts: vec![],
            stale_artifacts: vec![],
            changed_files: vec![],
            blockers: vec![],
            next_action: "draft the opportunity brief".to_string(),
            stats: StatusStats {
                events: 3,
                model_calls: 1,
                tool_calls: 0,
            },
        };
        let text = render_view_text(&loose_status(&view), &STATUS_KEY_ORDER);
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines[0], "  Goal: ship a sailing weather CLI");
        assert_eq!(lines[1], "  Mode: discovery");
        assert!(lines.contains(&"  Next Action: draft the opportunity brief"));
        assert!(lines.contains(&"  Autonomy: next-checkpoint"));
        // Nested record: progress renders its fields indented.
        assert!(lines.contains(&"  Progress:"));
        assert!(lines.contains(&"    Stage: 1"));
        assert!(lines.contains(&"    Total: 7"));
    }

    #[test]
    fn renders_scalar_arrays_inline_and_empty_as_none() {
        let view = Loose::record(vec![
            ("blockers", Loose::Array(vec![])),
            (
                "staleArtifacts",
                Loose::Array(vec![Loose::str("art-1"), Loose::str("art-2")]),
            ),
        ]);
        let text = render_view_text(&view, &STATUS_KEY_ORDER);
        assert!(text.contains("Blockers: none"));
        assert!(text.contains("Stale Artifacts: art-1, art-2"));
    }

    #[test]
    fn renders_object_arrays_as_dash_rows() {
        let view = Loose::record(vec![(
            "routing",
            Loose::Array(vec![Loose::record(vec![
                ("purpose", Loose::str("intent")),
                ("role", Loose::str("fast")),
            ])]),
        )]);
        let text = render_view_text(&view, &WHY_KEY_ORDER);
        assert!(text.contains("Routing:"));
        assert!(text.contains("  -"));
        assert!(text.contains("    Purpose: intent"));
        assert!(text.contains("    Role: fast"));
    }

    #[test]
    fn renders_empty_views_as_no_data() {
        assert_eq!(
            render_view_text(&Loose::Record(vec![]), &STATUS_KEY_ORDER),
            "(no data)"
        );
        assert_eq!(
            render_view_text(&Loose::Null, &STATUS_KEY_ORDER),
            "(no data)"
        );
    }

    #[test]
    fn inspect_body_prefers_markdown_and_falls_back_to_fields() {
        let with_body = Loose::record(vec![
            ("markdown", Loose::Null),
            ("body", Loose::str("# Title\n\nbody text")),
            ("path", Loose::str(".relay/artifacts/prd/v1.md")),
        ]);
        assert_eq!(
            render_inspect_body(Some(&with_body)),
            "# Title\n\nbody text"
        );
        let without_body = Loose::record(vec![
            ("markdown", Loose::Null),
            ("body", Loose::str("")),
            ("path", Loose::str("p")),
            ("status", Loose::str("draft")),
        ]);
        let text = render_inspect_body(Some(&without_body));
        assert!(text.contains("Path: p"));
        assert!(text.contains("Status: draft"));
        assert_eq!(render_inspect_body(None), "(nothing to inspect)");
        assert_eq!(
            render_inspect_body(Some(&Loose::str(""))),
            "(nothing to inspect)"
        );
    }

    #[test]
    fn paused_status_renders_question_and_options() {
        let view = StatusView {
            goal: "g".to_string(),
            mode: relay_core::WorkflowMode::Specification,
            autonomy: "off".to_string(),
            paused: Some(StatusPause {
                by: "policy".to_string(),
                reason: "approval needed".to_string(),
                question: Some("Approve?".to_string()),
                options: vec!["yes".to_string(), "no".to_string()],
            }),
            progress: StatusProgress { stage: 2, total: 7 },
            artifacts: vec![],
            stale_artifacts: vec![],
            changed_files: vec![],
            blockers: vec![],
            next_action: "approve".to_string(),
            stats: StatusStats {
                events: 0,
                model_calls: 0,
                tool_calls: 0,
            },
        };
        let text = render_view_text(&loose_status(&view), &STATUS_KEY_ORDER);
        assert!(text.contains("Paused:"));
        assert!(text.contains("Question: Approve?"));
        assert!(text.contains("Options: yes, no"));
        assert!(text.contains("By: policy"));
    }
}
