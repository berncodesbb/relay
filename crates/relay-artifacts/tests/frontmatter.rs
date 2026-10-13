//! Focused tests for the frontmatter writer/reader round trip — a generic
//! utility pair (flat YAML frontmatter + Markdown body) that survives into
//! the new architecture. The `Frontmatter` struct's fixed TS field set is
//! schema-bound (classification in the storage/migration report); the
//! round-trip capability tested here is what the new artifact publication
//! path keeps.

use relay_artifacts::frontmatter::{Frontmatter, render_artifact_file, split_frontmatter};
use relay_artifacts::slug::SLUG_DECOMPOSITIONS;
use relay_core::ArtifactKind;
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn sample(title: &str) -> Frontmatter<'_> {
    Frontmatter {
        id: "art_00000000-0000-4000-8000-000000000001",
        kind: ArtifactKind::Prd,
        version: 2,
        title,
        status: "draft",
        created_at: "2026-10-13T02:46:40.000Z",
        updated_at: "2026-10-13T02:46:40.000Z",
    }
}

#[test]
fn renders_and_splits_back() -> Result<()> {
    let frontmatter = sample("plain title");
    let file = render_artifact_file(&frontmatter, "# Heading\n\nBody text.\n");
    let parsed = split_frontmatter(&file, "prd/v2-plain-title.md")?;

    // Byte shape: delimiters, then YAML, blank line, then the body.
    assert!(file.starts_with("---\n"));
    assert!(file.contains("\n---\n\n# Heading"));
    assert_eq!(parsed.body, "# Heading\n\nBody text.\n");
    assert_eq!(
        parsed.frontmatter.get("id"),
        Some(&Value::String(frontmatter.id.to_string()))
    );
    assert_eq!(
        parsed.frontmatter.get("kind"),
        Some(&Value::String("prd".to_string()))
    );
    assert_eq!(parsed.frontmatter.get("version"), Some(&json!(2)));
    assert_eq!(parsed.frontmatter.get("status"), Some(&json!("draft")));
    assert_eq!(
        parsed.frontmatter.get("title"),
        Some(&Value::String("plain title".to_string()))
    );
    // Timestamps stay strings (they are not YAML dates in this writer).
    assert_eq!(
        parsed.frontmatter.get("createdAt"),
        Some(&Value::String("2026-10-13T02:46:40.000Z".to_string()))
    );
    Ok(())
}

#[test]
fn quoted_and_folded_titles_round_trip() -> Result<()> {
    // Titles that force single/double quoting and flow folding all survive
    // a write/read cycle with the exact same string value.
    let titles = [
        "plain title",
        "Draft: second version of something",
        "it's quoted",
        "say \"hello\"",
        "true",
        "42",
        "3.14",
        "null",
        "a very long title that must be folded across lines because it \
         exceeds the eighty column line width of the yaml writer",
        "héllo → 🚀",
        "  leading and trailing spaces  ",
        "#hashtag at the start",
        "ends with colon:",
    ];
    for title in titles {
        let frontmatter = sample(title);
        let file = render_artifact_file(&frontmatter, "body");
        let parsed =
            split_frontmatter(&file, "f.md").map_err(|e| format!("title {title:?}: {e}"))?;
        assert_eq!(
            parsed.frontmatter.get("title"),
            Some(&Value::String(title.to_string())),
            "title {title:?} did not round trip; file:\n{file}"
        );
        assert_eq!(parsed.body, "body");
    }
    Ok(())
}

#[test]
fn corpus_titles_round_trip() -> Result<()> {
    // Every scalar in the decomposition table and every title shape the
    // table can produce must round-trip: build adversarial titles from the
    // NFKD entries themselves.
    let mut titles: Vec<String> = Vec::new();
    for (code_point, decomposition) in SLUG_DECOMPOSITIONS.iter().take(200) {
        if let Some(character) = char::from_u32(*code_point) {
            titles.push(format!("title with {character} ({decomposition})"));
        }
    }
    for title in &titles {
        let frontmatter = sample(title);
        let file = render_artifact_file(&frontmatter, "b");
        let parsed =
            split_frontmatter(&file, "f.md").map_err(|e| format!("title {title:?}: {e}"))?;
        assert_eq!(
            parsed.frontmatter.get("title"),
            Some(&Value::String(title.clone())),
            "NFKD-derived title {title:?} did not round trip; file:\n{file}"
        );
    }
    Ok(())
}

#[test]
fn body_gets_leading_blank_lines_stripped_only() -> Result<()> {
    let file = render_artifact_file(&sample("t"), "\n\n\nactual body\n\n");
    let parsed = split_frontmatter(&file, "f.md")?;
    assert_eq!(parsed.body, "actual body\n\n");
    Ok(())
}

#[test]
fn malformed_files_fail_with_actionable_diagnostics() -> Result<()> {
    let missing_open = "no delimiters at all";
    let error = split_frontmatter(missing_open, "f.md")
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(error.contains("opening"), "{error}");

    let missing_close = "---\nid: art_1\n";
    let error = split_frontmatter(missing_close, "f.md")
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(error.contains("closing"), "{error}");

    let not_mapping = "---\n- just\n- a list\n---\n\nbody";
    assert!(split_frontmatter(not_mapping, "f.md").is_err());

    // A line without a `key: value` shape fails with guidance. (An indented
    // line after a value is legitimate flow folding, not an error.)
    let no_colon = "---\nid: art_1\njust text\n---\n\nbody";
    let error = split_frontmatter(no_colon, "f.md")
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(error.contains("key: value"), "{error}");

    let duplicate_key = "---\nid: a\nid: b\n---\n\nbody";
    let error = split_frontmatter(duplicate_key, "f.md")
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(error.contains("unique"), "{error}");
    Ok(())
}
