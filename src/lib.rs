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

pub const GITHUB_API: &str = "https://api.github.com/repos/MystenLabs/sui/releases";
const USER_AGENT: &str = "svm-cli";
const SVM_BINARIES: &[&str] = &["sui", "move-analyzer"];

#[derive(Parser)]
#[command(name = "svm")]
#[command(about = "Sui Version Manager", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// List available versions on GitHub (uses fzf if available)
    RemoteList {
        #[arg(short, long)]
        network: Option<String>, // e.g., "mainnet", "testnet"
        /// Max number of pages to fetch (100 releases per page)
        #[arg(short = 'p', long, default_value = "3", value_parser = clap::value_parser!(u32).range(1..))]
        pages: u32,
        /// Print plain table without fzf
        #[arg(long)]
        plain: bool,
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
pub enum VersionSource {
    Global,
    Local(PathBuf),
}

pub fn run() -> Result<()> {
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
        Commands::RemoteList { network, pages, plain } => list_remote(network, &svm_dir, pages, plain)?,
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

                    let custom_funcs = r#"
        _svm_local_versions() {
            local -a versions
            versions=($(ls -1 $HOME/.svm/versions 2>/dev/null))
            _describe 'installed versions' versions
        }
        _svm_remote_versions() {
            local -a versions
            local cache_file="$HOME/.svm/cache/releases.json"
            if [[ -f "$cache_file" ]]; then
                versions=($(cat "$cache_file" | python3 -c "import sys,json;[print(r['tag_name']) for r in json.load(sys.stdin).get('releases',[])]" 2>/dev/null))
            fi
            if [[ ${#versions[@]} -eq 0 ]]; then
                versions=($(svm remote-list --plain 2>/dev/null | tail -n +3 | awk '{print $1}' | grep -v '^$'))
            fi
            _describe 'available versions' versions
        }
        "#;

                    let mut final_script = script.replace("#compdef svm", &format!("#compdef svm\n{}", custom_funcs));

                    // install should complete with remote versions
                    final_script = final_script.replacen("':version:_default'", "':version:_svm_remote_versions'", 1);
                    // use and uninstall should complete with local versions
                    final_script = final_script.replace("':version:_default'", "':version:_svm_local_versions'");
                    // link name gets no special completion (user picks the name)
                    final_script = final_script.replace("':name:_default'", "':name:'");

                    println!("{}", final_script);
                }
                _ => return Err(anyhow!("Unsupported shell: {}", shell)),
            }
        }
    }
    Ok(())
}

fn read_version_file(path: &Path) -> Result<Option<String>> {
    let content = fs::read_to_string(path)?;
    let v = content.lines().next().unwrap_or("").trim().to_string();
    if v.is_empty() {
        Ok(None)
    } else {
        Ok(Some(v))
    }
}

