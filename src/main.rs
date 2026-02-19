use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand, CommandFactory};
use colored::*;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use clap_complete::{generate, shells::Zsh};

const GITHUB_API: &str = "https://api.github.com/repos/MystenLabs/sui/releases";
const USER_AGENT: &str = "svm-cli";
const SVM_BINARIES: &[&str] = &["sui", "move-analyzer"];

#[derive(Parser)]
#[command(name = "svm")]
#[command(about = "Sui Version Manager", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List available versions on GitHub
    RemoteList {
        #[arg(short, long)]
        network: Option<String>, // e.g., "mainnet", "testnet"
    },
    /// Install a version (e.g., v1.63.4 or mainnet-v1.63.4)
    Install { version: String },
    /// Switch to an installed/linked version
    Use { version: String },
    /// Link a local build (e.g., svm link custom-dev)
    Link { name: String },
    /// List local versions (active one marked with *)
    List,
    /// Show the currently active version
    Show,
    /// Deactivate svm by removing shims
    Unset,
    /// Generate shell completions
    Completions { shell: String },
}

#[derive(Debug, PartialEq)]
enum VersionSource {
    Global,
    Local(PathBuf),
}

fn main() -> Result<()> {
    // 1. Shim Detection
    let args: Vec<String> = std::env::args().collect();
    if let Some(program_name) = args.get(0).and_then(|p| Path::new(p).file_name()) {
        let program_name = program_name.to_string_lossy();
        if SVM_BINARIES.contains(&program_name.as_ref()) {
            return run_shim(&program_name, &args[1..]);
        }
    }

    // 2. Standard CLI
    let cli = Cli::parse();
    let svm_dir = dirs::home_dir().context("No home dir")?.join(".svm");
    let versions_dir = svm_dir.join("versions");
    let bin_dir = svm_dir.join("bin");

    fs::create_dir_all(&versions_dir)?;
    fs::create_dir_all(&bin_dir)?;

    match cli.command {
        Commands::RemoteList { network } => list_remote(network)?,
        Commands::Install { version } => install_version(&version, &versions_dir)?,
        Commands::Use { version } => use_version(&version, &versions_dir, &bin_dir)?,
        Commands::Link { name } => link_local(&name, &versions_dir)?,
        Commands::List => list_local(&versions_dir, &svm_dir)?,
        Commands::Show => show_version(&svm_dir)?,
        Commands::Unset => unset_version(&bin_dir, &svm_dir)?,
        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            let bin_name = "svm";

            match shell.to_lowercase().as_str() {
                "zsh" => {
                    let mut buffer = Vec::new();
                    generate(Zsh, &mut cmd, bin_name, &mut buffer);
                    let script = String::from_utf8(buffer)?;

                    // 1. Define the custom completion function logic
                    let custom_func = r#"
        _svm_versions() {
            local -a versions
            versions=($(ls -1 $HOME/.svm/versions 2>/dev/null))
            _describe 'installed versions' versions
        }
        "#;

                    // 2. Inject the function at the top (after the #compdef line)
                    let mut final_script = script.replace("#compdef svm", &format!("#compdef svm\n{}", custom_func));

                    // 3. Replace the generic _default with our custom function in the 'use' block
                    // We target the specific pattern clap generated for your 'use' case
                    final_script = final_script.replace("':version:_default'", "':version:_svm_versions'");

                    println!("{}", final_script);
                }
                _ => return Err(anyhow!("Unsupported shell: {}", shell)),
            }
        }
    }
    Ok(())
}

fn resolve_version(svm_dir: &Path) -> Result<Option<(String, VersionSource)>> {
    let mut current_dir = std::env::current_dir().ok();
    while let Some(dir) = current_dir {
        let version_file = dir.join(".svm-version");
        if version_file.exists() {
            let v = fs::read_to_string(&version_file)?.trim().to_string();
            return Ok(Some((v, VersionSource::Local(version_file))));
        }
        current_dir = dir.parent().map(|p| p.to_path_buf());
    }

    let global_version_file = svm_dir.join("version");
    if global_version_file.exists() {
        let v = fs::read_to_string(global_version_file)?.trim().to_string();
        Ok(Some((v, VersionSource::Global)))
    } else {
        Ok(None)
    }
}

