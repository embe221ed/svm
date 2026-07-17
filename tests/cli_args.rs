use clap::Parser;
use svm::{CacheAction, Cli, Commands};

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
        Commands::RemoteList { network, pages, .. } => {
            assert_eq!(network, Some("testnet".into()));
            assert_eq!(pages, 5);
        }
        _ => panic!("wrong command"),
    }
}

// --- remote-list --plain ---

#[test]
fn cli_remote_list_plain_flag() {
    let cli = Cli::try_parse_from(["svm", "remote-list", "--plain"]).unwrap();
    match cli.command {
        Commands::RemoteList { plain, .. } => assert!(plain),
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_remote_list_default_not_plain() {
    let cli = Cli::try_parse_from(["svm", "remote-list"]).unwrap();
    match cli.command {
        Commands::RemoteList { plain, .. } => assert!(!plain),
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
        Commands::Install { version, .. } => assert_eq!(version, "my-custom-build"),
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
fn cli_completions_accepts_known_shells() {
    for shell in ["zsh", "bash", "fish"] {
        let cli = Cli::try_parse_from(["svm", "completions", shell]).unwrap();
        match cli.command {
            Commands::Completions { shell: parsed } => assert_eq!(parsed.to_string(), shell),
            _ => panic!("wrong command"),
        }
    }
}

#[test]
fn cli_completions_rejects_unknown_shell() {
    let result = Cli::try_parse_from(["svm", "completions", "tcsh"]);
    assert!(result.is_err());
}

// --- install flags ---

#[test]
fn cli_install_use_flag() {
    let cli = Cli::try_parse_from(["svm", "install", "--use", "v1.0.0"]).unwrap();
    match cli.command {
        Commands::Install { version, use_after } => {
            assert_eq!(version, "v1.0.0");
            assert!(use_after);
        }
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_install_use_short_flag() {
    let cli = Cli::try_parse_from(["svm", "install", "-u", "latest"]).unwrap();
    match cli.command {
        Commands::Install { version, use_after } => {
            assert_eq!(version, "latest");
            assert!(use_after);
        }
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_install_default_not_use() {
    let cli = Cli::try_parse_from(["svm", "install", "v1.0.0"]).unwrap();
    match cli.command {
        Commands::Install { use_after, .. } => assert!(!use_after),
        _ => panic!("wrong command"),
    }
}

// --- remote-list new flags ---

#[test]
fn cli_remote_list_tags_only_flag() {
    let cli = Cli::try_parse_from(["svm", "remote-list", "--tags-only"]).unwrap();
    match cli.command {
        Commands::RemoteList { tags_only, .. } => assert!(tags_only),
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_remote_list_cached_flag() {
    let cli = Cli::try_parse_from(["svm", "remote-list", "--cached", "--tags-only"]).unwrap();
    match cli.command {
        Commands::RemoteList { cached, tags_only, .. } => {
            assert!(cached);
            assert!(tags_only);
        }
        _ => panic!("wrong command"),
    }
}

// --- update ---

#[test]
fn cli_update_no_args() {
    let cli = Cli::try_parse_from(["svm", "update"]).unwrap();
    assert!(matches!(cli.command, Commands::Update));
}

#[test]
fn cli_update_rejects_extra_args() {
    let result = Cli::try_parse_from(["svm", "update", "v1.0.0"]);
    assert!(result.is_err());
}

// --- link with optional path ---

#[test]
fn cli_link_accepts_optional_path() {
    let cli = Cli::try_parse_from(["svm", "link", "my-dev", "/some/build/dir"]).unwrap();
    match cli.command {
        Commands::Link { name, path } => {
            assert_eq!(name, "my-dev");
            assert_eq!(path.unwrap().to_string_lossy(), "/some/build/dir");
        }
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_link_path_defaults_to_none() {
    let cli = Cli::try_parse_from(["svm", "link", "my-dev"]).unwrap();
    match cli.command {
        Commands::Link { path, .. } => assert!(path.is_none()),
        _ => panic!("wrong command"),
    }
}

// --- which ---

#[test]
fn cli_which_defaults_to_sui() {
    let cli = Cli::try_parse_from(["svm", "which"]).unwrap();
    match cli.command {
        Commands::Which { binary } => assert_eq!(binary, "sui"),
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_which_accepts_binary_name() {
    let cli = Cli::try_parse_from(["svm", "which", "move-analyzer"]).unwrap();
    match cli.command {
        Commands::Which { binary } => assert_eq!(binary, "move-analyzer"),
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_which_rejects_extra_args() {
    let result = Cli::try_parse_from(["svm", "which", "sui", "extra"]);
    assert!(result.is_err());
}

// --- exec ---

#[test]
fn cli_exec_requires_version_and_command() {
    assert!(Cli::try_parse_from(["svm", "exec"]).is_err());
    assert!(Cli::try_parse_from(["svm", "exec", "v1.0.0"]).is_err());
}

#[test]
fn cli_exec_captures_command_and_args() {
    let cli = Cli::try_parse_from(["svm", "exec", "testnet-v1.2.3", "sui", "move", "test"]).unwrap();
    match cli.command {
        Commands::Exec { version, command } => {
            assert_eq!(version, "testnet-v1.2.3");
            assert_eq!(command, vec!["sui", "move", "test"]);
        }
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_exec_passes_hyphen_args_through() {
    // Flags after the command belong to the command, not to svm
    let cli = Cli::try_parse_from(["svm", "exec", "v1.0.0", "sui", "--version"]).unwrap();
    match cli.command {
        Commands::Exec { version, command } => {
            assert_eq!(version, "v1.0.0");
            assert_eq!(command, vec!["sui", "--version"]);
        }
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_exec_supports_double_dash_separator() {
    let cli = Cli::try_parse_from(["svm", "exec", "v1.0.0", "--", "sui", "client", "gas"]).unwrap();
    match cli.command {
        Commands::Exec { command, .. } => assert_eq!(command, vec!["sui", "client", "gas"]),
        _ => panic!("wrong command"),
    }
}

// --- cache ---

#[test]
fn cli_cache_no_action() {
    let cli = Cli::try_parse_from(["svm", "cache"]).unwrap();
    match cli.command {
        Commands::Cache { action } => assert!(action.is_none()),
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_cache_clean() {
    let cli = Cli::try_parse_from(["svm", "cache", "clean"]).unwrap();
    match cli.command {
        Commands::Cache { action } => {
            assert!(matches!(action, Some(CacheAction::Clean { all: false })));
        }
        _ => panic!("wrong command"),
    }
}

#[test]
fn cli_cache_clean_all() {
    let cli = Cli::try_parse_from(["svm", "cache", "clean", "--all"]).unwrap();
    match cli.command {
        Commands::Cache { action } => {
            assert!(matches!(action, Some(CacheAction::Clean { all: true })));
        }
        _ => panic!("wrong command"),
    }
}

// --- no subcommand ---

#[test]
fn cli_no_subcommand_errors() {
    let result = Cli::try_parse_from(["svm"]);
    assert!(result.is_err());
}
