//! Focused tests for the generic `slugify` utility — the kebab-case
//! filename slug used by artifact publication. This is a storage-side
//! generic utility that survives into the new architecture (classification:
//! generic, see the storage/migration report).

use relay_artifacts::slugify;

/// Assert a vector; `\u{…}` spellings in the table stay readable.
fn check(input: &str, expected: &str) {
    assert_eq!(&slugify(input), expected, "input {input:?}");
}

#[test]
fn ascii_basics() {
    check(
        "Faster release feedback loop",
        "faster-release-feedback-loop",
    );
    check("Release gate PRD", "release-gate-prd");
    check("already-kebab-case", "already-kebab-case");
    check("Mixed   spaces\tand tabs", "mixed-spaces-and-tabs");
    check(
        "Trim -- leading and trailing -- dashes --",
        "trim-leading-and-trailing-dashes",
    );
}

#[test]
fn empty_and_degenerate_inputs_fall_back_to_untitled() {
    check("", "untitled");
    check("   ", "untitled");
    check("---", "untitled");
    check("!!!", "untitled");
    check("😄😄", "untitled");
}

#[test]
fn punctuation_and_symbols_collapse_to_one_dash() {
    check("A_B/C:D.E", "a-b-c-d-e");
    check("C++ & Rust?", "c-rust");
    check("  (parenthetical)  ", "parenthetical");
}

#[test]
fn digits_are_kept() {
    check("v2 of the 3rd kind", "v2-of-the-3rd-kind");
    check("123", "123");
}

#[test]
fn latin_diacritics_decompose() {
    // NFKD decomposition + mark stripping.
    check("héllo wörld", "hello-world");
    check("Álvaro Nuñez", "alvaro-nunez");
    check("façade naïve", "facade-naive");
}

#[test]
fn non_latin_scripts_without_transliteration_drop_out() {
    // Code points with no decomposition contributing ASCII alnums vanish;
    // separators around them collapse.
    check("日本語 title", "title");
    check("mixed 日本語 title", "mixed-title");
}

#[test]
fn truncation_happens_at_sixty_utf16_units_and_never_ends_in_a_dash() {
    let sixty_a = "a".repeat(60);
    check(&sixty_a, &sixty_a);
    // Cutting at 60 UTF-16 units keeps a trailing dash only if the 61st
    // char is alphanumeric; a trailing dash after the cut is trimmed.
    let long = format!("{}-{}", "a".repeat(40), "b".repeat(40));
    let slug = slugify(&long);
    assert_eq!(slug.chars().count(), 60, "{slug}");
    assert!(!slug.ends_with('-'), "{slug}");
    assert!(slug.starts_with(&"a".repeat(40)), "{slug}");
    // A surrogate pair counts as two UTF-16 units: the cut lands inside it.
    let with_emoji = format!("{}{}", "a".repeat(58), "😀😀");
    let slug = slugify(&with_emoji);
    assert!(slug.starts_with(&"a".repeat(58)), "{slug}");
    assert!(slug.chars().count() <= 60, "{slug}");
}

#[test]
fn deterministic() {
    for input in [
        "Ship the release gate",
        "héllo → 🚀",
        "  spaced  ",
        "12345",
        "",
    ] {
        assert_eq!(slugify(input), slugify(input), "input {input:?}");
    }
}