fn run_shim(binary_name: &str, args: &[String]) -> Result<()> {
    let svm_dir = dirs::home_dir().context("No home dir")?.join(".svm");
    
    let (version, _) = resolve_version(&svm_dir)?
        .ok_or_else(|| anyhow!(
            "{} SVM is not active. Run '{}' or create a {} file.",
            "error:".red().bold(),
            "svm use <version>".cyan(),
            ".svm-version".cyan()
        ))?;

    let binary_path = svm_dir.join("versions").join(&version).join(binary_name);

    if !binary_path.exists() {
        return Err(anyhow!(
            "{} Binary '{}' not found for version '{}'.\n{} Run '{}' to install it.",
            "error:".red().bold(),
            binary_name.yellow(),
            version.cyan(),
            "help:".blue().bold(),
            format!("svm install {}", version).cyan()
        ));
    }

    let mut child = Command::new(binary_path)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("Failed to spawn command")?;

    let status = child.wait()?;

    if let Some(code) = status.code() {
        std::process::exit(code);
    } else {
        std::process::exit(1);
    }
}

fn unset_version(bin_dir: &Path, svm_dir: &Path) -> Result<()> {
    let mut removed = 0;

    for bin in SVM_BINARIES {
        let bin_path = bin_dir.join(bin);
        if bin_path.exists() || bin_path.is_symlink() {
            fs::remove_file(&bin_path)
                .with_context(|| format!("Failed to remove shim at {:?}", bin_path))?;
            removed += 1;
        }
    }

    let version_file = svm_dir.join("version");
    if version_file.exists() {
        fs::remove_file(&version_file)?;
        println!("{} Removed global version file.", "✔".green());
    }

    if removed > 0 {
        println!("{} SVM deactivated. Shims removed from {:?}", "✔".green(), bin_dir);
    } else {
        println!("{} SVM was not active.", "ℹ".blue());
    }
    Ok(())
}

fn show_version(svm_dir: &Path) -> Result<()> {
    match resolve_version(svm_dir)? {
        Some((v, source)) => {
            print!("{} Current version: {}", "➜".cyan(), v.green().bold());
            match source {
                VersionSource::Global => println!(" ({})", "global".dimmed()),
                VersionSource::Local(path) => {
                    println!("\n  {} set by {}", "↳".dimmed(), path.display().to_string().blue());
                }
            }
        }
        None => println!("{} No version currently in use. Run '{}' to set one.", "ℹ".blue(), "svm use <version>".cyan()),
    }
    Ok(())
}

fn list_remote(network_filter: Option<String>) -> Result<()> {
    let client = reqwest::blocking::Client::builder().user_agent(USER_AGENT).build()?;
    let releases: serde_json::Value = client.get(GITHUB_API).send()?.json()?;

    println!("\n{:<25} | {:<15}", "Tag Name".bold(), "Network".bold());
    println!("{}", "-".repeat(45).dimmed());

    if let Some(arr) = releases.as_array() {
        for release in arr {
            let tag = release["tag_name"].as_str().unwrap_or("");
            let network = if tag.contains("mainnet") { "mainnet".green() }
                          else if tag.contains("testnet") { "testnet".yellow() }
                          else if tag.contains("devnet") { "devnet".blue() }
                          else { "other".dimmed() };

            if let Some(ref filter) = network_filter {
                if !tag.contains(filter) { continue; }
            }
            println!("{:<25} | {:<15}", tag.cyan(), network);
        }
    }
    println!("");
    Ok(())
}