pub fn resolve_version(svm_dir: &Path) -> Result<Option<(String, VersionSource)>> {
    let mut current_dir = std::env::current_dir().ok();
    while let Some(dir) = current_dir {
        let version_file = dir.join(".svm-version");
        if version_file.exists() {
            if let Some(v) = read_version_file(&version_file)? {
                return Ok(Some((v, VersionSource::Local(version_file))));
            }
        }
        current_dir = dir.parent().map(|p| p.to_path_buf());
    }

    let global_version_file = svm_dir.join("version");
    if global_version_file.exists() {
        if let Some(v) = read_version_file(&global_version_file)? {
            return Ok(Some((v, VersionSource::Global)));
        }
    }
    Ok(None)
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

pub fn build_client() -> Result<reqwest::blocking::Client> {
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

pub fn cache_dir(svm_dir: &Path) -> Result<PathBuf> {
    let dir = svm_dir.join("cache");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ReleaseCache {
    pub etag: Option<String>,
    pub pages: u32,
    pub releases: Vec<serde_json::Value>,
}

/// Fetch releases with ETag-based caching and pagination.
fn fetch_releases_cached(svm_dir: &Path, max_pages: u32) -> Result<Vec<serde_json::Value>> {
    let client = build_client()?;
    fetch_releases_impl(&client, GITHUB_API, svm_dir, max_pages)
}

pub fn fetch_releases_impl(
    client: &reqwest::blocking::Client,
    base_url: &str,
    svm_dir: &Path,
    max_pages: u32,
) -> Result<Vec<serde_json::Value>> {
    let cache = cache_dir(svm_dir)?;
    let cache_file = cache.join("releases.json");
    let limit = (max_pages as usize) * 100;

    let cached = fs::read_to_string(&cache_file)
        .ok()
        .and_then(|s| serde_json::from_str::<ReleaseCache>(&s).ok());

    // If cache has enough pages, try ETag validation
    if let Some(ref cached) = cached {
        if cached.pages >= max_pages {
            if let Some(ref etag) = cached.etag {
                let url = format!("{}?per_page=100&page=1", base_url);
                match client.get(&url).header("If-None-Match", etag).send() {
                    Ok(resp) if resp.status() == reqwest::StatusCode::NOT_MODIFIED => {
                        return Ok(cached.releases[..limit.min(cached.releases.len())].to_vec());
                    }
                    Ok(resp) if resp.status().is_success() => {
                        // ETag changed — cache is stale, proceed to full fetch below
                    }
                    _ => {
                        // Network/API error — use stale cache as fallback
                        eprintln!("{} Using cached release list (request failed).", "ℹ".blue());
                        return Ok(cached.releases[..limit.min(cached.releases.len())].to_vec());
                    }
                }
            }
        }
    }

    // Full fetch
    let mut all_releases: Vec<serde_json::Value> = Vec::new();
    let mut new_etag: Option<String> = None;

    for page in 1..=max_pages {
        let url = format!("{}?per_page=100&page={}", base_url, page);
        match client.get(&url).send() {
            Ok(resp) => {
                if !resp.status().is_success() {
                    if page == 1 {
                        if let Some(cached) = cached {
                            eprintln!("{} Using cached release list (API returned {}).", "ℹ".blue(), resp.status());
                            let r = &cached.releases;
                            return Ok(r[..limit.min(r.len())].to_vec());
                        }
                    }
                    return Err(anyhow!("GitHub API error: {}", resp.status()));
                }

                if page == 1 {
                    new_etag = resp.headers()
                        .get("etag")
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_string());
                }

                let page_releases: Vec<serde_json::Value> = resp.json()?;
                let is_last = page_releases.len() < 100;
                all_releases.extend(page_releases);

                if is_last {
                    break;
                }
            }
            Err(e) => {
                if page == 1 {
                    if let Some(cached) = cached {
                        eprintln!("{} Using cached release list (network error: {}).", "ℹ".blue(), e);
                        let r = &cached.releases;
                        return Ok(r[..limit.min(r.len())].to_vec());
                    }
                }
                return Err(e.into());
            }
        }
    }

    // Only update cache if we got results
    if !all_releases.is_empty() {
        let new_cache = ReleaseCache {
            etag: new_etag,
            pages: max_pages,
            releases: all_releases.clone(),
        };
        let _ = fs::write(&cache_file, serde_json::to_string(&new_cache)?);
    }

    Ok(all_releases)
}

// Gruvbox Material Dark truecolor palette
const C_GREEN: &str = "\x1b[38;2;169;182;101m";   // #a9b665
const C_YELLOW: &str = "\x1b[38;2;216;166;87m";    // #d8a657
const C_BLUE: &str = "\x1b[38;2;125;174;163m";     // #7daea3
const C_AQUA: &str = "\x1b[38;2;137;180;130m";     // #89b482
#[allow(dead_code)]
const C_ORANGE: &str = "\x1b[38;2;231;138;78m";    // #e78a4e
const C_FG: &str = "\x1b[38;2;221;199;161m";       // #ddc7a1
const C_DIM: &str = "\x1b[38;2;141;135;125m";      // #8d877d
const C_BORDER: &str = "\x1b[38;2;80;73;69m";      // #504945
const C_RESET: &str = "\x1b[0m";
const C_BOLD: &str = "\x1b[1m";

fn network_label(tag: &str) -> &'static str {
    if tag.contains("mainnet") { "mainnet" }
    else if tag.contains("testnet") { "testnet" }
    else if tag.contains("devnet") { "devnet" }
    else { "other" }
}

fn network_color(net: &str) -> &'static str {
    match net {
        "mainnet" => C_GREEN,
        "testnet" => C_YELLOW,
        "devnet"  => C_BLUE,
        _         => C_DIM,
    }
}

