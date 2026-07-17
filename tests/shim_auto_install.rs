//! Binary-level tests for shim dispatch (argv[0]) and shim auto-install.
mod common;

use common::*;
use std::fs;

#[test]
fn shim_dispatches_to_active_version_with_arg_passthrough() {
    let h = TestHome::new();
    h.install_fake_version("mainnet-v1.0.0", &["sui", "move-analyzer"]);
    h.set_global_version("mainnet-v1.0.0");

    let out = h.shim("sui").args(["client", "--json"]).output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), "mainnet-v1.0.0:sui client --json");

    let out = h.shim("move-analyzer").output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), "mainnet-v1.0.0:move-analyzer");
}

#[test]
fn shim_respects_local_pin_over_global() {
    let h = TestHome::new();
    h.install_fake_version("mainnet-v1.0.0", &["sui"]);
    h.install_fake_version("testnet-v2.0.0", &["sui"]);
    h.set_global_version("mainnet-v1.0.0");

    let project = h.home().join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join(".svm-version"), "testnet-v2.0.0\n").unwrap();

    let out = h.shim("sui").current_dir(&project).output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), "testnet-v2.0.0:sui");
}

#[test]
fn shim_missing_version_fails_when_not_interactive() {
    // SVM_AUTO_INSTALL unset + no TTY: no prompt, no hang, the original error
    let h = TestHome::new();
    h.set_global_version("mainnet-v9.9.9");
    let out = h.shim("sui").output().unwrap();
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("is set but not installed"));
}

#[test]
fn shim_auto_install_disabled_by_falsy_env() {
    let h = TestHome::new();
    h.set_global_version("mainnet-v9.9.9");
    for value in ["0", "false", "no", "n", "off"] {
        let out = h.shim("sui").env("SVM_AUTO_INSTALL", value).output().unwrap();
        assert!(!out.status.success(), "SVM_AUTO_INSTALL={value} must not install");
        assert!(stderr_str(&out).contains("is set but not installed"));
    }
}

#[test]
fn shim_auto_install_truthy_values_attempt_install() {
    // Endpoints point at a dead port: the attempt must start (proving the
    // truthy value was recognized) and then fail on the network.
    let h = TestHome::new();
    h.set_global_version("mainnet-v9.9.9");
    for value in ["true", "yes", "y", "on"] {
        let out = h
            .shim("sui")
            .env("SVM_AUTO_INSTALL", value)
            .env("SVM_API_BASE", "http://127.0.0.1:1")
            .env("SVM_DOWNLOAD_BASE", "http://127.0.0.1:1")
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(
            stderr_str(&out).contains("installing it now"),
            "SVM_AUTO_INSTALL={value} must trigger an install attempt, stderr: {}",
            stderr_str(&out)
        );
    }
}

#[test]
fn shim_auto_install_unrecognized_value_declines_when_not_interactive() {
    let h = TestHome::new();
    h.set_global_version("mainnet-v9.9.9");
    let out = h.shim("sui").env("SVM_AUTO_INSTALL", "maybe").output().unwrap();
    assert!(!out.status.success());
    let err = stderr_str(&out);
    assert!(err.contains("is set but not installed"));
    assert!(!err.contains("installing it now"));
}

#[test]
fn shim_resolves_series_and_channel_pins_offline() {
    // A pin like "v1.0" or "testnet" resolves against installed versions with
    // no network access and no auto-install round-trip.
    let h = TestHome::new();
    h.install_fake_version("mainnet-v1.0.3", &["sui"]);
    h.install_fake_version("mainnet-v1.0.5", &["sui"]);
    h.install_fake_version("testnet-v2.0.0", &["sui"]);

    h.set_global_version("v1.0");
    let out = h.shim("sui").env("SVM_AUTO_INSTALL", "1").output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), "mainnet-v1.0.5:sui");
    assert!(
        !stderr_str(&out).contains("installing"),
        "resolved pins must not re-trigger installs"
    );

    h.set_global_version("testnet");
    let out = h.shim("sui").env("SVM_AUTO_INSTALL", "1").output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), "testnet-v2.0.0:sui");
}

