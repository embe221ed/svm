use clap::Parser;
use svm::{Cli, Commands};

// --- pages validation ---

#[test]
fn cli_rejects_pages_zero() {
    let result = Cli::try_parse_from(["svm", "remote-list", "-p", "0"]);
    assert!(result.is_err());
}

#[test]
fn cli_rejects_negative_pages() {
    let result = Cli::try_parse_from(["svm", "remote-list", "-p", "-1"]);
    assert!(result.is_err());
}

#[test]
fn cli_rejects_non_numeric_pages() {
    let result = Cli::try_parse_from(["svm", "remote-list", "-p", "abc"]);
    assert!(result.is_err());
}

#[test]
fn cli_accepts_pages_one() {
    let result = Cli::try_parse_from(["svm", "remote-list", "-p", "1"]);
    assert!(result.is_ok());
}

#[test]
fn cli_accepts_large_pages() {
    let cli = Cli::try_parse_from(["svm", "remote-list", "-p", "100"]).unwrap();
    match cli.command {
        Commands::RemoteList { pages, .. } => assert_eq!(pages, 100),
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_pages_defaults_to_three() {
    let cli = Cli::try_parse_from(["svm", "remote-list"]).unwrap();
    match cli.command {
        Commands::RemoteList { pages, .. } => assert_eq!(pages, 3),
        _ => panic!("wrong command"),
    }
}

// --- use command ---

#[test]
fn cli_use_local_long_flag() {
    let cli = Cli::try_parse_from(["svm", "use", "--local", "v1.0"]).unwrap();
    match cli.command {
        Commands::Use { version, local } => {
            assert_eq!(version, "v1.0");
            assert!(local);
        }
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_use_local_short_flag() {
    let cli = Cli::try_parse_from(["svm", "use", "-l", "v1.0"]).unwrap();
    match cli.command {
        Commands::Use { version, local } => {
            assert_eq!(version, "v1.0");
            assert!(local);
        }
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_use_default_not_local() {
    let cli = Cli::try_parse_from(["svm", "use", "v1.0"]).unwrap();
    match cli.command {
        Commands::Use { local, .. } => assert!(!local),
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_use_requires_version_argument() {
    let result = Cli::try_parse_from(["svm", "use"]);
    assert!(result.is_err());
}

#[test]
fn cli_use_local_flag_before_version() {
    // --local can come before the positional arg
    let cli = Cli::try_parse_from(["svm", "use", "--local", "testnet-v1.2.3"]).unwrap();
    match cli.command {
        Commands::Use { version, local } => {
            assert!(local);
            assert_eq!(version, "testnet-v1.2.3");
        }
        _ => panic!("wrong command"),
    }
}

// --- remote-list ---

#[test]
fn cli_remote_list_network_filter() {
    let cli = Cli::try_parse_from(["svm", "remote-list", "-n", "mainnet"]).unwrap();
    match cli.command {
        Commands::RemoteList { network, .. } => assert_eq!(network, Some("mainnet".into())),
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_remote_list_no_network_filter() {
    let cli = Cli::try_parse_from(["svm", "remote-list"]).unwrap();
    match cli.command {
        Commands::RemoteList { network, .. } => assert_eq!(network, None),
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_remote_list_network_and_pages() {
    let cli = Cli::try_parse_from(["svm", "remote-list", "-n", "testnet", "-p", "5"]).unwrap();
    match cli.command {
        Commands::RemoteList { network, pages } => {
            assert_eq!(network, Some("testnet".into()));
            assert_eq!(pages, 5);
        }
        _ => panic!("wrong command"),
    }
}

// --- install/uninstall ---

#[test]
fn cli_install_requires_version() {
    let result = Cli::try_parse_from(["svm", "install"]);
    assert!(result.is_err());
}

#[test]
fn cli_uninstall_requires_version() {
    let result = Cli::try_parse_from(["svm", "uninstall"]);
    assert!(result.is_err());
}

#[test]
fn cli_install_accepts_any_version_string() {
    let cli = Cli::try_parse_from(["svm", "install", "my-custom-build"]).unwrap();
    match cli.command {
        Commands::Install { version } => assert_eq!(version, "my-custom-build"),
        _ => panic!("wrong command"),
    }
}

// --- link ---

#[test]
fn cli_link_requires_name() {
    let result = Cli::try_parse_from(["svm", "link"]);
    assert!(result.is_err());
}

// --- subcommands with no args ---

#[test]
fn cli_list_no_args() {
    let cli = Cli::try_parse_from(["svm", "list"]).unwrap();
    assert!(matches!(cli.command, Commands::List));
}

#[test]
fn cli_show_no_args() {
    let cli = Cli::try_parse_from(["svm", "show"]).unwrap();
    assert!(matches!(cli.command, Commands::Show));
}

#[test]
fn cli_unset_no_args() {
    let cli = Cli::try_parse_from(["svm", "unset"]).unwrap();
    assert!(matches!(cli.command, Commands::Unset));
}

// --- completions ---

#[test]
fn cli_completions_requires_shell() {
    let result = Cli::try_parse_from(["svm", "completions"]);
    assert!(result.is_err());
}

#[test]
fn cli_completions_accepts_shell() {
    let cli = Cli::try_parse_from(["svm", "completions", "zsh"]).unwrap();
    match cli.command {
        Commands::Completions { shell } => assert_eq!(shell, "zsh"),
        _ => panic!("wrong command"),
    }
}

// --- no subcommand ---

#[test]
fn cli_no_subcommand_errors() {
    let result = Cli::try_parse_from(["svm"]);
    assert!(result.is_err());
}
