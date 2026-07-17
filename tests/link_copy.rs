//! Binary-level tests for `svm link` copy semantics: a linked build is a
//! stable copy that survives in-place rebuilds of the source until an
//! explicit re-link refreshes it.
mod common;

use common::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn link_copies_binaries_instead_of_symlinking() {
    let h = TestHome::new();
    let src = h.home().join("build");
    fs::create_dir_all(&src).unwrap();
    write_exec_script(&src.join("sui"), "echo my-fork:sui \"$@\"");
    write_exec_script(&src.join("move-analyzer"), "echo my-fork:move-analyzer \"$@\"");

    let out = h
        .svm()
        .args(["link", "my-fork", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));

    let dir = h.versions_dir().join("my-fork");
    for bin in ["sui", "move-analyzer"] {
        let meta = fs::symlink_metadata(dir.join(bin)).unwrap();
        assert!(
            meta.file_type().is_file(),
            "{bin} must be a regular file copy, not a symlink"
        );
        assert!(
            meta.permissions().mode() & 0o111 != 0,
            "{bin} must stay executable"
        );
    }
    // Provenance marker records where the copies came from
    let marker = fs::read_to_string(dir.join(".svm-link")).unwrap();
    let canonical_src = src.canonicalize().unwrap();
    assert!(marker.contains(&canonical_src.display().to_string()));
}

