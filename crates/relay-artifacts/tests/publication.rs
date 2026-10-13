//! Focused tests for the generic artifact publication utilities:
//! prepare-and-publish atomicity (no overwrite), content digests, and
//! orphan detection. These are storage-side generic utilities that survive
//! into the new architecture; the TS draft-schema surfaces around them are
//! not covered here (classification: `publish_file`, `sha256_hex`,
//! `find_orphan_artifacts`, `store_relative_path` are generic;
//! `ArtifactMarkdownStore::save/list/inspect` are TS-schema-bound).

use std::fs;
use std::path::Path;

use relay_artifacts::{find_orphan_artifacts, publish_file, sha256_hex, store_relative_path};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn expected_sha256(content: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(content))
}

#[test]
fn publish_is_atomic_and_never_overwrites() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("v1-release-gate-prd.md");

    let published = publish_file(dir.path(), "v1-release-gate-prd.md", b"first bytes")?;
    assert_eq!(published.absolute_path, target);
    assert!(target.is_file());
    assert_eq!(fs::read(&target)?, b"first bytes");

    // A second publication under the same name fails loudly and leaves the
    // original bytes untouched.
    let error = publish_file(dir.path(), "v1-release-gate-prd.md", b"second bytes")
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(
        error.to_lowercase().contains("already"),
        "expected an already-exists failure, got: {error}"
    );
    assert_eq!(fs::read(&target)?, b"first bytes");

    // No temporary leftovers remain after either publication.
    let leftovers: Vec<String> = fs::read_dir(dir.path())?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".tmp-"))
        .collect();
    assert_eq!(leftovers, Vec::<String>::new());
    Ok(())
}

#[test]
fn published_digest_matches_the_content() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let content = "---\nid: art_1\n---\n\nbody with héllo → 🚀\n".as_bytes();
    let published = publish_file(dir.path(), "v1-digest.md", content)?;
    assert_eq!(published.sha256, expected_sha256(content));
    // The digest matches the bytes on disk, not just the input buffer.
    let on_disk = fs::read(published.absolute_path)?;
    assert_eq!(published.sha256, sha256_hex(&on_disk));
    // sha256_hex is lowercase hex of length 64.
    assert_eq!(published.sha256.len(), 64);
    assert!(
        published
            .sha256
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    Ok(())
}

#[test]
fn orphan_detection_reports_but_never_deletes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let artifacts = dir.path().join("artifacts");
    fs::create_dir_all(artifacts.join("prd"))?;
    fs::create_dir_all(artifacts.join("trd"))?;

    let referenced = "prd/v1-referenced.md";
    publish_file(
        &artifacts.join("prd"),
        "v1-referenced.md",
        b"referenced content",
    )?;
    let orphan_a = "prd/v2-orphan.md";
    publish_file(&artifacts.join("prd"), "v2-orphan.md", b"orphan a")?;
    let orphan_b = "trd/v1-orphan.md";
    publish_file(&artifacts.join("trd"), "v1-orphan.md", b"orphan b")?;
    // A crash leftover from the prepare step is an orphan too.
    let tmp = artifacts.join("prd").join("v3-crashed.md.tmp-123-1");
    fs::write(&tmp, b"partial")?;

    let orphans = find_orphan_artifacts(
        &artifacts,
        &[
            // Workspace-relative and store-relative forms both count as
            // references.
            ".relay/artifacts/prd/v1-referenced.md".to_string(),
            referenced.to_string(),
        ],
    )?;
    let paths: Vec<&str> = orphans.iter().map(|o| o.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "prd/v2-orphan.md",
            "prd/v3-crashed.md.tmp-123-1",
            "trd/v1-orphan.md"
        ],
        "orphans are sorted by path"
    );
    // Each orphan carries the digest of its bytes.
    for orphan in &orphans {
        let bytes = fs::read(artifacts.join(&orphan.path))?;
        assert_eq!(orphan.sha256, sha256_hex(&bytes));
    }

    // Nothing was deleted by the scan.
    assert!(artifacts.join(orphan_a).exists());
    assert!(artifacts.join(orphan_b).exists());
    assert!(tmp.exists());
    Ok(())
}

#[test]
fn orphan_detection_on_a_missing_directory_is_empty() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let missing = dir.path().join("nope");
    let orphans = find_orphan_artifacts(&missing, &[])?;
    // Scanning a directory that cannot be read yields no orphans rather
    // than failing: unreadable subtrees are skipped by design.
    assert!(orphans.is_empty());
    let _ = Path::new(&missing);
    Ok(())
}

#[test]
fn store_relative_paths_normalize_the_relay_prefix() {
    assert_eq!(
        store_relative_path(".relay/artifacts/prd/v1-x.md"),
        "prd/v1-x.md"
    );
    assert_eq!(store_relative_path("prd/v1-x.md"), "prd/v1-x.md");
    assert_eq!(store_relative_path("other/v1-x.md"), "other/v1-x.md");
    // Windows separators in the workspace-relative form are not special
    // here; the documented contract strips only the forward-slash prefix,
    // so a mixed-separator path is returned unchanged.
    assert_eq!(
        store_relative_path(".relay/artifacts\\prd\\v1-x.md"),
        ".relay/artifacts\\prd\\v1-x.md"
    );
}
