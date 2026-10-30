//! Regression coverage for moving version pins and lifecycle operations.
mod common;

use common::*;
use std::fs;

#[test]
fn use_channel_and_series_resolves_installed_versions_offline_and_preserves_pin() {
    let h = TestHome::new();
    h.install_fake_version("mainnet-v1.63.3", &["sui"]);
    h.install_fake_version("mainnet-v1.63.11", &["sui"]);
    h.install_fake_version("mainnet-v1.64.0", &["sui"]);
    h.install_fake_version("testnet-v1.63.5", &["sui"]);

    for (spec, expected) in [
        ("v1.63", "mainnet-v1.63.11"),
        ("latest", "mainnet-v1.64.0"),
        ("testnet", "testnet-v1.63.5"),
    ] {
        let out = h.svm().args(["use", spec]).output().unwrap();
        assert!(out.status.success(), "spec {spec}: {}", stderr_str(&out));
        assert_eq!(
            fs::read_to_string(h.svm_dir().join("version"))
                .unwrap()
                .trim(),
            spec
        );
        let out = h.shim("sui").output().unwrap();
        assert!(out.status.success(), "{}", stderr_str(&out));
        assert_eq!(stdout_str(&out).trim(), format!("{expected}:sui"));
    }
}

#[test]
fn use_local_series_pin_follows_new_installed_patches_without_changing_global() {
    let h = TestHome::new();
    h.install_fake_version("mainnet-v1.63.3", &["sui"]);
    h.set_global_version("testnet-v1.0.0");
    let project = h.home().join("project");
    fs::create_dir_all(&project).unwrap();

    let out = h
        .svm()
        .args(["use", "--local", "v1.63"])
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr_str(&out));
    assert_eq!(
        fs::read_to_string(project.join(".svm-version"))
            .unwrap()
            .trim(),
        "v1.63"
    );
    h.install_fake_version("mainnet-v1.63.11", &["sui"]);
    let out = h.shim("sui").current_dir(&project).output().unwrap();
    assert_eq!(stdout_str(&out).trim(), "mainnet-v1.63.11:sui");
    assert_eq!(
        fs::read_to_string(h.svm_dir().join("version"))
            .unwrap()
            .trim(),
        "testnet-v1.0.0"
    );
}

#[test]
fn update_series_pin_stays_within_requested_series_globally_and_locally() {
    for local in [false, true] {
        let h = TestHome::new();
        h.install_fake_version("mainnet-v1.63.3", &["sui"]);
        h.set_global_version(if local { "testnet-v1.0.0" } else { "v1.63" });
        let project = h.home().join("project");
        fs::create_dir_all(&project).unwrap();
        if local {
            fs::write(project.join(".svm-version"), "v1.63\n").unwrap();
        }
        let mut server = mockito::Server::new();
        let target_tag = "mainnet-v1.63.11";
        let (os, arch) = svm::platform_parts().unwrap();
        let asset_name = format!("sui-{target_tag}-{os}-{arch}.tgz");
        let archive = make_tgz(&[("sui", b"#!/bin/sh\necho mainnet-v1.63.11:sui\n")]);
        let assets = server
            .mock("GET", format!("/tags/{target_tag}").as_str())
            .with_status(200)
            .with_body(
                serde_json::json!({"assets": [{
                    "name": asset_name,
                    "digest": format!("sha256:{}", sha256_hex(&archive))
                }]})
                .to_string(),
            )
            .expect(1)
            .create();
        let download = server
            .mock("GET", format!("/{target_tag}/{asset_name}").as_str())
            .with_status(200)
            .with_body(archive)
            .expect(1)
            .create();
        let releases = server
            .mock("GET", "/?per_page=100&page=1")
            .with_status(200)
            .with_body(
                serde_json::json!([
                    {"tag_name": "mainnet-v1.64.0"},
                    {"tag_name": "mainnet-v1.63.11"},
                    {"tag_name": "mainnet-v1.63.3"}
                ])
                .to_string(),
            )
            .expect(1)
            .create();
        let out = h
            .svm()
            .arg("update")
            .current_dir(&project)
            .env("SVM_API_BASE", server.url())
            .env("SVM_DOWNLOAD_BASE", server.url())
            .output()
            .unwrap();
        assert!(out.status.success(), "local={local}: {}", stderr_str(&out));
        assert!(
            !stdout_str(&out).contains("mainnet-v1.64.0"),
            "{}",
            stdout_str(&out)
        );
        assert!(!h.versions_dir().join("mainnet-v1.64.0").exists());
        let pin_file = if local {
            project.join(".svm-version")
        } else {
            h.svm_dir().join("version")
        };
        assert_eq!(fs::read_to_string(pin_file).unwrap().trim(), "v1.63");
        let out = h.shim("sui").current_dir(&project).output().unwrap();
        assert_eq!(stdout_str(&out).trim(), "mainnet-v1.63.11:sui");
        releases.assert();
        assets.assert();
        download.assert();
    }
}

#[test]
fn uninstall_normalized_release_does_not_clear_exact_link_alias() {
    let h = TestHome::new();
    h.install_fake_version("mainnet-v1.0.0", &["sui"]);
    h.install_fake_version("v1.0.0", &["sui"]);
    h.set_global_version("v1.0.0");

    let out = h
        .svm()
        .args(["uninstall", "mainnet-v1.0.0"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr_str(&out));
    assert_eq!(
        fs::read_to_string(h.svm_dir().join("version"))
            .unwrap()
            .trim(),
        "v1.0.0"
    );
    let out = h.shim("sui").output().unwrap();
    assert_eq!(stdout_str(&out).trim(), "v1.0.0:sui");
}

#[test]
fn uninstall_active_channel_uses_remaining_match_then_clears_when_empty() {
    let h = TestHome::new();
    h.install_fake_version("testnet-v1.0.0", &["sui"]);
    h.install_fake_version("testnet-v1.1.0", &["sui"]);
    h.set_global_version("testnet");

    let out = h
        .svm()
        .args(["uninstall", "testnet-v1.1.0"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr_str(&out));
    assert_eq!(
        fs::read_to_string(h.svm_dir().join("version"))
            .unwrap()
            .trim(),
        "testnet"
    );
    let out = h.shim("sui").output().unwrap();
    assert_eq!(stdout_str(&out).trim(), "testnet-v1.0.0:sui");

    let out = h
        .svm()
        .args(["uninstall", "testnet-v1.0.0"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr_str(&out));
    assert!(!h.svm_dir().join("version").exists());
}

#[test]
fn update_rejects_linked_builds_without_network_requests() {
    let h = TestHome::new();
    let mut server = mockito::Server::new();
    let releases = server
        .mock("GET", "/?per_page=100&page=1")
        .with_status(200)
        .with_body("[]")
        .expect(0)
        .create();
    for name in ["my-dev", "testnet-fork"] {
        h.install_fake_version(name, &["sui"]);
        h.set_global_version(name);
        let out = h
            .svm()
            .arg("update")
            .env("SVM_API_BASE", server.url())
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(
            stderr_str(&out).contains("linked/custom build"),
            "{}",
            stderr_str(&out)
        );
        assert!(h.versions_dir().join(name).join("sui").exists());
        assert_eq!(
            fs::read_to_string(h.svm_dir().join("version"))
                .unwrap()
                .trim(),
            name
        );
    }
    releases.assert();
    assert!(!h.svm_dir().join("cache").exists());
}