#[test]
fn rebuilding_source_does_not_change_linked_copy_until_relink() {
    let h = TestHome::new();
    let src = h.home().join("build");
    fs::create_dir_all(&src).unwrap();
    write_exec_script(&src.join("sui"), "echo v1");

    let out = h
        .svm()
        .args(["link", "my-fork", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));

    // Simulate `cargo build` overwriting the source binary in place
    write_exec_script(&src.join("sui"), "echo v2-rebuilt");

    let copy = fs::read_to_string(h.versions_dir().join("my-fork/sui")).unwrap();
    assert!(copy.contains("echo v1"), "the copy must not track the rebuild");

    // Re-linking refreshes the copy
    let out = h
        .svm()
        .args(["link", "my-fork", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let copy = fs::read_to_string(h.versions_dir().join("my-fork/sui")).unwrap();
    assert!(copy.contains("echo v2-rebuilt"));
}

#[test]
fn relink_removes_binaries_the_new_build_no_longer_has() {
    let h = TestHome::new();
    let src = h.home().join("build");
    fs::create_dir_all(&src).unwrap();
    write_exec_script(&src.join("sui"), "echo s1");
    write_exec_script(&src.join("move-analyzer"), "echo m1");

    let out = h
        .svm()
        .args(["link", "my-fork", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));

    // This rebuild produced only sui
    fs::remove_file(src.join("move-analyzer")).unwrap();
    let out = h
        .svm()
        .args(["link", "my-fork", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));

    assert!(h.versions_dir().join("my-fork/sui").exists());
    assert!(
        !h.versions_dir().join("my-fork/move-analyzer").exists(),
        "stale binary from the previous link must be removed"
    );
}

#[test]
fn relink_of_release_shaped_link_name_is_allowed_via_marker() {
    // A linked build under a release-shaped name has regular-file copies; the
    // .svm-link marker is what distinguishes it from an installed release and
    // keeps it refreshable.
    let h = TestHome::new();
    let src = h.home().join("build");
    fs::create_dir_all(&src).unwrap();
    write_exec_script(&src.join("sui"), "echo b1");

    let out = h
        .svm()
        .args(["link", "testnet-v0.0.1", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));

    write_exec_script(&src.join("sui"), "echo b2");
    let out = h
        .svm()
        .args(["link", "testnet-v0.0.1", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "re-link must succeed: {}", stderr_str(&out));
    let copy = fs::read_to_string(h.versions_dir().join("testnet-v0.0.1/sui")).unwrap();
    assert!(copy.contains("echo b2"));
}

#[test]
fn link_refuses_release_dir_even_without_top_level_binaries() {
    // An installed release whose archive layout hid the binaries (no top-level
    // sui) must still be protected — a re-link now replaces the whole dir.
    let h = TestHome::new();
    let release = h.versions_dir().join("mainnet-v1.0.0");
    fs::create_dir_all(release.join("bin")).unwrap();
    write_exec_script(&release.join("bin/sui"), "echo nested-release");

    let src = h.home().join("build");
    fs::create_dir_all(&src).unwrap();
    write_exec_script(&src.join("sui"), "echo local");

    let out = h
        .svm()
        .args(["link", "mainnet-v1.0.0", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("is an installed release"));
    assert!(release.join("bin/sui").exists(), "release contents must survive");
}

#[test]
fn link_source_inside_target_is_refused_and_nothing_is_destroyed() {
    let h = TestHome::new();
    let src = h.home().join("build");
    fs::create_dir_all(&src).unwrap();
    write_exec_script(&src.join("sui"), "echo original");
    let out = h
        .svm()
        .args(["link", "my-fork", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));

    // Pointing link at its own version directory must fail before any wipe
    let inside = h.versions_dir().join("my-fork");
    let out = h
        .svm()
        .args(["link", "my-fork", inside.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("is inside the link target"));
    let copy = fs::read_to_string(inside.join("sui")).unwrap();
    assert!(copy.contains("echo original"), "linked build must be untouched");
}

#[test]
fn failed_copy_leaves_previous_link_intact() {
    let h = TestHome::new();
    let src = h.home().join("build");
    fs::create_dir_all(&src).unwrap();
    write_exec_script(&src.join("sui"), "echo good");
    let out = h
        .svm()
        .args(["link", "my-fork", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));

    // Make the new source unreadable so the staged copy fails
    write_exec_script(&src.join("sui"), "echo broken");
    let mut perms = fs::metadata(src.join("sui")).unwrap().permissions();
    perms.set_mode(0o000);
    fs::set_permissions(src.join("sui"), perms).unwrap();
    if fs::File::open(src.join("sui")).is_ok() {
        eprintln!("skipping: running as root, mode 000 is still readable");
        return;
    }

    let out = h
        .svm()
        .args(["link", "my-fork", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    // The previous link (binaries + marker) survives the failed re-link
    let dir = h.versions_dir().join("my-fork");
    let copy = fs::read_to_string(dir.join("sui")).unwrap();
    assert!(copy.contains("echo good"));
    assert!(dir.join(".svm-link").exists());
    // No staging leftovers
    let staging: Vec<_> = fs::read_dir(h.versions_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".staging-"))
        .collect();
    assert!(staging.is_empty());
}

#[test]
fn link_searches_target_release_by_default() {
    let h = TestHome::new();
    let project = h.home().join("suirepo");
    let release = project.join("target/release");
    fs::create_dir_all(&release).unwrap();
    write_exec_script(&release.join("sui"), "echo from-target-release");

    let out = h.svm().args(["link", "dev"]).current_dir(&project).output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert!(h.versions_dir().join("dev/sui").exists());
}

#[test]
fn link_refuses_to_clobber_installed_release() {
    let h = TestHome::new();
    // A real installed release: regular binaries and no .svm-link marker
    h.install_fake_version("mainnet-v1.0.0", &["sui"]);
    let src = h.home().join("build");
    fs::create_dir_all(&src).unwrap();
    write_exec_script(&src.join("sui"), "echo evil");

    let out = h
        .svm()
        .args(["link", "mainnet-v1.0.0", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("is an installed release"));
    // The installed release is untouched
    let content = fs::read_to_string(h.versions_dir().join("mainnet-v1.0.0/sui")).unwrap();
    assert!(content.contains("mainnet-v1.0.0:sui"));
}

#[test]
fn linked_build_runs_through_use_and_shim_end_to_end() {
    let h = TestHome::new();
    let src = h.home().join("build");
    fs::create_dir_all(&src).unwrap();
    write_exec_script(&src.join("sui"), "echo my-fork:sui \"$@\"");

    let out = h
        .svm()
        .args(["link", "my-fork", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));

    let out = h.svm().args(["use", "my-fork"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(
        fs::read_to_string(h.svm_dir().join("version")).unwrap().trim(),
        "my-fork"
    );

    // `svm use` created the shims; running one dispatches to the copied build
    let shim = h.svm_dir().join("bin/sui");
    assert!(shim.is_symlink());
    let out = h.command_at(&shim).arg("hello").output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), "my-fork:sui hello");

    // And `svm which` reports the copy's path
    let out = h.svm().arg("which").output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(
        stdout_str(&out).trim(),
        h.versions_dir().join("my-fork/sui").display().to_string()
    );
}
