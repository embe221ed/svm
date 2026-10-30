//! Cross-process coordination and deterministic staging/atomicity regressions.
mod common;
use common::*;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

fn hold_mutation_lock(home: &TestHome) -> File {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.svm_dir().join("mutation.lock"))
        .unwrap();
    file.lock().unwrap();
    file
}

fn wait_until_waiting(child: &mut Child, log: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if fs::read_to_string(log)
            .unwrap_or_default()
            .contains("Waiting for another svm operation")
        {
            assert!(child.try_wait().unwrap().is_none());
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!(
                "command exited instead of waiting for mutation lock ({status}): {}",
                fs::read_to_string(log).unwrap_or_default()
            );
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("command did not report waiting for mutation lock");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn finish(mut child: Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("command did not finish; possible nested mutation-lock deadlock");
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn mutations_wait_for_lock_and_read_only_commands_remain_responsive() {
    let cases: &[(&[&str], bool)] = &[
        (&["cache", "clean", "--all"], true),
        (&["install", "mainnet-v1.0.0", "--use"], true),
        (&["use", "mainnet-v1.0.0"], true),
        (&["link", "my-build", "build"], true),
        (&["uninstall", "mainnet-v1.0.0"], true),
        (&["unset"], true),
        (&["update"], false), // A custom build is rejected, after acquiring the lock.
    ];
    for (args, expected_success) in cases {
        let home = TestHome::new();
        home.install_fake_version("mainnet-v1.0.0", &["sui"]);
        home.install_fake_version("my-build", &["sui"]);
        home.set_global_version("my-build");
        fs::create_dir(home.home().join("build")).unwrap();
        write_exec_script(&home.home().join("build/sui"), "echo linked");
        fs::create_dir_all(home.svm_dir().join("cache")).unwrap();
        fs::write(
            home.svm_dir().join("cache/active.partial"),
            b"partial download",
        )
        .unwrap();
        fs::create_dir(home.versions_dir().join(".staging-active")).unwrap();
        fs::write(
            home.versions_dir().join(".staging-active/sui"),
            b"extracting",
        )
        .unwrap();
        let lock = hold_mutation_lock(&home);
        let log = home.home().join("stderr");
        let mut child = home
            .svm()
            .args(*args)
            .stdout(Stdio::piped())
            .stderr(Stdio::from(File::create(&log).unwrap()))
            .spawn()
            .unwrap();
        wait_until_waiting(&mut child, &log);
        assert!(
            home.svm_dir().join("cache/active.partial").exists(),
            "{args:?}"
        );
        assert!(
            home.versions_dir().join(".staging-active/sui").exists(),
            "{args:?}"
        );
        assert_eq!(
            fs::read_to_string(home.svm_dir().join("version")).unwrap(),
            "my-build\n"
        );
        for read_args in [
            &["list"][..],
            &["show"],
            &["which"],
            &["remote-list", "--cached", "--tags-only"],
        ] {
            let child = home
                .svm()
                .args(read_args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let out = finish(child);
            assert!(out.status.success(), "{read_args:?}: {}", stderr_str(&out));
        }
        drop(lock);
        let out = finish(child);
        assert_eq!(
            out.status.success(),
            *expected_success,
            "{args:?}: {}",
            fs::read_to_string(log).unwrap()
        );
    }
}

#[test]
fn shim_rechecks_after_wait_and_releases_lock_before_exec() {
    let home = TestHome::new();
    let tag = "mainnet-v1.0.0";
    home.set_global_version(tag);
    let lock = hold_mutation_lock(&home);
    let mut server = mockito::Server::new();
    let lookup = server
        .mock("GET", "/tags/mainnet-v1.0.0")
        .with_status(500)
        .expect(0)
        .create();
    let log = home.home().join("stderr");
    let mut child = home
        .shim("sui")
        .env("SVM_AUTO_INSTALL", "1")
        .env("SVM_API_BASE", server.url())
        .env("SVM_DOWNLOAD_BASE", server.url())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(File::create(&log).unwrap()))
        .spawn()
        .unwrap();
    wait_until_waiting(&mut child, &log);
    // Simulate the installation that held the lock while this shim waited.
    let dir = home.install_fake_version(tag, &["sui"]);
    write_exec_script(
        &dir.join("sui"),
        &format!("'{}' use '{tag}'", env!("CARGO_BIN_EXE_svm")),
    );
    drop(lock);
    let out = finish(child);
    assert!(out.status.success(), "{}", fs::read_to_string(log).unwrap());
    assert!(stdout_str(&out).contains("Active version set to"));
    lookup.assert();
}

#[test]
fn failed_unpack_promotion_cleans_staging_and_preserves_existing_version_and_archive() {
    let home = TestHome::new();
    let tag = "mainnet-v1.0.0";
    home.install_fake_version(tag, &["sui"]);
    let original = fs::read(home.versions_dir().join(tag).join("sui")).unwrap();
    let archive = home.home().join("release.tgz");
    fs::write(&archive, make_tgz(&[("sui", b"replacement")])).unwrap();
    assert!(svm::unpack_archive(&archive, &home.versions_dir(), tag, false).is_err());
    assert_eq!(
        fs::read(home.versions_dir().join(tag).join("sui")).unwrap(),
        original
    );
    assert!(archive.exists());
    assert!(!fs::read_dir(home.versions_dir()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".staging-")
    }));
}

fn fifo_writer(path: PathBuf) -> File {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(OpenOptions::new().write(true).open(path));
    });
    receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("archive reader did not open FIFO")
        .unwrap()
}