fn install_version(version: &str, versions_dir: &Path) -> Result<()> {
    let full_tag = if version.starts_with('v') && !version.contains("net") { 
        format!("mainnet-{}", version) 
    } else { 
        version.to_string() 
    };
    
    let target_dir = versions_dir.join(&full_tag);
    if target_dir.exists() {
        println!("{} Version {} is already installed.", "ℹ".blue(), full_tag.cyan());
        return Ok(())
    }

    // Hardcoded for now (improvements for multi-platform coming soon)
    let url = format!(
        "https://github.com/MystenLabs/sui/releases/download/{}/sui-{}-macos-x86_64.tgz",
        full_tag, full_tag
    );

    println!("{} Downloading {}...", "⬇".blue(), full_tag.cyan());
    let response = reqwest::blocking::get(url)?;
    if !response.status().is_success() {
        return Err(anyhow!(
            "{} Release not found: {}.\n{} Run '{}' to see available versions.", 
            "error:".red().bold(),
            full_tag.yellow(),
            "help:".blue().bold(),
            "svm remote-list".cyan()
        ));
    }

    let tar_gz = response.bytes()?;
    let tar = flate2::read::GzDecoder::new(&tar_gz[..]);
    let mut archive = tar::Archive::new(tar);

    fs::create_dir_all(&target_dir)?;
    archive.unpack(&target_dir)?;

    if cfg!(target_os = "macos") {
        let _ = Command::new("sh")
            .arg("-c")
            .arg(format!("xattr -d com.apple.quarantine {}/* 2>/dev/null || true", target_dir.display()))
            .status();
    }

    println!("{} Successfully installed {} to {:?}", "✔".green(), full_tag.green().bold(), target_dir);
    Ok(())
}

fn use_version(version: &str, versions_dir: &Path, bin_dir: &Path) -> Result<()> {
    let version_path = versions_dir.join(version);
    if !version_path.exists() {
        return Err(anyhow!(
            "{} Version {} not found.\n{} Run '{}' first.",
            "error:".red().bold(),
            version.yellow(),
            "help:".blue().bold(),
            format!("svm install {}", version).cyan()
        ));
    }

    let current_exe = std::env::current_exe()?;

    for bin in SVM_BINARIES {
        let shim_path = bin_dir.join(bin);
        if shim_path.exists() || shim_path.is_symlink() {
            fs::remove_file(&shim_path)?;
        }
        symlink(&current_exe, &shim_path)?;
    }

    let svm_dir = versions_dir.parent().unwrap();
    fs::write(svm_dir.join("version"), version)?;

    println!("{} Active version set to: {}", "✨".green(), version.green().bold());
    Ok(())
}

fn link_local(name: &str, versions_dir: &Path) -> Result<()> {
    let target_dir = versions_dir.join(name);
    fs::create_dir_all(&target_dir)?;

    let cwd = std::env::current_dir()?;
    let mut linked_any = false;
    for bin in SVM_BINARIES {
        let local_bin = cwd.join(bin);
        if local_bin.exists() {
            fs::copy(&local_bin, target_dir.join(bin))?;
            println!("  {} Linked {}...", "↳".dimmed(), bin.cyan());
            linked_any = true;
        }
    }

    if !linked_any {
        return Err(anyhow!(
            "{} No Sui binaries found in current directory.\nExpected: {:?}",
            "error:".red().bold(),
            SVM_BINARIES
        ));
    }

    println!("{} Local build linked as '{}'", "✔".green(), name.green().bold());
    Ok(())
}

fn list_local(versions_dir: &Path, svm_dir: &Path) -> Result<()> {
    let current = resolve_version(svm_dir)?.map(|(v, _)| v);

    println!("\n{:<2} {:<25}", "", "Installed Versions".bold());
    println!("{}", "-".repeat(30).dimmed());

    let mut entries: Vec<_> = fs::read_dir(versions_dir)?
        .filter_map(|e| e.ok())
        .collect();
    
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();

        if Some(name.clone()) == current {
            println!("{} {:<25} {}", "✔".green(), name.green().bold(), "(active)".dimmed());
        } else {
            println!("  {:<25}", name.dimmed());
        }
    }
    println!("");
    Ok(())
}