fn extract_version(tag: &str) -> &str {
    tag.split_once('-').map(|(_, v)| v).unwrap_or(tag)
}

fn format_release_line(tag: &str, installed: &[String]) -> String {
    let net = network_label(tag);
    let nc = network_color(net);
    let ver = extract_version(tag);
    let mark = if installed.contains(&tag.to_string()) {
        format!(" {C_GREEN}✔{C_RESET}")
    } else {
        String::new()
    };
    // Pad plain text first, then wrap with color
    format!(
        " {nc}●{C_RESET}  {C_AQUA}{C_BOLD}{ver:<12}{C_RESET}  {C_BORDER}│{C_RESET}  {nc}{net:>7}{C_RESET}  {C_BORDER}│{C_RESET}  {C_DIM}{tag}{C_RESET}{mark}"
    )
}

fn list_remote(network_filter: Option<String>, svm_dir: &Path, max_pages: u32, plain: bool) -> Result<()> {
    let releases = fetch_releases_cached(svm_dir, max_pages)?;
    let versions_dir = svm_dir.join("versions");

    let installed: Vec<String> = if versions_dir.exists() {
        fs::read_dir(&versions_dir)
            .ok()
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    } else {
        vec![]
    };

    let mut tags: Vec<&str> = Vec::new();
    for release in &releases {
        let tag = release["tag_name"].as_str().unwrap_or("");
        if tag.is_empty() { continue; }
        if let Some(ref filter) = network_filter {
            if !tag.contains(filter) { continue; }
        }
        tags.push(tag);
    }

    if plain || !atty::is(atty::Stream::Stdout) {
        // Plain output — pad before coloring to keep alignment
        println!(
            "\n    {C_FG}{C_BOLD}{:<12}{C_RESET}  {C_BORDER}│{C_RESET}  {C_FG}{C_BOLD}{:>7}{C_RESET}  {C_BORDER}│{C_RESET}  {C_FG}{C_BOLD}{}{C_RESET}",
            "Version", "Network", "Tag",
        );
        println!("    {C_BORDER}{}{C_RESET}", "─".repeat(48));
        for tag in &tags {
            let net = network_label(tag);
            let nc = network_color(net);
            let ver = extract_version(tag);
            let mark = if installed.contains(&tag.to_string()) {
                format!(" {C_GREEN}✔{C_RESET}")
            } else {
                String::new()
            };
            println!(
                " {nc}●{C_RESET}  {C_AQUA}{C_BOLD}{ver:<12}{C_RESET}  {C_BORDER}│{C_RESET}  {nc}{net:>7}{C_RESET}  {C_BORDER}│{C_RESET}  {C_DIM}{tag}{C_RESET}{mark}"
            );
        }
        println!();
    } else {
        let header = format!(
            "    {C_FG}{C_BOLD}{:<12}{C_RESET}  {C_BORDER}│{C_RESET}  {C_FG}{C_BOLD}{:>7}{C_RESET}  {C_BORDER}│{C_RESET}  {C_FG}{C_BOLD}{}{C_RESET}",
            "Version", "Network", "Tag"
        );
        let lines: Vec<String> = tags.iter().map(|tag| format_release_line(tag, &installed)).collect();
        let input = lines.join("\n");

        let mut child = Command::new("fzf")
            .args([
                "--ansi",
                "--reverse",
                "--header", &header,
                "--prompt", "  Filter > ",
                "--pointer", "▶",
                "--marker", "●",
                "--color", "header:bold,prompt:#89b482,pointer:#89b482,marker:#a9b665,hl:#e78a4e,hl+:#e78a4e",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("fzf is not installed. Use --plain or install fzf.")?;

        if let Some(ref mut stdin) = child.stdin {
            use std::io::Write;
            let _ = stdin.write_all(input.as_bytes());
        }
        let output = child.wait_with_output()?;

        if output.status.success() {
            let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if let Some(tag) = tags.iter().find(|t| selected.contains(**t)) {
                println!("{}", tag);
            }
        }
    }
    Ok(())
}

/// Normalize a user-provided version string into a full release tag.
/// Bare version like "v1.63.4" gets "mainnet-" prefix.
/// Tags already containing a network prefix are left unchanged.
pub fn normalize_install_tag(version: &str) -> String {
    if version.starts_with('v') && !version.contains("net") {
        format!("mainnet-{}", version)
    } else {
        version.to_string()
    }
}

fn install_version(version: &str, versions_dir: &Path, svm_dir: &Path) -> Result<()> {
    let full_tag = normalize_install_tag(version);

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

        let _ = fs::write(&cached_archive, &bytes);

        bytes
    };

    // MD5 verification
    let md5_url = format!("{}.md5", url);
    if let Ok(md5_resp) = build_client()?.get(&md5_url).send() {
        if md5_resp.status().is_success() {
            if let Ok(expected) = md5_resp.text() {
                let expected = expected.trim().split_whitespace().next().unwrap_or("").to_lowercase();
                let actual = format!("{:x}", md5::compute(&archive_bytes));
                if !expected.is_empty() && actual != expected {
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

/// Resolve a version name to an existing directory under versions_dir.
/// Tries the exact name first (e.g. a linked build named "v-custom"),
/// then falls back to the normalized tag (e.g. v1.63.4 → mainnet-v1.63.4).
pub fn resolve_installed_version(version: &str, versions_dir: &Path) -> Option<(String, PathBuf)> {
    let raw_path = versions_dir.join(version);
    if raw_path.exists() {
        return Some((version.to_string(), raw_path));
    }
    // Fall back to normalized name if different from raw
    let normalized = normalize_install_tag(version);
    if normalized != version {
        let normalized_path = versions_dir.join(&normalized);
        if normalized_path.exists() {
            return Some((normalized, normalized_path));
        }
    }
    None
}

fn uninstall_version(version: &str, versions_dir: &Path) -> Result<()> {
    let (resolved, target_dir) = resolve_installed_version(version, versions_dir)
        .ok_or_else(|| anyhow!(
            "{} Version {} is not installed.",
            "error:".red().bold(),
            version.yellow()
        ))?;

    fs::remove_dir_all(&target_dir)?;
    println!("{} Successfully uninstalled {}.", "✔".green(), resolved.red().bold());
    Ok(())
}

fn use_version(version: &str, versions_dir: &Path, bin_dir: &Path, local: bool) -> Result<()> {
    let (resolved, _) = resolve_installed_version(version, versions_dir)
        .ok_or_else(|| anyhow!(
            "{} Version {} not found.\n{} Run '{}' first.",
            "error:".red().bold(),
            version.yellow(),
            "help:".blue().bold(),
            format!("svm install {}", version).cyan()
        ))?;

    if local {
        let cwd = std::env::current_dir()?;
        let version_file = cwd.join(".svm-version");
        fs::write(&version_file, format!("{}\n", resolved))?;
        println!("{} Set local version to {} in {}", "✔".green(), resolved.green().bold(), version_file.display().to_string().dimmed());
    } else {
        let svm_dir = versions_dir.parent().unwrap();
        fs::write(svm_dir.join("version"), &resolved)?;
        println!("{} Active version set to: {}", "✨".green(), resolved.green().bold());
    }

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
