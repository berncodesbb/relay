//! Repository hygiene suite (retirement/build owner).
//!
//! Relay's implementation and build surfaces are Rust-only. After the
//! TypeScript retirement (2026-10-14) no JavaScript or TypeScript source
//! file and no Node package/tool manifest may live in the repository, with
//! two deliberate carve-outs:
//!
//! - `tests/fixtures/**` — frozen compatibility fixtures and sample
//!   user-workspace inputs. Relay is a language-agnostic coding tool, so
//!   sample projects written in JavaScript or TypeScript are legitimate
//!   *inputs* there and only there;
//! - ignored local directories (`.git`, `target`, `node_modules`, `dist`) —
//!   developer and CI machine state, out of scope for repository policy.
//!
//! The walk below is `std`-only, skips the carve-out directories at any
//! depth, and ignores symlinks and other non-file entries so a stray link
//! can neither fail the walk nor smuggle a file in.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

/// File extensions that mark a file as JavaScript or TypeScript.
const JS_TS_EXTENSIONS: [&str; 8] = ["ts", "tsx", "mts", "cts", "mjs", "cjs", "js", "jsx"];

/// Node package and tool manifests that must not exist outside fixtures.
const NODE_MANIFEST_NAMES: [&str; 9] = [
    "package.json",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    ".npmrc",
    ".nvmrc",
    "tsconfig.json",
    "tsconfig.build.json",
];

/// Whether a file name is a TypeScript project manifest of the allowed
/// base name with suffix variants (`tsconfig.node.json` and friends).
fn is_tsconfig_variant(name: &str) -> bool {
    name.starts_with("tsconfig.") && name.ends_with(".json")
}

/// Directory names that are local machine state rather than repository
/// content; skipped wherever they appear during the walk.
const IGNORED_DIR_NAMES: [&str; 4] = [".git", "target", "node_modules", "dist"];

/// The repository root, reached from this crate's manifest directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Collect every regular file below `dir`, as `/`-separated paths relative
/// to the repository root, skipping ignored directories, `tests/fixtures`,
/// symlinks, and non-file entries.
fn collect_repo_files(dir: &Path, relative: &Path, out: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            // Never follow or count links; crate-level symlinks stay inert.
            continue;
        }
        let name = entry.file_name();
        let child_relative = relative.join(&name);
        if file_type.is_dir() {
            if IGNORED_DIR_NAMES.iter().any(|ignored| name == *ignored) {
                continue;
            }
            if child_relative == Path::new("tests/fixtures") {
                continue;
            }
            let child = dir.join(&name);
            collect_repo_files(&child, &child_relative, out);
        } else if file_type.is_file() {
            out.push(child_relative.to_string_lossy().into_owned());
        }
    }
}

/// Every regular file in the repository outside the ignored directories and
/// `tests/fixtures/**`.
fn repository_files() -> Vec<String> {
    let root = repo_root();
    let mut files = Vec::new();
    collect_repo_files(&root, Path::new(""), &mut files);
    files.sort();
    assert!(
        files.iter().any(|path| path == "Cargo.toml"),
        "repository walk did not reach the workspace root at {}",
        root.display()
    );
    files
}

/// Whether a path carries a JavaScript/TypeScript file extension.
fn has_js_ts_extension(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            let ext = ext.to_ascii_lowercase();
            JS_TS_EXTENSIONS.contains(&ext.as_str())
        })
}

#[test]
fn no_javascript_or_typescript_files_outside_fixtures() {
    let offenders = repository_files()
        .into_iter()
        .filter(|path| has_js_ts_extension(path))
        .collect::<Vec<_>>()
        .join(", ");
    assert!(
        offenders.is_empty(),
        "JavaScript/TypeScript files exist outside `tests/fixtures/**`; Relay \
         implementation and build surfaces are Rust-only (sample user \
         workspaces belong under `tests/fixtures/**`): {offenders}"
    );
}

#[test]
fn no_node_package_or_tool_manifests_outside_fixtures() {
    let offenders = repository_files()
        .into_iter()
        .filter(|path| {
            Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    NODE_MANIFEST_NAMES.contains(&name) || is_tsconfig_variant(name)
                })
        })
        .collect::<Vec<_>>()
        .join(", ");
    assert!(
        offenders.is_empty(),
        "Node package/tool manifests exist outside `tests/fixtures/**`; the \
         repository builds with Cargo alone (sample user workspaces belong \
         under `tests/fixtures/**`): {offenders}"
    );
}
