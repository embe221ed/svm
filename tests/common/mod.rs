//! Shared helpers for binary-level tests: a throwaway $HOME with the ~/.svm
//! layout, fake version directories whose binaries are shell scripts, and
//! hermetic Command builders for the compiled svm binary and its shims.
#![allow(dead_code)]

use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

pub struct TestHome {
    pub tmp: TempDir,
}

impl TestHome {
    pub fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".svm/versions")).unwrap();
        Self { tmp }
    }

    pub fn home(&self) -> &Path {
        self.tmp.path()
    }

    pub fn svm_dir(&self) -> PathBuf {
        self.tmp.path().join(".svm")
    }

    pub fn versions_dir(&self) -> PathBuf {
        self.svm_dir().join("versions")
    }

    /// Create an installed version whose binaries are shell scripts echoing
    /// "<version>:<binary>" plus their arguments.
    pub fn install_fake_version(&self, name: &str, binaries: &[&str]) -> PathBuf {
        let dir = self.versions_dir().join(name);
        fs::create_dir_all(&dir).unwrap();
        for bin in binaries {
            write_exec_script(&dir.join(bin), &format!("echo {name}:{bin} \"$@\""));
        }
        dir
    }

    pub fn set_global_version(&self, version: &str) {
        fs::write(self.svm_dir().join("version"), format!("{version}\n")).unwrap();
    }

    /// The compiled svm binary with a hermetic environment.
    pub fn svm(&self) -> Command {
        self.command_at(Path::new(env!("CARGO_BIN_EXE_svm")))
    }

    /// The compiled svm binary invoked through a shim named `binary`
    /// (argv[0] dispatch), using a shim directory private to this home.
    pub fn shim(&self, binary: &str) -> Command {
        let shim_dir = self.home().join("shimbin");
        fs::create_dir_all(&shim_dir).unwrap();
        let shim = shim_dir.join(binary);
        if !shim.exists() {
            symlink(env!("CARGO_BIN_EXE_svm"), &shim).unwrap();
        }
        self.command_at(&shim)
    }

    /// Hermetic Command for an arbitrary executable (e.g. a shim that
    /// `svm use` created under this home's ~/.svm/bin). The default cwd is
    /// the temp home so a .svm-version in an ancestor of the test-harness
    /// cwd can never leak in; tests override with .current_dir() as needed.
    pub fn command_at(&self, path: &Path) -> Command {
        let mut cmd = Command::new(path);
        cmd.current_dir(self.home())
            .env("HOME", self.home())
            .env_remove("SVM_AUTO_INSTALL")
            .env_remove("SVM_API_BASE")
            .env_remove("SVM_DOWNLOAD_BASE")
            .env_remove("GITHUB_TOKEN")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null());
        cmd
    }
}

pub fn write_exec_script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

/// Gzipped tar archive with the given entries, each mode 0755.
pub fn make_tgz(entries: &[(&str, &[u8])]) -> Vec<u8> {
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

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

pub fn stdout_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn stderr_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}
