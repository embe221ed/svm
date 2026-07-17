use serde_json::json;
use std::fs;
use svm::{
    asset_platform, asset_sha256, completion_script, human_bytes, load_cached_releases,
    plan_update, uninstall_version, unpack_archive, valid_version_name, verify_sha256,
    version_sort_key, ReleaseCache,
};
use tempfile::TempDir;

// --- valid_version_name (security guard) ---

#[test]
fn version_names_stay_single_path_components() {
    assert!(valid_version_name("mainnet-v1.63.4"));
    assert!(valid_version_name("my-dev"));
    assert!(valid_version_name("v1.0.0"));

    assert!(!valid_version_name(""));
    assert!(!valid_version_name("."));
    assert!(!valid_version_name(".."));
    assert!(!valid_version_name(".hidden"));
    assert!(!valid_version_name("a/b"));
    assert!(!valid_version_name("../escape"));
    assert!(!valid_version_name("/tmp/evil"));
    assert!(!valid_version_name("a\\b"));
}

#[test]
fn resolve_rejects_path_traversal_names() {
    // A hostile .svm-version file must not be able to point outside versions/
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();
    // Even though this absolute path exists, it must not resolve
    assert!(svm::resolve_installed_version("/tmp", &versions).is_none());
    assert!(svm::resolve_installed_version("..", &versions).is_none());
    assert!(svm::resolve_installed_version("", &versions).is_none());
}

// --- verify_sha256 ---

#[test]
fn sha256_accepts_matching_digest_case_insensitively() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("data");
    fs::write(&file, b"hello world").unwrap();
    // sha256("hello world")
    let digest = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
    assert!(verify_sha256(&file, digest).unwrap());
    assert!(verify_sha256(&file, &digest.to_uppercase()).unwrap());
}

#[test]
fn sha256_rejects_mismatch() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("data");
    fs::write(&file, b"hello world").unwrap();
    assert!(!verify_sha256(&file, &"0".repeat(64)).unwrap());
}

// --- asset_sha256 (GitHub digest field parsing) ---

#[test]
fn asset_digest_parses_sha256_prefix() {
    let asset = json!({"name": "x.tgz", "digest": "sha256:ABCDEF012345"});
    assert_eq!(asset_sha256(&asset), Some("abcdef012345".into()));
}

#[test]
fn asset_digest_ignores_other_algorithms_and_absence() {
    assert_eq!(asset_sha256(&json!({"digest": "md5:abc"})), None);
    assert_eq!(asset_sha256(&json!({"digest": null})), None);
    assert_eq!(asset_sha256(&json!({"name": "x.tgz"})), None);
}

// --- asset_platform ---

#[test]
fn platform_mapping_matches_sui_asset_naming() {
    assert_eq!(asset_platform("macos", "x86_64").unwrap(), ("macos", "x86_64"));
    // macOS ARM is "arm64" but Linux ARM is "aarch64" in Sui asset names
    assert_eq!(asset_platform("macos", "aarch64").unwrap(), ("macos", "arm64"));
    assert_eq!(asset_platform("linux", "x86_64").unwrap(), ("ubuntu", "x86_64"));
    assert_eq!(asset_platform("linux", "aarch64").unwrap(), ("ubuntu", "aarch64"));
}

#[test]
fn platform_mapping_rejects_unsupported() {
    assert!(asset_platform("windows", "x86_64").is_err());
    assert!(asset_platform("linux", "riscv64").is_err());
}

// --- human_bytes ---

#[test]
fn human_bytes_unit_boundaries() {
    assert_eq!(human_bytes(0), "0 B");
    assert_eq!(human_bytes(1023), "1023 B");
    assert_eq!(human_bytes(1024), "1.0 KiB");
    assert_eq!(human_bytes(1536), "1.5 KiB");
    assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
    assert_eq!(human_bytes(378 * 1024 * 1024), "378.0 MiB");
    assert_eq!(human_bytes(1024u64.pow(3)), "1.0 GiB");
}

// --- version_sort_key ---

#[test]
fn sort_groups_by_network_newest_first_linked_last() {
    let mut names = vec![
        "my-dev".to_string(),
        "testnet-v1.74.1".to_string(),
        "mainnet-v1.9.0".to_string(),
        "mainnet-v1.63.4".to_string(),
        "devnet-v1.75.0".to_string(),
        "mainnet-v1.63.11".to_string(),
    ];
    names.sort_by_key(|n| version_sort_key(n));
    assert_eq!(
        names,
        vec![
            "mainnet-v1.63.11", // 1.63.11 > 1.63.4 numerically
            "mainnet-v1.63.4",
            "mainnet-v1.9.0", // 1.9 < 1.63 numerically (lexicographic would misplace it)
            "testnet-v1.74.1",
            "devnet-v1.75.0",
            "my-dev",
        ]
    );
}

