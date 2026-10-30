use super::*;

fn shell(script: &str) -> Command {
    let mut command = Command::new("sh");
    command.args(["-c", script]);
    command
}

#[test]
fn selection_distinguishes_prefix_collisions() {
    let rows = vec!["    mainnet-v1.2.3".into(), "    mainnet-v1.2.30".into()];
    assert_eq!(
        picker_command(shell("sed -n '2p'"), &rows, "Pick").unwrap(),
        Some(1)
    );
}

#[test]
fn selection_preserves_spaces_in_link_names() {
    let rows = vec!["my local build  ".into()];
    assert_eq!(
        picker_command(shell("cat"), &rows, "Pick").unwrap(),
        Some(0)
    );
}

#[test]
fn cancellation_and_no_match_are_successful() {
    let rows = vec!["a".into()];
    for status in [1, 130] {
        assert_eq!(
            picker_command(shell(&format!("exit {status}")), &rows, "Pick").unwrap(),
            None
        );
    }
}

#[test]
fn unknown_selection_and_failure_are_errors() {
    let rows = vec!["a".into()];
    assert!(
        picker_command(shell("echo other"), &rows, "Pick")
            .unwrap_err()
            .to_string()
            .contains("unknown selection")
    );
    assert!(
        picker_command(shell("exit 2"), &rows, "Pick")
            .unwrap_err()
            .to_string()
            .contains("fzf failed")
    );
}

#[test]
fn large_input_can_be_cancelled_without_pipe_deadlock() {
    let rows = vec!["x".repeat(128); 100_000];
    assert_eq!(
        picker_command(shell("exit 130"), &rows, "Pick").unwrap(),
        None
    );
}

#[test]
fn picker_ignores_inherited_fzf_options() {
    let mut command = shell(
        "test -z \"${FZF_DEFAULT_OPTS+x}${FZF_DEFAULT_OPTS_FILE+x}${FZF_DEFAULT_COMMAND+x}\" || exit 2; cat",
    );
    command
        .env("FZF_DEFAULT_OPTS", "--print-query --multi")
        .env("FZF_DEFAULT_OPTS_FILE", "/untrusted/options")
        .env("FZF_DEFAULT_COMMAND", "untrusted");
    assert_eq!(
        picker_command(command, &["a".into()], "Pick").unwrap(),
        Some(0)
    );
}
