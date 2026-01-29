use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser)]
#[command(name = "svm", about = "Sui Version Manager")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Install a version from GitHub (e.g., v1.18.0)
    Install { version: String },
    /// Use a specific version (sets symlinks)
    Use { version: String },
    /// List all installed/linked versions
    List,
    /// Link a local build to svm (e.g., svm link my-local-build)
    /// Expects 'sui' and 'move-analyzer' to be in the current directory.
    Link { name: String },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let svm_dir = dirs::home_dir().unwrap().join(".svm");
    let versions_dir = svm_dir.join("versions");
    let bin_dir = svm_dir.join("bin");

    // Initialize directory structure
    fs::create_dir_all(&versions_dir)?;
    fs::create_dir_all(&bin_dir)?;

    match cli.command {
        Commands::Install { version } => install_version(&version, &versions_dir)?,
        Commands::Use { version } => use_version(&version, &versions_dir, &bin_dir)?,
        Commands::List => list_versions(&versions_dir)?,
        Commands::Link { name } => link_local(&name, &versions_dir)?,
    }

    Ok(())
}

fn install_version(version: &str, versions_dir: &Path) -> Result<()> {
    let target_dir = versions_dir.join(version);
    if target_dir.exists() {
        println!("Version {} is already installed.", version);
        return Ok(());
    }

    // Example URL for Linux x86_64; in a real tool, detect OS/Arch dynamically
    let url = format!(
        "https://github.com/MystenLabs/sui/releases/download/{}/sui-{}-ubuntu-x86_64.tgz",
        version, version
    );

    println!("Downloading {}...", url);
    let response = reqwest::blocking::get(url)?;
    if !response.status().is_success() {
        return Err(anyhow!("Failed to download version: {}", response.status()));
    }

    let tar_gz = response.bytes()?;
    let tar = flate2::read::GzDecoder::new(&tar_gz[..]);
    let mut archive = tar::Archive::new(tar);

    fs::create_dir_all(&target_dir)?;
    archive.unpack(&target_dir)?;

    println!("Successfully installed version {} to {:?}", version, target_dir);
    Ok(())
}

fn use_version(version: &str, versions_dir: &Path, bin_dir: &Path) -> Result<()> {
    let version_path = versions_dir.join(version);
    if !version_path.exists() {
        return Err(anyhow!("Version {} not found locally.", version));
    }

    for binary in ["sui", "move-analyzer"] {
        let src = version_path.join(binary);
        let dest = bin_dir.join(binary);

        if dest.exists() || dest.is_symlink() {
            fs::remove_file(&dest)?;
        }
        symlink(src, dest)?;
    }

    println!("Now using version: {}", version);
    Ok(())
}

fn list_versions(versions_dir: &Path) -> Result<()> {
    println!("Installed versions:");
    for entry in fs::read_dir(versions_dir)? {
        let entry = entry?;
        if let Some(name) = entry.file_name().to_str() {
            println!("  - {}", name);
        }
    }
    Ok(())
}

fn link_local(name: &str, versions_dir: &Path) -> Result<()> {
    let target_dir = versions_dir.join(name);
    fs::create_dir_all(&target_dir)?;

    let current_dir = std::env::current_dir()?;
    for binary in ["sui", "move-analyzer"] {
        let binary_path = current_dir.join(binary);
        if !binary_path.exists() {
            return Err(anyhow!("Binary '{}' not found in current directory.", binary));
        }
        // Copy the local build into the version manager
        fs::copy(&binary_path, target_dir.join(binary))?;
    }

    println!("Linked local build as '{}'", name);
    Ok(())
}