#[test]
fn sort_key_is_total_for_mixed_other_group() {
    // "v1.2.3" (parsable semver, network "other") and linked names (no semver)
    // land in the same group; the key must still give a stable total order.
    let mut names = vec![
        "zed-build".to_string(),
        "v1.2.3".to_string(),
        "alpha-build".to_string(),
        "v2.0.0".to_string(),
    ];
    names.sort_by_key(|n| version_sort_key(n)); // must not panic
    assert_eq!(names, vec!["v2.0.0", "v1.2.3", "alpha-build", "zed-build"]);
}

// --- plan_update ---

fn releases_from(tags: &[&str]) -> Vec<serde_json::Value> {
    tags.iter().map(|t| json!({"tag_name": t})).collect()
}

#[test]
fn plan_update_finds_newer_release_on_same_network() {
    let releases = releases_from(&["mainnet-v1.74.1", "testnet-v1.75.0", "mainnet-v1.63.4"]);
    assert_eq!(
        plan_update("mainnet-v1.63.4", &releases).unwrap(),
        Some("mainnet-v1.74.1".into())
    );
}

#[test]
fn plan_update_already_latest_returns_none() {
    let releases = releases_from(&["mainnet-v1.74.1"]);
    assert_eq!(plan_update("mainnet-v1.74.1", &releases).unwrap(), None);
}

#[test]
fn plan_update_never_downgrades() {
    // Active version is newer than anything in the (possibly stale) list
    let releases = releases_from(&["mainnet-v1.70.0"]);
    assert_eq!(plan_update("mainnet-v1.74.1", &releases).unwrap(), None);
}

#[test]
fn plan_update_respects_network_of_current() {
    let releases = releases_from(&["mainnet-v1.80.0", "testnet-v1.75.0"]);
    assert_eq!(
        plan_update("testnet-v1.74.1", &releases).unwrap(),
        Some("testnet-v1.75.0".into())
    );
}

#[test]
fn plan_update_rejects_linked_builds() {
    assert!(plan_update("my-dev", &releases_from(&["mainnet-v1.74.1"])).is_err());
}

#[test]
fn plan_update_rejects_linked_builds_with_network_substrings() {
    // network_label would substring-match these; the structural release check
    // must not (a linked build named "mainnet-fork" is not a mainnet release)
    let releases = releases_from(&["mainnet-v1.74.1", "testnet-v1.75.0", "devnet-v1.76.0"]);
    for name in ["mainnet-fork", "testnet-dev", "devnet-local", "sui-testnet-build", "testnet"] {
        assert!(
            plan_update(name, &releases).is_err(),
            "{name} must be classified as a custom build"
        );
    }
}

#[test]
fn release_network_requires_exact_shape() {
    assert_eq!(svm::release_network("mainnet-v1.63.4"), Some("mainnet"));
    assert_eq!(svm::release_network("testnet-v1.0.0"), Some("testnet"));
    assert_eq!(svm::release_network("mainnet-fork"), None);
    assert_eq!(svm::release_network("testnet-dev"), None);
    assert_eq!(svm::release_network("my-dev"), None);
    assert_eq!(svm::release_network("v1.63.4"), None);
    assert_eq!(svm::release_network("mainnet"), None);
}

// --- uninstall clears the active global version ---

#[test]
fn uninstall_active_version_clears_global_file() {
    let tmp = TempDir::new().unwrap();
    let svm_dir = tmp.path();
    let versions = svm_dir.join("versions");
    fs::create_dir_all(versions.join("mainnet-v1.0.0")).unwrap();
    // Global file holds the bare shorthand — must still match after normalization
    fs::write(svm_dir.join("version"), "v1.0.0\n").unwrap();

    uninstall_version("v1.0.0", &versions, svm_dir).unwrap();

    assert!(!versions.join("mainnet-v1.0.0").exists());
    assert!(!svm_dir.join("version").exists());
}

#[test]
fn uninstall_inactive_version_keeps_global_file() {
    let tmp = TempDir::new().unwrap();
    let svm_dir = tmp.path();
    let versions = svm_dir.join("versions");
    fs::create_dir_all(versions.join("mainnet-v1.0.0")).unwrap();
    fs::create_dir_all(versions.join("mainnet-v2.0.0")).unwrap();
    fs::write(svm_dir.join("version"), "mainnet-v2.0.0\n").unwrap();

    uninstall_version("v1.0.0", &versions, svm_dir).unwrap();

    assert!(!versions.join("mainnet-v1.0.0").exists());
    assert_eq!(
        fs::read_to_string(svm_dir.join("version")).unwrap().trim(),
        "mainnet-v2.0.0"
    );
}

