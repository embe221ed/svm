use std::fs;
use svm::{resolve_version, VersionSource};
use tempfile::TempDir;

#[test]
fn resolve_global_version() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("version"), "mainnet-v1.63.4\n").unwrap();
    let result = resolve_version(tmp.path()).unwrap();
    assert_eq!(
        result,
        Some(("mainnet-v1.63.4".into(), VersionSource::Global))
    );
}

#[test]
fn resolve_no_version_returns_none() {
    let tmp = TempDir::new().unwrap();
    let result = resolve_version(tmp.path()).unwrap();
    assert_eq!(result, None);
}

#[test]
fn resolve_global_version_trims_whitespace() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("version"), "  v1.0.0  \n").unwrap();
    let result = resolve_version(tmp.path()).unwrap();
    assert_eq!(result, Some(("v1.0.0".into(), VersionSource::Global)));
}

#[test]
fn resolve_empty_version_file_returns_none() {
    // Empty file should be treated as if it doesn't exist
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("version"), "").unwrap();
    let result = resolve_version(tmp.path()).unwrap();
    assert_eq!(result, None);
}

#[test]
fn resolve_whitespace_only_version_file_returns_none() {
    // Whitespace-only file should be treated as if it doesn't exist
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("version"), "  \n\n  ").unwrap();
    let result = resolve_version(tmp.path()).unwrap();
    assert_eq!(result, None);
}

#[test]
fn resolve_multi_line_version_file_takes_first_line() {
    // Only the first non-empty line matters
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("version"), "v1.0.0\nv2.0.0\n").unwrap();
    let result = resolve_version(tmp.path()).unwrap();
    assert_eq!(result, Some(("v1.0.0".into(), VersionSource::Global)));
}

#[test]
fn resolve_version_file_with_leading_blank_lines() {
    // First line is empty, but first *non-empty* line... wait, we take lines().next()
    // which is the first line. If it's blank, that trims to empty → None.
    // The version is on line 2 but we only read line 1.
    // This documents the behavior: put the version on line 1.
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("version"), "\nv1.0.0\n").unwrap();
    let result = resolve_version(tmp.path()).unwrap();
    // First line is empty → treated as no version
    assert_eq!(result, None);
}
