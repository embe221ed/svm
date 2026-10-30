//! Binary-level tests for `svm install`: slim-by-default extraction (only the
//! shim-managed binaries) and the --full escape hatch.
mod common;

use common::*;

const TAG: &str = "mainnet-v1.9.9";

#[test]
fn install_does_not_report_a_stray_file_as_installed() {
    let home = TestHome::new();
    let target = home.versions_dir().join(TAG);
    std::fs::write(&target, "unrelated data").unwrap();
    let out = home.svm().args(["install", TAG]).output().unwrap();
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("not a directory"));
    assert!(!stderr_str(&out).contains("already installed"));
    assert_eq!(std::fs::read_to_string(target).unwrap(), "unrelated data");
}

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
    assert!(
        !dir.join("sui-debug").exists(),
        "sui-debug must be skipped by default"
    );
    assert!(
        !dir.join("sui-node").exists(),
        "sui-node must be skipped by default"
    );
    // sui exists at the top level, so no layout warning
    assert!(!stderr_str(&out).contains("layout may have changed"));
    // The verified archive stays cached (e.g. for a later --full reinstall)
    assert!(
        std::fs::read_dir(h.svm_dir().join("cache"))
            .unwrap()
            .any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(&asset_name))
            })
    );

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
    assert!(
        err.contains("does not re-extract"),
        "must explain --full needs a reinstall: {err}"
    );
    // Without --full, no such hint
    let out = h.svm().args(["install", TAG]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert!(!stderr_str(&out).contains("does not re-extract"));
}

#[test]
fn mirror_archives_without_digests_are_cached_per_download_source() {
    let h = TestHome::new();
    let mut api = mockito::Server::new();
    let mut first = mockito::Server::new();
    let mut second = mockito::Server::new();
    let (os, arch) = svm::platform_parts().unwrap();
    let asset_name = format!("sui-{TAG}-{os}-{arch}.tgz");
    let metadata = api
        .mock("GET", format!("/tags/{TAG}").as_str())
        .with_body(
            serde_json::json!({
                "tag_name": TAG,
                "assets": [{"name": asset_name}]
            })
            .to_string(),
        )
        .expect(3)
        .create();
    let first_archive = make_tgz(&[("sui", b"#!/bin/sh\necho first-mirror\n")]);
    let second_archive = make_tgz(&[("sui", b"#!/bin/sh\necho second-mirror\n")]);
    let first_download = first
        .mock("GET", format!("/{TAG}/{asset_name}").as_str())
        .with_body(&first_archive)
        .expect(1)
        .create();
    let second_download = second
        .mock("GET", format!("/{TAG}/{asset_name}").as_str())
        .with_body(&second_archive)
        .expect(1)
        .create();

    // A legacy, unscoped archive must not be used for either mirror.
    let cache = h.svm_dir().join("cache");
    std::fs::create_dir(&cache).unwrap();
    std::fs::write(
        cache.join(&asset_name),
        make_tgz(&[("sui", b"#!/bin/sh\necho legacy-source\n")]),
    )
    .unwrap();
    let install = |source: &str| {
        let out = h
            .svm()
            .args(["install", TAG])
            .env("SVM_API_BASE", api.url())
            .env("SVM_DOWNLOAD_BASE", source)
            .output()
            .unwrap();
        assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    };

    install(&first.url());
    assert_eq!(
        stdout_str(&h.shim("sui").output().unwrap()).trim(),
        "first-mirror"
    );
    assert!(
        h.svm()
            .args(["uninstall", TAG])
            .output()
            .unwrap()
            .status
            .success()
    );
    install(&second.url());
    assert_eq!(
        stdout_str(&h.shim("sui").output().unwrap()).trim(),
        "second-mirror"
    );
    assert!(
        h.svm()
            .args(["uninstall", TAG])
            .output()
            .unwrap()
            .status
            .success()
    );
    // The same endpoint still reuses its complete download.
    install(&second.url());
    assert_eq!(
        stdout_str(&h.shim("sui").output().unwrap()).trim(),
        "second-mirror"
    );
    metadata.assert();
    first_download.assert();
    second_download.assert();
    assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 3);
    assert!(
        h.svm()
            .args(["cache", "clean"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(std::fs::read_dir(cache).unwrap().count(), 0);
}
