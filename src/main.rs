use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand, CommandFactory};
use colored::*;
use std::fs;
use std::io::Read;
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use clap_complete::{generate, shells::Zsh};
use indicatif::{ProgressBar, ProgressStyle};

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
        /// Max number of pages to fetch (100 releases per page)
        #[arg(short = 'p', long, default_value = "3")]
        pages: u32,
    },
    /// Install a version (e.g., v1.63.4 or mainnet-v1.63.4)
    Install { version: String },
    /// Uninstall a version
    Uninstall { version: String },
    /// Switch to an installed/linked version
    Use {
        version: String,
        /// Write .svm-version in current directory instead of setting globally
        #[arg(short, long)]
        local: bool,
    },
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
        Commands::RemoteList { network, pages } => list_remote(network, &svm_dir, pages)?,
        Commands::Install { version } => install_version(&version, &versions_dir, &svm_dir)?,
        Commands::Uninstall { version } => uninstall_version(&version, &versions_dir)?,
        Commands::Use { version, local } => use_version(&version, &versions_dir, &bin_dir, local)?,
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

    let err = Command::new(binary_path)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .exec();

    Err(anyhow!("Failed to execute binary: {}", err))
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

// --- GitHub API helpers ---

fn build_client() -> Result<reqwest::blocking::Client> {
    let mut builder = reqwest::blocking::Client::builder().user_agent(USER_AGENT);
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        use reqwest::header;
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {}", token))?,
        );
        builder = builder.default_headers(headers);
    }
    Ok(builder.build()?)
}

fn cache_dir(svm_dir: &Path) -> Result<PathBuf> {
    let dir = svm_dir.join("cache");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Fetch releases with ETag-based caching and pagination.
fn fetch_releases_cached(svm_dir: &Path, max_pages: u32) -> Result<Vec<serde_json::Value>> {
    let cache = cache_dir(svm_dir)?;
    let cache_file = cache.join("releases.json");
    let etag_file = cache.join("releases.etag");

    let client = build_client()?;

    let mut all_releases: Vec<serde_json::Value> = Vec::new();

    for page in 1..=max_pages {
        let url = format!("{}?per_page=100&page={}", GITHUB_API, page);
        let mut request = client.get(&url);

        // Use ETag only for the first page to detect staleness
        if page == 1 {
            if let Ok(etag) = fs::read_to_string(&etag_file) {
                request = request.header("If-None-Match", etag.trim());
            }
        }

        let response = request.send();

        match response {
            Ok(resp) => {
                if page == 1 && resp.status() == reqwest::StatusCode::NOT_MODIFIED {
                    // Cache is still fresh, use it
                    if let Ok(cached) = fs::read_to_string(&cache_file) {
                        if let Ok(parsed) = serde_json::from_str(&cached) {
                            return Ok(parsed);
                        }
                    }
                }

                if !resp.status().is_success() {
                    // If first page fails and we have cache, fall back
                    if page == 1 {
                        if let Ok(cached) = fs::read_to_string(&cache_file) {
                            if let Ok(parsed) = serde_json::from_str(&cached) {
                                eprintln!("{} Using cached release list (API returned {}).", "ℹ".blue(), resp.status());
                                return Ok(parsed);
                            }
                        }
                    }
                    return Err(anyhow!("GitHub API error: {}", resp.status()));
                }

                // Save ETag from first page
                if page == 1 {
                    if let Some(etag) = resp.headers().get("etag") {
                        let _ = fs::write(&etag_file, etag.as_bytes());
                    }
                }

                let page_releases: Vec<serde_json::Value> = resp.json()?;
                let is_last = page_releases.len() < 100;
                all_releases.extend(page_releases);

                if is_last {
                    break;
                }
            }
            Err(e) => {
                // Network error — fall back to cache if available
                if page == 1 {
                    if let Ok(cached) = fs::read_to_string(&cache_file) {
                        if let Ok(parsed) = serde_json::from_str(&cached) {
                            eprintln!("{} Using cached release list (network error: {}).", "ℹ".blue(), e);
                            return Ok(parsed);
                        }
                    }
                }
                return Err(e.into());
            }
        }
    }

    // Update cache
    let _ = fs::write(&cache_file, serde_json::to_string(&all_releases)?);

    Ok(all_releases)
}

fn list_remote(network_filter: Option<String>, svm_dir: &Path, max_pages: u32) -> Result<()> {
    let releases = fetch_releases_cached(svm_dir, max_pages)?;

    println!("\n{:<25} | {:<15}", "Tag Name".bold(), "Network".bold());
    println!("{}", "-".repeat(45).dimmed());

    for release in &releases {
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
    println!("");
    Ok(())
}

fn install_version(version: &str, versions_dir: &Path, svm_dir: &Path) -> Result<()> {
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

    let os_part = match std::env::consts::OS {
        "macos" => "macos",
        "linux" => "ubuntu",
        "windows" => "windows",
        os => return Err(anyhow!("Unsupported OS: {}", os)),
    };

    let arch_part = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "arm64",
        arch => return Err(anyhow!("Unsupported architecture: {}", arch)),
    };

    // Windows support is limited as we rely on .tgz and shim execution which might need tweaks
    if std::env::consts::OS == "windows" {
         println!("{} Warning: Windows support is experimental.", "⚠".yellow());
    }

    let asset_name = format!("sui-{}-{}-{}.tgz", full_tag, os_part, arch_part);
    let url = format!(
        "https://github.com/MystenLabs/sui/releases/download/{}/{}",
        full_tag, asset_name
    );

    let cache = cache_dir(svm_dir)?;
    let cached_archive = cache.join(&asset_name);

    // Check download cache
    let archive_bytes = if cached_archive.exists() {
        println!("{} Using cached archive for {}...", "ℹ".blue(), full_tag.cyan());
        fs::read(&cached_archive)?
    } else {
        println!("{} Downloading {}...", "⬇".blue(), full_tag.cyan());

        let client = build_client()?;
        let response = client.get(&url).send()?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "{} Release not found: {}.\n{} Run '{}' to see available versions.",
                "error:".red().bold(),
                full_tag.yellow(),
                "help:".blue().bold(),
                "svm remote-list".cyan()
            ));
        }

        let total_size = response.content_length().unwrap_or(0);
        let pb = ProgressBar::new(total_size);
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("#>-"));

        let mut bytes = Vec::new();
        pb.wrap_read(response).read_to_end(&mut bytes)?;
        pb.finish_with_message("Download complete");

        // Cache the downloaded archive
        let _ = fs::write(&cached_archive, &bytes);

        bytes
    };

    // MD5 verification — try to fetch .md5 sidecar
    let md5_url = format!("{}.md5", url);
    if let Ok(md5_resp) = build_client()?.get(&md5_url).send() {
        if md5_resp.status().is_success() {
            if let Ok(expected) = md5_resp.text() {
                let expected = expected.trim().split_whitespace().next().unwrap_or("").to_lowercase();
                let actual = format!("{:x}", md5::compute(&archive_bytes));
                if !expected.is_empty() && actual != expected {
                    // Remove corrupted cached archive
                    let _ = fs::remove_file(&cached_archive);
                    return Err(anyhow!(
                        "{} MD5 verification failed.\n  Expected: {}\n  Got:      {}\n{} The archive may be corrupted. Please retry.",
                        "error:".red().bold(),
                        expected.yellow(),
                        actual.yellow(),
                        "help:".blue().bold(),
                    ));
                }
                println!("{} MD5 checksum verified.", "✔".green());
            }
        }
    }

    // Extract
    let tar = flate2::read::GzDecoder::new(archive_bytes.as_slice());
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

