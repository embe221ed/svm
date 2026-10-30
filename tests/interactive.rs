mod common;
use common::*;

#[test]
fn interactive_requires_terminal_and_changes_no_selection() {
    let home = TestHome::new();
    home.set_global_version("my-build");
    let out = home.svm().arg("interactive").output().unwrap();
    assert!(!out.status.success());
    assert!(stderr_str(&out).contains("requires a terminal"));
    assert!(stderr_str(&out).contains("svm use <version>"));
    assert_eq!(
        std::fs::read_to_string(home.svm_dir().join("version")).unwrap(),
        "my-build\n"
    );
}

#[test]
fn activation_uses_checkmark_and_no_sparkle_or_ansi() {
    let home = TestHome::new();
    home.install_fake_version("my-build", &["sui"]);
    let out = home
        .svm()
        .args(["use", "my-build"])
        .env("CLICOLOR_FORCE", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr_str(&out));
    assert!(stdout_str(&out).contains("✔ Active version"));
    for output in [stdout_str(&out), stderr_str(&out)] {
        assert!(!output.contains('✨'));
        assert!(!output.contains('\x1b'));
    }
}

#[test]
fn dumb_terminal_disables_forced_color() {
    let home = TestHome::new();
    let out = home
        .svm()
        .arg("show")
        .env_remove("NO_COLOR")
        .env("TERM", "dumb")
        .env("CLICOLOR_FORCE", "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!stdout_str(&out).contains('\x1b'));
}

#[test]
fn list_ignores_staging_files_and_control_character_names() {
    let home = TestHome::new();
    home.install_fake_version("my-build", &["sui"]);
    std::fs::write(home.versions_dir().join("stray-file"), "data").unwrap();
    std::fs::create_dir(home.versions_dir().join(".staging-partial")).unwrap();
    std::fs::create_dir(home.versions_dir().join("bad\x1b[31m-name")).unwrap();
    let out = home.svm().arg("list").output().unwrap();
    assert!(out.status.success());
    let text = stdout_str(&out);
    assert!(text.contains("my-build"));
    assert!(!text.contains("stray-file"));
    assert!(!text.contains(".staging-partial"));
    assert!(!text.contains('\x1b'));
}

#[test]
fn completion_generation_does_not_initialize_home() {
    let home = TestHome::new();
    std::fs::remove_dir_all(home.svm_dir()).unwrap();
    let out = home.svm().args(["completions", "bash"]).output().unwrap();
    assert!(out.status.success());
    assert!(stdout_str(&out).contains("_svm_dynamic"));
    assert!(!home.svm_dir().exists());
}
