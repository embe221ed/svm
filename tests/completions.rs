//! Execute generated completion helpers in real shells. All candidates come
//! from a throwaway home and a fake svm; no completion test uses the network.
mod common;

use clap_complete::Shell;
use common::{stdout_str, write_exec_script};
use std::fs;
use std::process::Command;
use tempfile::TempDir;

fn shell_available(shell: &str) -> bool {
    let available = Command::new(shell)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        available || std::env::var_os("SVM_REQUIRE_SHELL_TESTS").is_none(),
        "{shell} is required when SVM_REQUIRE_SHELL_TESTS is set"
    );
    if !available {
        eprintln!("{shell} is unavailable; skipping execution test");
    }
    available
}

const HOME_COMPONENT: &str = "home with spaces '\" []:$";

struct Fixture {
    tmp: TempDir,
}

impl Fixture {
    fn new(shell: Shell) -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join(HOME_COMPONENT);
        let versions = home.join(".svm/versions");
        fs::create_dir_all(&versions).unwrap();
        for name in ["mainnet-v1.2.3", "my fork", "fork:debug", ".staging-test"] {
            fs::create_dir(versions.join(name)).unwrap();
        }
        fs::write(versions.join("not-a-version"), "stray file").unwrap();
        fs::write(tmp.path().join("completion"), svm::completion_script(shell)).unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir(&bin).unwrap();
        write_exec_script(
            &bin.join("svm"),
            r#"printf '%s\n' "$*" >> "$CALL_LOG"
[ "$*" = 'remote-list --tags-only --cached' ] || exit 88
if [ -n "$CACHED_TAG" ]; then printf '%s\n' "$CACHED_TAG"; fi"#,
        );
        Self { tmp }
    }

    fn run(&self, shell: &str, body: &str, cached_tag: &str) -> String {
        self.run_with(shell, body, cached_tag, &[])
    }

    fn run_with(&self, shell: &str, body: &str, cached_tag: &str, vars: &[(&str, &str)]) -> String {
        let mut cmd = Command::new(shell);
        if shell == "bash" {
            cmd.args(["--noprofile", "--norc", "-c"]);
        } else if shell == "fish" {
            cmd.args(["--no-config", "-c"]);
        } else {
            cmd.arg("-fc");
        }
        let output = cmd
            .arg(body)
            .envs(vars.iter().copied())
            .env("HOME", self.tmp.path().join(HOME_COMPONENT))
            .env("COMPLETION_SCRIPT", self.tmp.path().join("completion"))
            .env("CALL_LOG", self.tmp.path().join("calls"))
            .env("CACHED_TAG", cached_tag)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.tmp.path().join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        stdout_str(&output)
    }

    fn add_versions(&self, names: &[&str]) {
        for name in names {
            fs::create_dir(
                self.tmp
                    .path()
                    .join(HOME_COMPONENT)
                    .join(".svm/versions")
                    .join(name),
            )
            .unwrap();
        }
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.tmp.path().join("calls")).unwrap_or_default()
    }
}

#[test]
fn bash_completes_installed_versions_with_spaces_and_ignores_staging_and_files() {
    let f = Fixture::new(Shell::Bash);
    let result = f.run(
        "bash",
        r#"source "$COMPLETION_SCRIPT"
shopt -s dotglob
COMP_WORDS=(svm use --local '')
COMP_CWORD=3
_svm_dynamic svm '' --local
printf '%s\n' "${COMPREPLY[@]}""#,
        "",
    );
    let candidates: Vec<_> = result.lines().collect();
    assert_eq!(candidates, ["fork:debug", "mainnet-v1.2.3", "my fork"]);
    assert!(f.calls().is_empty());
}

#[test]
fn bash_remote_completion_is_offline_and_keeps_aliases_without_cache() {
    let f = Fixture::new(Shell::Bash);
    let body = r#"source "$COMPLETION_SCRIPT"
COMP_WORDS=(svm install '')
COMP_CWORD=2
_svm_dynamic svm '' install
printf '%s\n' "${COMPREPLY[@]}""#;
    assert_eq!(
        f.run("bash", body, ""),
        "latest\nmainnet\ntestnet\ndevnet\n"
    );
    assert!(
        f.run("bash", body, "mainnet-v1.2.3")
            .contains("mainnet-v1.2.3\n")
    );
    assert_eq!(f.calls(), "remote-list --tags-only --cached\n".repeat(2));
}

#[test]
fn bash_completion_filters_prefix_and_does_not_offer_versions_for_exec_command() {
    let f = Fixture::new(Shell::Bash);
    assert_eq!(
        f.run(
            "bash",
            r#"source "$COMPLETION_SCRIPT"
COMP_WORDS=(svm use 'my ')
COMP_CWORD=2
_svm_dynamic svm 'my ' use
printf '%s\n' "${COMPREPLY[@]}""#,
            ""
        ),
        "my fork\n"
    );
    let result = f.run(
        "bash",
        r#"source "$COMPLETION_SCRIPT"
_svm() { printf 'static-completion\n'; }
COMP_WORDS=(svm exec mainnet-v1.2.3 '')
COMP_CWORD=3
_svm_dynamic svm '' mainnet-v1.2.3"#,
        "",
    );
    assert_eq!(result, "static-completion\n");
    assert!(f.calls().is_empty());
}

