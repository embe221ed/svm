use std::fs;
use std::path::Path;
use svm::{normalize_install_tag, resolve_installed_version};
use tempfile::TempDir;

fn create_version(versions_dir: &Path, name: &str) {
    let dir = versions_dir.join(name);
    fs::create_dir_all(&dir).unwrap();
    // Create a fake sui binary so the version looks real
    fs::write(dir.join("sui"), "fake-binary").unwrap();
}

// --- resolve_installed_version: normalization + fallback ---

#[test]
fn resolve_bare_version_finds_mainnet_prefixed() {
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();
    create_version(&versions, "mainnet-v1.63.4");

    let result = resolve_installed_version("v1.63.4", &versions);
    assert!(result.is_some());
    let (name, path) = result.unwrap();
    assert_eq!(name, "mainnet-v1.63.4");
    assert_eq!(path, versions.join("mainnet-v1.63.4"));
}

#[test]
fn resolve_full_tag_finds_directly() {
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();
    create_version(&versions, "testnet-v1.63.4");

    let result = resolve_installed_version("testnet-v1.63.4", &versions);
    assert!(result.is_some());
    let (name, _) = result.unwrap();
    assert_eq!(name, "testnet-v1.63.4");
}

#[test]
fn resolve_linked_name_finds_directly() {
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();
    create_version(&versions, "my-dev");

    let result = resolve_installed_version("my-dev", &versions);
    assert!(result.is_some());
    let (name, _) = result.unwrap();
    assert_eq!(name, "my-dev");
}

#[test]
fn resolve_nonexistent_version_returns_none() {
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();

    let result = resolve_installed_version("v1.0.0", &versions);
    assert!(result.is_none());
}

// --- Link names must NOT be normalized ---

#[test]
fn linked_name_with_v_prefix_not_normalized() {
    // If someone links a build as "v-custom", it should be stored as "v-custom",
    // not "mainnet-v-custom"
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();
    create_version(&versions, "v-custom");

    // normalize_install_tag would turn "v-custom" into "mainnet-v-custom"
    assert_eq!(normalize_install_tag("v-custom"), "mainnet-v-custom");

    // But resolve_installed_version should find "v-custom" via fallback
    let result = resolve_installed_version("v-custom", &versions);
    assert!(result.is_some());
    let (name, _) = result.unwrap();
    assert_eq!(name, "v-custom");
}

#[test]
fn linked_name_starting_with_v_prefers_exact_match_if_both_exist() {
    // Edge case: both "mainnet-v1.0.0" and "v1.0.0" exist.
    // Exact match should take precedence — the user typed "v1.0.0"
    // and that name exists, so use it directly.
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();
    create_version(&versions, "mainnet-v1.0.0");
    create_version(&versions, "v1.0.0");

    let result = resolve_installed_version("v1.0.0", &versions);
    assert!(result.is_some());
    let (name, _) = result.unwrap();
    // Prefers the exact match
    assert_eq!(name, "v1.0.0");
}

// --- Install + use consistency ---

#[test]
fn install_tag_normalization_consistent_with_resolve() {
    // Whatever install normalizes to, resolve should find
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();

    let inputs = ["v1.63.4", "mainnet-v1.63.4", "testnet-v1.0.0", "my-dev"];
    for input in inputs {
        let installed_as = normalize_install_tag(input);
        create_version(&versions, &installed_as);

        let result = resolve_installed_version(input, &versions);
        assert!(
            result.is_some(),
            "resolve_installed_version({:?}) should find {:?}",
            input,
            installed_as
        );
        let (resolved_name, _) = result.unwrap();
        assert_eq!(
            resolved_name, installed_as,
            "resolve({:?}) returned {:?}, expected {:?}",
            input, resolved_name, installed_as
        );
    }
}

// --- Link name edge cases ---

#[test]
fn link_name_with_dots_and_dashes() {
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();
    create_version(&versions, "sui-2024.01-custom");

    let result = resolve_installed_version("sui-2024.01-custom", &versions);
    assert!(result.is_some());
    assert_eq!(result.unwrap().0, "sui-2024.01-custom");
}