#[test]
fn shim_auto_install_of_series_pin_converges() {
    // First run: nothing installed, the series pin "v1.9" resolves remotely
    // and installs. Second run: resolves against the installed version with
    // zero network traffic (every mock allows exactly one hit).
    let h = TestHome::new();
    let tag = "mainnet-v1.9.9";
    let (os_part, arch_part) = svm::platform_parts().unwrap();
    let asset_name = format!("sui-{tag}-{os_part}-{arch_part}.tgz");
    let tgz = make_tgz(&[("sui", b"#!/bin/sh\necho series:sui \"$@\"\n" as &[u8])]);
    let digest = sha256_hex(&tgz);

    let mut server = mockito::Server::new();
    let list = server
        .mock("GET", "/")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_body(serde_json::json!([{"tag_name": tag}]).to_string())
        .expect(1)
        .create();
    let assets = server
        .mock("GET", format!("/tags/{tag}").as_str())
        .with_status(200)
        .with_body(
            serde_json::json!({
                "tag_name": tag,
                "assets": [{"name": asset_name, "digest": format!("sha256:{digest}")}]
            })
            .to_string(),
        )
        .expect(1)
        .create();
    let download = server
        .mock("GET", format!("/{tag}/{asset_name}").as_str())
        .with_status(200)
        .with_body(tgz)
        .expect(1)
        .create();

    h.set_global_version("v1.9");
    let out = h
        .shim("sui")
        .arg("first")
        .env("SVM_AUTO_INSTALL", "1")
        .env("SVM_API_BASE", server.url())
        .env("SVM_DOWNLOAD_BASE", server.url())
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), "series:sui first");

    // Second run keeps the mock endpoints wired: any request would exceed
    // the expect(1) budgets and fail the assertions below.
    let out = h
        .shim("sui")
        .arg("second")
        .env("SVM_AUTO_INSTALL", "1")
        .env("SVM_API_BASE", server.url())
        .env("SVM_DOWNLOAD_BASE", server.url())
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), "series:sui second");
    assert!(
        !stderr_str(&out).contains("installing"),
        "second run must resolve the installed version"
    );

    list.assert();
    assets.assert();
    download.assert();
}

#[test]
fn shim_auto_install_downloads_verifies_installs_and_execs() {
    let h = TestHome::new();
    let tag = "mainnet-v1.9.9";
    let (os_part, arch_part) = svm::platform_parts().unwrap();
    let asset_name = format!("sui-{tag}-{os_part}-{arch_part}.tgz");

    let script = "#!/bin/sh\necho auto-installed:sui \"$@\"\n";
    let tgz = make_tgz(&[("sui", script.as_bytes())]);
    let digest = sha256_hex(&tgz);

    let mut server = mockito::Server::new();
    let assets = server
        .mock("GET", format!("/tags/{tag}").as_str())
        .with_status(200)
        .with_body(
            serde_json::json!({
                "tag_name": tag,
                "assets": [{"name": asset_name, "digest": format!("sha256:{digest}")}]
            })
            .to_string(),
        )
        .create();
    let download = server
        .mock("GET", format!("/{tag}/{asset_name}").as_str())
        .with_status(200)
        .with_body(tgz)
        .create();

    // The pin comes from a repo-local .svm-version, like a freshly cloned project
    let project = h.home().join("cloned-repo");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join(".svm-version"), format!("{tag}\n")).unwrap();

    let out = h
        .shim("sui")
        .arg("--version")
        .current_dir(&project)
        .env("SVM_AUTO_INSTALL", "1")
        .env("SVM_API_BASE", server.url())
        .env("SVM_DOWNLOAD_BASE", server.url())
        .output()
        .unwrap();

    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    // stdout carries ONLY the exec'd binary's output — install chatter is stderr
    assert_eq!(stdout_str(&out).trim(), "auto-installed:sui --version");
    assert!(stderr_str(&out).contains("SHA-256 checksum verified"));
    assets.assert();
    download.assert();

    // Installed under versions/, and the global default was NOT touched:
    // the local pin already selects this version.
    assert!(h.versions_dir().join(tag).join("sui").exists());
    assert!(!h.svm_dir().join("version").exists());

    // Second invocation resolves locally — no network endpoints configured
    let out = h
        .shim("sui")
        .arg("ok")
        .current_dir(&project)
        .env("SVM_AUTO_INSTALL", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), "auto-installed:sui ok");
}

#[test]
fn shim_auto_install_checksum_mismatch_fails_and_installs_nothing() {
    let h = TestHome::new();
    let tag = "mainnet-v1.8.8";
    let (os_part, arch_part) = svm::platform_parts().unwrap();
    let asset_name = format!("sui-{tag}-{os_part}-{arch_part}.tgz");
    let tgz = make_tgz(&[("sui", b"#!/bin/sh\necho evil\n")]);

    let mut server = mockito::Server::new();
    server
        .mock("GET", format!("/tags/{tag}").as_str())
        .with_status(200)
        .with_body(
            serde_json::json!({
                "tag_name": tag,
                "assets": [{"name": asset_name, "digest": format!("sha256:{}", "0".repeat(64))}]
            })
            .to_string(),
        )
        .create();
    server
        .mock("GET", format!("/{tag}/{asset_name}").as_str())
        .with_status(200)
        .with_body(tgz)
        .expect_at_least(1)
        .create();

    h.set_global_version(tag);
    let out = h
        .shim("sui")
        .env("SVM_AUTO_INSTALL", "1")
        .env("SVM_API_BASE", server.url())
        .env("SVM_DOWNLOAD_BASE", server.url())
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("SHA-256 verification failed"));
    assert!(!h.versions_dir().join(tag).exists(), "nothing must be installed");
}