#[test]
fn zsh_completes_names_without_splitting_or_interpreting_colons() {
    if !shell_available("zsh") {
        return;
    }
    let f = Fixture::new(Shell::Zsh);
    let result = f.run(
        "zsh",
        r#"compdef() { :; }
compadd() { local name=$argv[-1]; print -rl -- "${(@P)name}"; }
source "$COMPLETION_SCRIPT"
setopt glob_dots
_svm_local_versions"#,
        "",
    );
    assert_eq!(result, "fork:debug\nmainnet-v1.2.3\nmy fork\n");
    assert!(f.calls().is_empty());
}

#[test]
fn zsh_remote_completion_is_offline_even_without_cache() {
    if !shell_available("zsh") {
        return;
    }
    let f = Fixture::new(Shell::Zsh);
    let body = r#"compdef() { :; }
compadd() { local name=$argv[-1]; print -rl -- "${(@P)name}"; }
source "$COMPLETION_SCRIPT"
_svm_remote_versions"#;
    assert_eq!(f.run("zsh", body, ""), "latest\nmainnet\ntestnet\ndevnet\n");
    assert!(
        f.run("zsh", body, "mainnet-v1.2.3")
            .contains("mainnet-v1.2.3\n")
    );
    assert_eq!(f.calls(), "remote-list --tags-only --cached\n".repeat(2));
}

#[test]
fn fish_completes_local_and_cached_remote_versions() {
    if !shell_available("fish") {
        return;
    }
    let f = Fixture::new(Shell::Fish);
    let result = f.run(
        "fish",
        r#"source "$COMPLETION_SCRIPT"
__svm_local_versions
__svm_remote_versions"#,
        "mainnet-v1.2.3",
    );
    assert!(result.contains("my fork\n"));
    assert!(result.contains("fork:debug\n"));
    assert!(!result.contains(".staging-test"));
    assert!(!result.contains("not-a-version"));
    assert!(result.contains("latest\n"));
    assert_eq!(f.calls(), "remote-list --tags-only --cached\n");
}

#[test]
fn fish_completion_uses_version_argument_context() {
    if !shell_available("fish") {
        return;
    }
    let f = Fixture::new(Shell::Fish);
    let local = f.run(
        "fish",
        r#"source "$COMPLETION_SCRIPT"
complete -C 'svm use --local my'"#,
        "",
    );
    assert!(local.contains("my fork"), "{local}");
    let remote = f.run(
        "fish",
        r#"source "$COMPLETION_SCRIPT"
complete -C 'svm install lat'"#,
        "",
    );
    assert!(remote.contains("latest"), "{remote}");
    let command = f.run(
        "fish",
        r#"source "$COMPLETION_SCRIPT"
complete -C 'svm exec mainnet-v1.2.3 my'"#,
        "",
    );
    assert!(!command.contains("my fork"), "{command}");
    assert!(
        f.calls()
            .lines()
            .all(|line| line == "remote-list --tags-only --cached")
    );
}

#[test]
fn bash_registered_completion_decodes_quotes_escapes_and_word_breaks() {
    let f = Fixture::new(Shell::Bash);
    f.add_versions(&["fork=debug", "single'quote", "double\"quote", "débogue"]);
    let body = r#"source "$COMPLETION_SCRIPT"
registered=$(complete -p svm)
entrypoint=${registered##* -F }
entrypoint=${entrypoint%% *}
COMP_WORDS=(svm use '')
COMP_CWORD=2
COMP_LINE=$QUERY
COMP_POINT=$QUERY_POINT
"$entrypoint" svm '' use
printf '%s\n' "${COMPREPLY[@]}""#;
    for (query, expected) in [
        ("svm use fork:d", "debug\n"),
        ("svm use fork=", "debug\n"),
        ("svm use \"my f", "my fork\n"),
        ("svm use my\\ f", "my fork\n"),
        ("svm use 'my f", "my fork\n"),
        ("svm use single\\'", "single'quote\n"),
        ("svm use double\\\"", "double\"quote\n"),
        ("svm use dé", "débogue\n"),
        ("svm use 'fork:d", "fork:debug\n"),
        ("svm use fork\\:d", "fork:debug\n"),
    ] {
        assert_eq!(
            f.run_with(
                "bash",
                body,
                "",
                &[("QUERY", query), ("QUERY_POINT", &query.len().to_string())]
            ),
            expected,
            "{query}"
        );
    }
    assert!(f.calls().is_empty());
}

#[test]
fn installed_completion_candidates_follow_version_name_validation() {
    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        let executable = shell.to_string();
        if !shell_available(&executable) {
            continue;
        }
        let f = Fixture::new(shell);
        f.add_versions(&[
            "-invalid",
            " leading",
            "trailing ",
            "tab\tname",
            "line\nname",
            "back\\slash",
            "double\"quote",
            "single'quote",
        ]);
        let body = match shell {
            Shell::Bash => {
                r#"source "$COMPLETION_SCRIPT"
COMP_WORDS=(svm use '')
COMP_CWORD=2
_svm_dynamic svm '' use
printf '%s\n' "${COMPREPLY[@]}""#
            }
            Shell::Zsh => {
                r#"compdef() { :; }
compadd() { local name=$argv[-1]; print -rl -- "${(@P)name}"; }
source "$COMPLETION_SCRIPT"
_svm_local_versions"#
            }
            Shell::Fish => {
                r#"source "$COMPLETION_SCRIPT"
complete -C 'svm use '"#
            }
            _ => unreachable!(),
        };
        let result = f.run(&executable, body, "");
        let candidates: Vec<_> = result.lines().collect();
        assert_eq!(
            candidates,
            [
                "double\"quote",
                "fork:debug",
                "mainnet-v1.2.3",
                "my fork",
                "single'quote"
            ],
            "{shell}"
        );
    }
}