#[test]
fn link_name_with_spaces_in_path() {
    // Version names shouldn't have spaces, but test that resolution
    // doesn't panic on weird input
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();

    let result = resolve_installed_version("has space", &versions);
    assert!(result.is_none());
}

// --- Uninstall consistency ---

#[test]
fn uninstall_finds_normalized_version() {
    // User installs with "v1.63.4" (stored as "mainnet-v1.63.4"),
    // then uninstalls with "v1.63.4" — should find it
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();
    create_version(&versions, "mainnet-v1.63.4");

    let result = resolve_installed_version("v1.63.4", &versions);
    assert!(result.is_some());
    assert_eq!(result.unwrap().0, "mainnet-v1.63.4");
}

#[test]
fn uninstall_finds_linked_version() {
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();
    create_version(&versions, "local-debug");

    let result = resolve_installed_version("local-debug", &versions);
    assert!(result.is_some());
    assert_eq!(result.unwrap().0, "local-debug");
}

// --- Version written to version file matches resolved name ---

#[test]
fn version_file_should_contain_resolved_name_not_raw_input() {
    // When svm use v1.63.4 is called, the version file should contain
    // "mainnet-v1.63.4", not "v1.63.4", because that's what the shim
    // will look up in ~/.svm/versions/
    let tag = normalize_install_tag("v1.63.4");
    assert_eq!(tag, "mainnet-v1.63.4");
    // This is what gets written to the version file
}

#[test]
fn linked_version_file_contains_exact_name() {
    // For a linked build, no normalization happens on the name itself
    let tag = normalize_install_tag("my-dev");
    assert_eq!(tag, "my-dev");
    // Exact name preserved
}

// --- resolve_available_version: non-exact specs against installed versions ---

#[test]
fn available_series_spec_picks_newest_installed_patch() {
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();
    create_version(&versions, "mainnet-v1.63.4");
    create_version(&versions, "mainnet-v1.63.11");
    create_version(&versions, "mainnet-v1.64.0");
    create_version(&versions, "testnet-v1.63.20");

    for spec in ["v1.63", "1.63", "mainnet-v1.63"] {
        let (name, path) = svm::resolve_available_version(spec, &versions).unwrap();
        assert_eq!(name, "mainnet-v1.63.11", "spec {spec}");
        assert_eq!(path, versions.join("mainnet-v1.63.11"));
    }
    let (name, _) = svm::resolve_available_version("testnet-v1.63", &versions).unwrap();
    assert_eq!(name, "testnet-v1.63.20");
}

#[test]
fn available_channel_spec_picks_newest_installed_on_network() {
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();
    create_version(&versions, "mainnet-v1.63.4");
    create_version(&versions, "mainnet-v1.64.0");
    create_version(&versions, "testnet-v1.70.1");

    for (spec, expected) in [
        ("latest", "mainnet-v1.64.0"),
        ("mainnet", "mainnet-v1.64.0"),
        ("testnet", "testnet-v1.70.1"),
        ("testnet-latest", "testnet-v1.70.1"),
    ] {
        let (name, _) = svm::resolve_available_version(spec, &versions).unwrap();
        assert_eq!(name, expected, "spec {spec}");
    }
}

#[test]
fn available_spec_ignores_linked_builds_with_network_substrings() {
    // A linked build named "testnet-fork" must not satisfy a "testnet" pin
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();
    create_version(&versions, "testnet-fork");
    create_version(&versions, "my-testnet-build");

    assert!(svm::resolve_available_version("testnet", &versions).is_none());
    assert!(svm::resolve_available_version("v1.63", &versions).is_none());
}

#[test]
fn available_exact_names_still_resolve_first() {
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();
    create_version(&versions, "my-dev");
    create_version(&versions, "mainnet-v1.63.4");

    let (name, _) = svm::resolve_available_version("my-dev", &versions).unwrap();
    assert_eq!(name, "my-dev");
    let (name, _) = svm::resolve_available_version("v1.63.4", &versions).unwrap();
    assert_eq!(name, "mainnet-v1.63.4");
    assert!(svm::resolve_available_version("nonexistent", &versions).is_none());
}