#[test]
fn uninstall_empty_name_errors_instead_of_wiping_versions_dir() {
    let tmp = TempDir::new().unwrap();
    let svm_dir = tmp.path();
    let versions = svm_dir.join("versions");
    fs::create_dir_all(versions.join("mainnet-v1.0.0")).unwrap();

    // join("") yields versions_dir itself — must be rejected, not deleted
    assert!(uninstall_version("", &versions, svm_dir).is_err());
    assert!(versions.join("mainnet-v1.0.0").exists());
}

// --- unpack_archive (atomic staging) ---

fn make_tgz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let enc = flate2::write::GzEncoder::new(&mut out, flate2::Compression::default());
        let mut builder = tar::Builder::new(enc);
        for (name, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, name, *content).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }
    out
}

#[test]
fn unpack_extracts_atomically_and_cleans_staging() {
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();
    let archive = tmp.path().join("sui-test.tgz");
    fs::write(&archive, make_tgz(&[("sui", b"binary"), ("move-analyzer", b"lsp")])).unwrap();

    unpack_archive(&archive, &versions, "mainnet-v9.9.9").unwrap();

    let target = versions.join("mainnet-v9.9.9");
    assert_eq!(fs::read(target.join("sui")).unwrap(), b"binary");
    assert_eq!(fs::read(target.join("move-analyzer")).unwrap(), b"lsp");
    // No staging leftovers
    let staging: Vec<_> = fs::read_dir(&versions)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".staging-"))
        .collect();
    assert!(staging.is_empty());
    // Archive is kept for future reinstalls
    assert!(archive.exists());
}

#[test]
fn unpack_corrupt_archive_leaves_no_target_and_evicts_archive() {
    let tmp = TempDir::new().unwrap();
    let versions = tmp.path().join("versions");
    fs::create_dir_all(&versions).unwrap();
    let archive = tmp.path().join("sui-corrupt.tgz");
    fs::write(&archive, b"this is not a gzip archive").unwrap();

    let result = unpack_archive(&archive, &versions, "mainnet-v9.9.9");

    assert!(result.is_err());
    assert!(!versions.join("mainnet-v9.9.9").exists());
    // Corrupt archive must not stay cached (would fail identically on retry)
    assert!(!archive.exists());
    let staging: Vec<_> = fs::read_dir(&versions)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".staging-"))
        .collect();
    assert!(staging.is_empty());
}

// --- load_cached_releases ---

#[test]
fn load_cached_releases_reads_valid_cache() {
    let tmp = TempDir::new().unwrap();
    let cache_dir = tmp.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    let cache = ReleaseCache {
        etag: None,
        pages: 1,
        releases: releases_from(&["mainnet-v1.0.0", "testnet-v1.0.0"]),
    };
    fs::write(cache_dir.join("releases.json"), serde_json::to_string(&cache).unwrap()).unwrap();

    let releases = load_cached_releases(tmp.path()).unwrap();
    assert_eq!(releases.len(), 2);
}

#[test]
fn load_cached_releases_none_when_missing_or_corrupt() {
    let tmp = TempDir::new().unwrap();
    assert!(load_cached_releases(tmp.path()).is_none());

    let cache_dir = tmp.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    fs::write(cache_dir.join("releases.json"), "{{{not json").unwrap();
    assert!(load_cached_releases(tmp.path()).is_none());
}

// --- completion scripts ---

#[test]
fn zsh_completions_wire_dynamic_version_functions() {
    let script = completion_script(clap_complete::Shell::Zsh);
    // install completes remote versions; use + uninstall + exec complete local ones
    assert_eq!(script.matches(":version:_svm_remote_versions").count(), 1);
    assert_eq!(script.matches(":version:_svm_local_versions").count(), 3);
    // No placeholder left un-replaced — if clap_complete changes its output
    // format, this catches the silent breakage
    assert_eq!(script.matches(":version:_default").count(), 0);
    // which completes the managed binary names
    assert!(script.contains("'::binary:(sui move-analyzer)'"));
    assert_eq!(script.matches(":binary:_default").count(), 0);
    assert!(script.contains("_svm_remote_versions()"));
    assert!(script.contains("_svm_local_versions()"));
    // Dynamic completion must not depend on python3
    assert!(!script.contains("python3"));
}

#[test]
fn bash_and_fish_completions_generate() {
    for shell in [clap_complete::Shell::Bash, clap_complete::Shell::Fish] {
        let script = completion_script(shell);
        assert!(script.contains("svm"), "{shell} script looks empty");
        assert!(script.len() > 100);
    }
}
