use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

const GITHUB_API: &str = "https://api.github.com/repos/MystenLabs/sui/releases";
const USER_AGENT: &str = "svm-cli";

#[derive(Parser)]
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
}

fn main() -> Result<()> {
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
        Commands::List => list_local(&versions_dir, &bin_dir)?,
        Commands::Show => {
            if let Some(v) = get_current_version(&bin_dir)? {
                println!("Current version: {}", v);
            } else {
                println!("No version currently in use. Use 'svm use <version>'.");
            }
        }
    }
    Ok(())
}

/// Helper to find which version the symlink is pointing to
fn get_current_version(bin_dir: &Path) -> Result<Option<String>> {
    let sui_bin = bin_dir.join("sui");
    if !sui_bin.exists() {
        return Ok(None);
    }

    // read_link returns the path the symlink points to
    let target = fs::read_link(sui_bin)?;
    
    // The target path looks like: /Users/name/.svm/versions/mainnet-v1.24.1/sui
    // We want the parent folder name (mainnet-v1.24.1)
    if let Some(parent) = target.parent() {
        if let Some(name) = parent.file_name() {
            return Ok(Some(name.to_string_lossy().into_owned()));
        }
    }
    Ok(None)
}

fn list_remote(network_filter: Option<String>) -> Result<()> {
    let client = reqwest::blocking::Client::builder().user_agent(USER_AGENT).build()?;
    let releases: serde_json::Value = client.get(GITHUB_API).send()?.json()?;

    println!("{:<25} | {:<15}", "Tag Name", "Network");
    println!("{}", "-".repeat(45));

    if let Some(arr) = releases.as_array() {
        for release in arr {
            let tag = release["tag_name"].as_str().unwrap_or("");
            let network = if tag.contains("mainnet") { "mainnet" }
                          else if tag.contains("testnet") { "testnet" }
                          else if tag.contains("devnet") { "devnet" }
                          else { "other" };

            if let Some(ref filter) = network_filter {
                if network != filter { continue; }
            }
            println!("{:<25} | {:<15}", tag, network);
        }
    }
    Ok(())
}

fn install_version(version: &str, versions_dir: &Path) -> Result<()> {
    // Standardize the version name for the URL
    // If user provides "v1.63.4", we check if they meant "mainnet-v1.63.4"
    // For simplicity, we'll assume they provide the full tag or we default to mainnet
    let full_tag = if version.starts_with('v') { format!("mainnet-{}", version) } else { version.to_string() };
    
    let target_dir = versions_dir.join(&full_tag);
    if target_dir.exists() {
        println!("Version {} already exists.", full_tag);
        return Ok(());
    }

    // URL Pattern for macos-x86_64
    let url = format!(
        "https://github.com/MystenLabs/sui/releases/download/{}/sui-{}-macos-x86_64.tgz",
        full_tag, full_tag
    );

    println!("Downloading from: {}", url);
    let response = reqwest::blocking::get(url)?;
    if !response.status().is_success() {
        return Err(anyhow!("Release not found. Try 'svm remote-list' to find exact tag."));
    }

    let tar_gz = response.bytes()?;
    let tar = flate2::read::GzDecoder::new(&tar_gz[..]);
    let mut archive = tar::Archive::new(tar);

    fs::create_dir_all(&target_dir)?;
    archive.unpack(&target_dir)?;

    // macOS binaries: clear the quarantine attribute only if it exists
    if cfg!(target_os = "macos") {
    // We use "|| true" to ensure the command doesn't return an error 
    // if the attribute is already missing.
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("xattr -d com.apple.quarantine {}/* 2>/dev/null || true", target_dir.display()))
        .status();
    }

    println!("Installed {} to {:?}", full_tag, target_dir);
    Ok(())
}

fn use_version(version: &str, versions_dir: &Path, bin_dir: &Path) -> Result<()> {
    let version_path = versions_dir.join(version);
    if !version_path.exists() {
        return Err(anyhow!("Version {} not found. Install it first.", version));
    }

    // Sui releases often put binaries in a subfolder or directly in root
    // We'll search for 'sui' and 'move-analyzer' within the version_path
    let binaries = ["sui", "move-analyzer"];
    for bin in binaries {
        let bin_src = version_path.join(bin);
        let bin_dest = bin_dir.join(bin);

        if bin_dest.exists() || bin_dest.is_symlink() {
            fs::remove_file(&bin_dest)?;
        }

        if bin_src.exists() {
            symlink(&bin_src, &bin_dest)?;
        } else {
            println!("Warning: Binary {} not found in version folder.", bin);
        }
    }

    println!("Active version set to: {}", version);
    Ok(())
}

fn link_local(name: &str, versions_dir: &Path) -> Result<()> {
    let target_dir = versions_dir.join(name);
    fs::create_dir_all(&target_dir)?;

    let cwd = std::env::current_dir()?;
    for bin in ["sui", "move-analyzer"] {
        let local_bin = cwd.join(bin);
        if local_bin.exists() {
            fs::copy(&local_bin, target_dir.join(bin))?;
            println!("Linked {}...", bin);
        }
    }
    println!("Local build linked as '{}'", name);
    Ok(())
}

fn list_local(versions_dir: &Path, bin_dir: &Path) -> Result<()> {
    let current = get_current_version(bin_dir)?;

    println!("{:<2} {:<25}", "", "Installed Versions");
    println!("{}", "-".repeat(30));

    for entry in fs::read_dir(versions_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();

        if Some(name.clone()) == current {
            println!("* {:<25} (active)", name);
        } else {
            println!("  {:<25}", name);
        }
    }
    Ok(())
}