fn uninstall_version(version: &str, versions_dir: &Path) -> Result<()> {
    let target_dir = versions_dir.join(version);
    if !target_dir.exists() {
        return Err(anyhow!(
            "{} Version {} is not installed.",
            "error:".red().bold(),
            version.yellow()
        ));
    }

    fs::remove_dir_all(&target_dir)?;
    println!("{} Successfully uninstalled {}.", "✔".green(), version.red().bold());
    Ok(())
}

fn use_version(version: &str, versions_dir: &Path, bin_dir: &Path, local: bool) -> Result<()> {
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

    if local {
        let cwd = std::env::current_dir()?;
        let version_file = cwd.join(".svm-version");
        fs::write(&version_file, format!("{}\n", version))?;
        println!("{} Set local version to {} in {}", "✔".green(), version.green().bold(), version_file.display().to_string().dimmed());
    } else {
        let svm_dir = versions_dir.parent().unwrap();
        fs::write(svm_dir.join("version"), version)?;
        println!("{} Active version set to: {}", "✨".green(), version.green().bold());
    }

    // Ensure shims exist regardless of local/global
    let current_exe = std::env::current_exe()?;
    for bin in SVM_BINARIES {
        let shim_path = bin_dir.join(bin);
        if shim_path.exists() || shim_path.is_symlink() {
            fs::remove_file(&shim_path)?;
        }
        symlink(&current_exe, &shim_path)?;
    }

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

        if current.as_deref() == Some(&name) {
            println!("{} {:<25} {}", "✔".green(), name.green().bold(), "(active)".dimmed());
        } else {
            println!("  {:<25}", name.dimmed());
        }
    }
    println!("");
    Ok(())
}
