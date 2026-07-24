//! Binary-level tests for `svm install`: slim-by-default extraction (only the
//! shim-managed binaries) and the --full escape hatch.
mod common;

use common::*;

const TAG: &str = "mainnet-v1.9.9";

fn release_tgz() -> Vec<u8> {
    make_tgz(&[
        ("sui", b"#!/bin/sh\necho slim:sui \"$@\"\n" as &[u8]),
        ("move-analyzer", b"#!/bin/sh\necho slim:move-analyzer\n"),
        ("sui-debug", b"#!/bin/sh\necho debug\n"),
        ("sui-node", b"#!/bin/sh\necho node\n"),
    ])
}

fn mock_release(server: &mut mockito::ServerGuard, tgz: &[u8]) -> String {
    let (os_part, arch_part) = svm::platform_parts().unwrap();
    let asset_name = format!("sui-{TAG}-{os_part}-{arch_part}.tgz");
    let digest = sha256_hex(tgz);
    server
        .mock("GET", format!("/tags/{TAG}").as_str())
        .with_status(200)
        .with_body(
            serde_json::json!({
                "tag_name": TAG,
                "assets": [{"name": asset_name, "digest": format!("sha256:{digest}")}]
            })
            .to_string(),
        )
        .create();
    server
        .mock("GET", format!("/{TAG}/{asset_name}").as_str())
        .with_status(200)
        .with_body(tgz)
        .create();
    asset_name
}

#[test]
fn install_extracts_only_managed_binaries_by_default() {
    let h = TestHome::new();
    let tgz = release_tgz();
    let mut server = mockito::Server::new();
    let asset_name = mock_release(&mut server, &tgz);

    let out = h
        .svm()
        .args(["install", TAG])
        .env("SVM_API_BASE", server.url())
        .env("SVM_DOWNLOAD_BASE", server.url())
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));

    let dir = h.versions_dir().join(TAG);
    assert!(dir.join("sui").exists());
    assert!(dir.join("move-analyzer").exists());
    assert!(!dir.join("sui-debug").exists(), "sui-debug must be skipped by default");
    assert!(!dir.join("sui-node").exists(), "sui-node must be skipped by default");
    // sui exists at the top level, so no layout warning
    assert!(!stderr_str(&out).contains("layout may have changed"));
    // The verified archive stays cached (e.g. for a later --full reinstall)
    assert!(h.svm_dir().join("cache").join(&asset_name).exists());

    // Nothing was active, so the install auto-activated; the shim dispatches
    // to the slim install end-to-end.
    let out = h.shim("sui").arg("ping").output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), "slim:sui ping");
}

#[test]
fn install_full_extracts_everything() {
    let h = TestHome::new();
    let tgz = release_tgz();
    let mut server = mockito::Server::new();
    mock_release(&mut server, &tgz);

    let out = h
        .svm()
        .args(["install", "--full", TAG])
        .env("SVM_API_BASE", server.url())
        .env("SVM_DOWNLOAD_BASE", server.url())
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));

    let dir = h.versions_dir().join(TAG);
    for bin in ["sui", "move-analyzer", "sui-debug", "sui-node"] {
        assert!(dir.join(bin).exists(), "--full must extract {bin}");
    }

    // The unmanaged binaries are reachable through `svm exec`
    let out = h.svm().args(["exec", TAG, "sui-node"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), "node");
}

#[test]
fn install_full_on_existing_install_hints_at_reinstall() {
    let h = TestHome::new();
    h.install_fake_version(TAG, &["sui", "move-analyzer"]);

    // Exact tag + already installed: no network involved
    let out = h.svm().args(["install", "--full", TAG]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let err = stderr_str(&out);
    assert!(err.contains("already installed"));
    assert!(err.contains("does not re-extract"), "must explain --full needs a reinstall: {err}");
    // Without --full, no such hint
    let out = h.svm().args(["install", TAG]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert!(!stderr_str(&out).contains("does not re-extract"));
}