#[test]
fn concurrent_unpacks_in_one_process_have_independent_staging_and_cleanup() {
    let home = TestHome::new();
    let tag = "mainnet-v1.0.0";
    let mut writers = Vec::new();
    let (sender, receiver) = mpsc::channel();
    for index in 0..2 {
        let archive = home.home().join(format!("archive-{index}"));
        assert!(
            Command::new("mkfifo")
                .arg(&archive)
                .status()
                .unwrap()
                .success()
        );
        let versions = home.versions_dir();
        let send = sender.clone();
        let path = archive.clone();
        thread::spawn(move || {
            let _ = send.send(svm::unpack_archive(&path, &versions, tag, false));
        });
        // Opening the writing end proves each unpack has created staging and
        // opened its archive. Keep both readers blocked until we count staging.
        writers.push(fifo_writer(archive));
    }
    let staged = fs::read_dir(home.versions_dir())
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".staging-")
        })
        .count();
    let bytes = make_tgz(&[("sui", b"complete toolchain")]);
    for mut writer in writers {
        writer.write_all(&bytes).unwrap();
    }
    let results = [
        receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
        receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
    ];
    assert_eq!(
        staged, 2,
        "same-process unpacks must never share a staging directory"
    );
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        fs::read(home.versions_dir().join(tag).join("sui")).unwrap(),
        b"complete toolchain"
    );
    assert!(!fs::read_dir(home.versions_dir()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".staging-")
    }));
}

#[test]
fn activation_replaces_version_files_without_truncating_existing_readers() {
    for local in [false, true] {
        let home = TestHome::new();
        home.install_fake_version("my-build", &["sui"]);
        let path = if local {
            home.home().join(".svm-version")
        } else {
            home.svm_dir().join("version")
        };
        fs::write(&path, "previous-build\n").unwrap();
        let mut old_reader = File::open(&path).unwrap();
        let mut command = home.svm();
        command.args(["use", "my-build"]);
        if local {
            command.arg("--local");
        }
        let out = command.output().unwrap();
        assert!(out.status.success(), "{}", stderr_str(&out));
        let mut previous = String::new();
        old_reader.read_to_string(&mut previous).unwrap();
        assert_eq!(
            previous, "previous-build\n",
            "existing readers must retain their complete version snapshot"
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "my-build\n");
    }
}

#[test]
fn concurrent_installs_download_once_and_finish_without_nested_lock_deadlocks() {
    let home = TestHome::new();
    let tag = "mainnet-v1.0.0";
    let (os, arch) = svm::platform_parts().unwrap();
    let asset_name = format!("sui-{tag}-{os}-{arch}.tgz");
    let archive = make_tgz(&[("sui", b"#!/bin/sh\necho installed-once\n")]);
    let mut server = mockito::Server::new();
    let lookup = server
        .mock("GET", format!("/tags/{tag}").as_str())
        .with_status(200)
        .with_body(
            serde_json::json!({"assets": [{
                "name": asset_name, "digest": format!("sha256:{}", sha256_hex(&archive))
            }]})
            .to_string(),
        )
        .expect(1)
        .create();
    let download = server
        .mock("GET", format!("/{tag}/{asset_name}").as_str())
        .with_status(200)
        .with_body(archive)
        .expect(1)
        .create();
    let lock = hold_mutation_lock(&home);
    let mut children = Vec::new();
    for index in 0..2 {
        let log = home.home().join(format!("stderr-{index}"));
        let mut child = home
            .svm()
            .args(["install", tag, "--use"])
            .env("SVM_API_BASE", server.url())
            .env("SVM_DOWNLOAD_BASE", server.url())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(File::create(&log).unwrap()))
            .spawn()
            .unwrap();
        wait_until_waiting(&mut child, &log);
        children.push((child, log));
    }
    drop(lock);
    for (child, log) in children {
        let out = finish(child);
        assert!(out.status.success(), "{}", fs::read_to_string(log).unwrap());
    }
    lookup.assert();
    download.assert();
    let out = home.shim("sui").output().unwrap();
    assert!(out.status.success(), "{}", stderr_str(&out));
    assert_eq!(stdout_str(&out).trim(), "installed-once");
    assert!(!fs::read_dir(home.versions_dir()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".staging-")
    }));
}

#[test]
fn unpack_creates_missing_versions_directory_for_first_shim_install() {
    let home = TestHome::new();
    fs::remove_dir(home.versions_dir()).unwrap();
    let archive = home.home().join("release.tgz");
    fs::write(&archive, make_tgz(&[("sui", b"first toolchain")])).unwrap();
    svm::unpack_archive(&archive, &home.versions_dir(), "mainnet-v1.0.0", false).unwrap();
    assert_eq!(
        fs::read(home.versions_dir().join("mainnet-v1.0.0/sui")).unwrap(),
        b"first toolchain"
    );
}
