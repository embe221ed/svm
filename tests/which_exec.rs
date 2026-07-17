//! Binary-level tests for `svm which` and `svm exec`.
mod common;

use common::*;
use std::fs;

// --- which ---

#[test]
fn which_prints_active_binary_path() {
    let h = TestHome::new();
    let dir = h.install_fake_version("mainnet-v1.0.0", &["sui", "move-analyzer"]);
    h.set_global_version("mainnet-v1.0.0");

    let out = h.svm().arg("which").output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), dir.join("sui").display().to_string());

    let out = h.svm().args(["which", "move-analyzer"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(
        stdout_str(&out).trim(),
        dir.join("move-analyzer").display().to_string()
    );
}

#[test]
fn which_resolves_shorthand_global_version() {
    let h = TestHome::new();
    let dir = h.install_fake_version("mainnet-v1.0.0", &["sui"]);
    // Global file holds the bare shorthand; which must resolve it like the shim
    h.set_global_version("v1.0.0");

    let out = h.svm().arg("which").output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), dir.join("sui").display().to_string());
}

#[test]
fn which_respects_local_pin_over_global() {
    let h = TestHome::new();
    h.install_fake_version("mainnet-v1.0.0", &["sui"]);
    let pinned = h.install_fake_version("testnet-v2.0.0", &["sui"]);
    h.set_global_version("mainnet-v1.0.0");

    let project = h.home().join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join(".svm-version"), "testnet-v2.0.0\n").unwrap();

    let out = h.svm().arg("which").current_dir(&project).output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), pinned.join("sui").display().to_string());
}

#[test]
fn which_errors_when_nothing_active() {
    let h = TestHome::new();
    let out = h.svm().arg("which").output().unwrap();
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("SVM is not active"));
}

#[test]
fn which_errors_when_version_not_installed() {
    let h = TestHome::new();
    h.set_global_version("mainnet-v9.9.9");
    let out = h.svm().arg("which").output().unwrap();
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("is set but not installed"));
}

#[test]
fn which_errors_when_binary_missing_from_version() {
    let h = TestHome::new();
    h.install_fake_version("mainnet-v1.0.0", &["sui"]); // no move-analyzer
    h.set_global_version("mainnet-v1.0.0");
    let out = h.svm().args(["which", "move-analyzer"]).output().unwrap();
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("not found for version"));
}

#[test]
fn which_rejects_invalid_binary_names() {
    let h = TestHome::new();
    h.install_fake_version("mainnet-v1.0.0", &["sui"]);
    h.set_global_version("mainnet-v1.0.0");
    for bad in ["../evil", "/bin/sh", ".hidden"] {
        let out = h.svm().args(["which", bad]).output().unwrap();
        assert!(!out.status.success(), "{bad} must be rejected");
        assert!(stderr_str(&out).contains("Invalid binary name"));
    }
}

#[test]
fn which_resolves_series_and_channel_pins_against_installed() {
    let h = TestHome::new();
    h.install_fake_version("mainnet-v1.0.3", &["sui"]);
    let newest = h.install_fake_version("mainnet-v1.0.5", &["sui"]);
    h.install_fake_version("testnet-v2.0.0", &["sui"]);

    // Series pin picks the newest installed patch
    h.set_global_version("v1.0");
    let out = h.svm().arg("which").output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), newest.join("sui").display().to_string());

    // Channel pin picks the newest installed release on that network
    h.set_global_version("testnet");
    let out = h.svm().arg("which").output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(
        stdout_str(&out).trim(),
        h.versions_dir().join("testnet-v2.0.0/sui").display().to_string()
    );
}

#[test]
fn which_reports_dangling_legacy_symlink_link() {
    let h = TestHome::new();
    let dir = h.versions_dir().join("old-style");
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(h.home().join("gone/sui"), dir.join("sui")).unwrap();
    h.set_global_version("old-style");

    let out = h.svm().arg("which").output().unwrap();
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("points to a build that no longer exists"));
}

#[test]
fn shim_refuses_self_exec_loop() {
    // A linked build whose "sui" is svm itself must not fork-bomb
    let h = TestHome::new();
    let dir = h.versions_dir().join("looped");
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_svm"), dir.join("sui")).unwrap();
    h.set_global_version("looped");

    let out = h.shim("sui").output().unwrap();
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("refusing to exec in a loop"));

    let out = h.svm().args(["exec", "looped", "sui"]).output().unwrap();
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("refusing to exec in a loop"));
}

// --- exec ---

#[test]
fn exec_runs_managed_binary_from_requested_version_not_active_one() {
    let h = TestHome::new();
    h.install_fake_version("mainnet-v1.0.0", &["sui"]);
    h.install_fake_version("testnet-v2.0.0", &["sui"]);
    h.set_global_version("mainnet-v1.0.0");

    let out = h
        .svm()
        .args(["exec", "testnet-v2.0.0", "sui", "--version"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), "testnet-v2.0.0:sui --version");
}

#[test]
fn exec_resolves_shorthand_version_specs() {
    let h = TestHome::new();
    h.install_fake_version("mainnet-v1.0.0", &["sui"]);
    let out = h.svm().args(["exec", "v1.0.0", "sui", "ping"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), "mainnet-v1.0.0:sui ping");
}

#[test]
fn exec_prepends_version_dir_to_path_for_arbitrary_commands() {
    let h = TestHome::new();
    let dir = h.install_fake_version("mainnet-v1.0.0", &["sui"]);
    let out = h
        .svm()
        .args(["exec", "mainnet-v1.0.0", "sh", "-c", "echo \"$PATH\""])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let path = stdout_str(&out);
    assert!(
        path.starts_with(&dir.display().to_string()),
        "version dir must lead PATH, got: {path}"
    );
}

#[test]
fn exec_resolves_series_spec_to_newest_installed_patch() {
    let h = TestHome::new();
    h.install_fake_version("mainnet-v1.0.3", &["sui"]);
    h.install_fake_version("mainnet-v1.0.5", &["sui"]);
    let out = h.svm().args(["exec", "v1.0", "sui", "go"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), "mainnet-v1.0.5:sui go");
}

#[test]
fn exec_runs_unmanaged_binary_from_version_dir_when_present() {
    // A binary that exists in the version dir but isn't shim-managed
    // (e.g. sui-tool) runs from the version dir, not from PATH.
    let h = TestHome::new();
    let dir = h.install_fake_version("mainnet-v1.0.0", &["sui"]);
    write_exec_script(&dir.join("sui-tool"), "echo version-dir:sui-tool \"$@\"");
    let out = h
        .svm()
        .args(["exec", "mainnet-v1.0.0", "sui-tool", "dump"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), "version-dir:sui-tool dump");
}

#[test]
fn exec_errors_when_version_not_installed() {
    let h = TestHome::new();
    let out = h.svm().args(["exec", "mainnet-v9.9.9", "sui"]).output().unwrap();
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("is not installed"));
}

#[test]
fn exec_managed_binary_missing_errors_instead_of_falling_back_to_path() {
    // If the requested version lacks a managed binary, exec must not silently
    // pick one up from PATH (that could be the shim running the ACTIVE version).
    let h = TestHome::new();
    h.install_fake_version("mainnet-v1.0.0", &["sui"]); // no move-analyzer
    let out = h
        .svm()
        .args(["exec", "mainnet-v1.0.0", "move-analyzer"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("not found for version"));
}
