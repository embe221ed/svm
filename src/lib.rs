use anyhow::{anyhow, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use colored::*;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::IsTerminal;
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const GITHUB_API: &str = "https://api.github.com/repos/MystenLabs/sui/releases";
const DOWNLOAD_BASE: &str = "https://github.com/MystenLabs/sui/releases/download";

/// Release-metadata API base. SVM_API_BASE overrides it (mirrors, tests).
fn api_base() -> String {
    env_override("SVM_API_BASE").unwrap_or_else(|| GITHUB_API.to_string())
}

/// Release-archive download base. SVM_DOWNLOAD_BASE overrides it.
fn download_base() -> String {
    env_override("SVM_DOWNLOAD_BASE").unwrap_or_else(|| DOWNLOAD_BASE.to_string())
}

fn env_override(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
}
const USER_AGENT: &str = "svm-cli";
const SVM_BINARIES: &[&str] = &["sui", "move-analyzer"];
/// Marker file written into a linked build's directory recording where each
/// binary was copied from ("<binary> <source path>" per line).
const LINK_MARKER: &str = ".svm-link";
pub const NETWORKS: &[&str] = &["mainnet", "testnet", "devnet"];

#[derive(Parser)]
#[command(name = "svm", version)]
#[command(about = "Sui Version Manager", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// List available versions on GitHub (uses fzf if available)
    RemoteList {
        /// Filter by network (mainnet, testnet, devnet)
        #[arg(short, long)]
        network: Option<String>,
        /// Max number of pages to fetch (100 releases per page)
        #[arg(short = 'p', long, default_value = "3", value_parser = clap::value_parser!(u32).range(1..))]
        pages: u32,
        /// Print plain table without fzf
        #[arg(long)]
        plain: bool,
        /// Print bare tags, one per line (for scripting; implies --plain)
        #[arg(long)]
        tags_only: bool,
        /// Use the local release cache only, never hit the network
        #[arg(long)]
        cached: bool,
    },
    /// Install a version (e.g. v1.63.4, testnet-v1.63.4, v1.63, latest, testnet)
    Install {
        version: String,
        /// Switch to the version after installing
        #[arg(short = 'u', long = "use")]
        use_after: bool,
    },
    /// Update the active version to the latest release on its network
    Update,
    /// Uninstall a version
    Uninstall { version: String },
    /// Switch to an installed/linked version
    Use {
        version: String,
        /// Write .svm-version in current directory instead of setting globally
        #[arg(short, long)]
        local: bool,
    },
    /// Run a command under a specific installed version without switching
    Exec {
        version: String,
        /// Command and arguments (e.g. sui move test)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// Link a local build (e.g. svm link custom-dev)
    Link {
        name: String,
        /// Directory containing the built binaries (default: ., ./target/release, ./target/debug)
        #[arg(value_hint = clap::ValueHint::DirPath)]
        path: Option<PathBuf>,
    },
    /// List local versions (active one marked with *)
    List,
    /// Show the currently active version
    Show,
    /// Print the full path of the binary the active version resolves to
    Which {
        #[arg(default_value = "sui")]
        binary: String,
    },
    /// Deactivate svm by removing shims
    Unset,
    /// Inspect or clean the download cache
    Cache {
        #[command(subcommand)]
        action: Option<CacheAction>,
    },
    /// Generate shell completions
    Completions { shell: Shell },
}

#[derive(Subcommand)]
pub enum CacheAction {
    /// Remove downloaded release archives
    Clean {
        /// Also remove the cached release list
        #[arg(long)]
        all: bool,
    },
}

#[derive(Debug, PartialEq)]
pub enum VersionSource {
    Global,
    Local(PathBuf),
}

/// Print an error for the user. Our own messages already carry a styled
/// "error:" prefix; add one only for raw errors that bubbled up without it.
/// Checked as a prefix (after any ANSI styling) so incidental "error:"
/// substrings — e.g. reqwest's "tcp connect error:" — don't suppress it.
pub fn report_error(err: &anyhow::Error) {
    let msg = format!("{:#}", err);
    if strip_leading_ansi(&msg).starts_with("error:") {
        eprintln!("{}", msg);
    } else {
        eprintln!("{} {}", "error:".red().bold(), msg);
    }
}

fn strip_leading_ansi(s: &str) -> &str {
    let mut rest = s;
    while let Some(after) = rest.strip_prefix('\x1b') {
        match after.find('m') {
            Some(i) => rest = &after[i + 1..],
            None => break,
        }
    }
    rest
}

/// A version/link name must stay a single path component under
/// ~/.svm/versions. Anything else (absolute paths, "..", hidden names)
/// could escape the directory — `Path::join` replaces the base entirely
/// when handed an absolute path, and a hostile `.svm-version` file in a
/// cloned repo would otherwise let the shim execute an arbitrary binary.
pub fn valid_version_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains('/') && !name.contains('\\')
}

pub fn run() -> Result<()> {
    // 1. Shim Detection
    let args: Vec<String> = std::env::args().collect();
    if let Some(program_name) = args.first().and_then(|p| Path::new(p).file_name()) {
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
        Commands::RemoteList { network, pages, plain, tags_only, cached } => {
            list_remote(network, &svm_dir, pages, plain, tags_only, cached)?
        }
        Commands::Install { version, use_after } => {
            install_version(&version, &versions_dir, &svm_dir, use_after, true)?;
        }
        Commands::Update => update_version(&versions_dir, &svm_dir)?,
        Commands::Uninstall { version } => uninstall_version(&version, &versions_dir, &svm_dir)?,
        Commands::Use { version, local } => use_version(&version, &versions_dir, &bin_dir, local)?,
        Commands::Exec { version, command } => exec_command(&version, &command, &versions_dir)?,
        Commands::Which { binary } => which_command(&binary, &svm_dir)?,
        Commands::Link { name, path } => link_local(&name, path.as_deref(), &versions_dir)?,
        Commands::List => list_local(&versions_dir, &svm_dir)?,
        Commands::Show => show_version(&svm_dir)?,
        Commands::Unset => unset_version(&bin_dir, &svm_dir)?,
        Commands::Cache { action } => cache_command(&svm_dir, action)?,
        Commands::Completions { shell } => print_completions(shell),
    }
    Ok(())
}

// --- Version file resolution ---

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
        if version_file.exists()
            && let Some(v) = read_version_file(&version_file)?
        {
            return Ok(Some((v, VersionSource::Local(version_file))));
        }
        current_dir = dir.parent().map(|p| p.to_path_buf());
    }

    let global_version_file = svm_dir.join("version");
    if global_version_file.exists()
        && let Some(v) = read_version_file(&global_version_file)?
    {
        return Ok(Some((v, VersionSource::Global)));
    }
    Ok(None)
}

fn err_not_active() -> anyhow::Error {
    anyhow!(
        "{} SVM is not active. Run '{}' or create a {} file.",
        "error:".red().bold(),
        "svm use <version>".cyan(),
        ".svm-version".cyan()
    )
}

/// Replace control characters before showing an untrusted string (a hostile
/// .svm-version could otherwise inject terminal escape sequences into
/// prompts and error messages).
fn sanitize_display(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect()
}

fn err_not_installed(version: &str) -> anyhow::Error {
    let version = sanitize_display(version);
    anyhow!(
        "{} Version '{}' is set but not installed.\n{} Run '{}' to install it.",
        "error:".red().bold(),
        version.yellow(),
        "help:".blue().bold(),
        format!("svm install {}", version).cyan()
    )
}

/// Path of `binary_name` inside an installed version's directory, with the
/// shim's diagnostics: a dangling symlink (stale linked build from older svm
/// versions) and a missing binary get distinct errors.
fn locate_binary(version_dir: &Path, resolved: &str, binary_name: &str) -> Result<PathBuf> {
    let binary_path = version_dir.join(binary_name);
    if binary_path.exists() {
        return Ok(binary_path);
    }
    if binary_path.is_symlink() {
        return Err(anyhow!(
            "{} '{}' in version '{}' points to a build that no longer exists.\n{} Rebuild it, then re-run '{}' from your build directory.",
            "error:".red().bold(),
            binary_name.yellow(),
            resolved.cyan(),
            "help:".blue().bold(),
            format!("svm link {}", resolved).cyan()
        ));
    }
    Err(anyhow!(
        "{} Binary '{}' not found for version '{}'.\n{} Run '{}' to install it.",
        "error:".red().bold(),
        binary_name.yellow(),
        resolved.cyan(),
        "help:".blue().bold(),
        format!("svm install {}", resolved).cyan()
    ))
}

/// Guard against exec'ing ourselves in a loop (e.g. a linked build whose
/// "sui" is actually the svm binary).
fn refuse_self_exec(binary_path: &Path, binary_name: &str, version: &str) -> Result<()> {
    if let (Ok(target), Ok(me)) = (
        binary_path.canonicalize(),
        std::env::current_exe().and_then(|p| p.canonicalize()),
    ) && target == me
    {
        return Err(anyhow!(
            "{} '{}' in version '{}' resolves to svm itself — refusing to exec in a loop.",
            "error:".red().bold(),
            binary_name.yellow(),
            version.cyan()
        ));
    }
    Ok(())
}

fn exec_binary(binary_path: &Path, args: &[String]) -> Result<()> {
    let err = Command::new(binary_path)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .exec();

    Err(anyhow!("Failed to execute binary: {}", err))
}

enum AutoInstall {
    /// SVM_AUTO_INSTALL unset or unrecognized: offer interactively.
    Ask,
    /// SVM_AUTO_INSTALL truthy: install without asking.
    Always,
    /// SVM_AUTO_INSTALL falsy: never install from the shim.
    Never,
}

fn auto_install_mode() -> AutoInstall {
    match std::env::var("SVM_AUTO_INSTALL") {
        Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "y" | "on" => AutoInstall::Always,
            "0" | "false" | "no" | "n" | "off" => AutoInstall::Never,
            _ => AutoInstall::Ask,
        },
        Err(_) => AutoInstall::Ask,
    }
}

/// Install a pinned-but-missing version from the shim path, so a cloned repo
/// with a .svm-version works without a manual `svm install`. Returns None
/// when installation was declined or not permitted; the caller falls back to
/// the plain "not installed" error.
fn shim_auto_install(
    version: &str,
    versions_dir: &Path,
    svm_dir: &Path,
) -> Result<Option<(String, PathBuf)>> {
    let shown = sanitize_display(version);
    match auto_install_mode() {
        AutoInstall::Never => return Ok(None),
        AutoInstall::Always => eprintln!(
            "{} Version {} is not installed — installing it now (SVM_AUTO_INSTALL).",
            "ℹ".blue(),
            shown.cyan()
        ),
        AutoInstall::Ask => {
            let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
            if !interactive
                || !prompt_yes_no(&format!(
                    "Version {} is not installed. Install it now?",
                    shown.cyan()
                ))?
            {
                return Ok(None);
            }
        }
    }
    // Install without activating: the version file that led us here already
    // selects this version, so touching the global default would be wrong.
    let tag = install_version(version, versions_dir, svm_dir, false, false)?;
    Ok(resolve_installed_version(&tag, versions_dir))
}

fn run_shim(binary_name: &str, args: &[String]) -> Result<()> {
    let svm_dir = dirs::home_dir().context("No home dir")?.join(".svm");

    let (version, _) = resolve_version(&svm_dir)?.ok_or_else(err_not_active)?;

    let versions_dir = svm_dir.join("versions");
    let (resolved, version_dir) = match resolve_available_version(&version, &versions_dir) {
        Some(found) => found,
        None => shim_auto_install(&version, &versions_dir, &svm_dir)?
            .ok_or_else(|| err_not_installed(&version))?,
    };

    let binary_path = locate_binary(&version_dir, &resolved, binary_name)?;
    refuse_self_exec(&binary_path, binary_name, &resolved)?;
    exec_binary(&binary_path, args)
}

fn which_command(binary: &str, svm_dir: &Path) -> Result<()> {
    if !valid_version_name(binary) {
        return Err(anyhow!(
            "{} Invalid binary name: {}",
            "error:".red().bold(),
            binary.yellow()
        ));
    }

    let (version, _) = resolve_version(svm_dir)?.ok_or_else(err_not_active)?;

    let versions_dir = svm_dir.join("versions");
    let (resolved, version_dir) = resolve_available_version(&version, &versions_dir)
        .ok_or_else(|| err_not_installed(&version))?;

    let binary_path = locate_binary(&version_dir, &resolved, binary)?;
    println!("{}", binary_path.display());
    Ok(())
}

fn exec_command(version: &str, command: &[String], versions_dir: &Path) -> Result<()> {
    let (resolved, version_dir) = resolve_available_version(version, versions_dir)
        .ok_or_else(|| anyhow!(
            "{} Version '{}' is not installed.\n{} Run '{}' first.",
            "error:".red().bold(),
            version.yellow(),
            "help:".blue().bold(),
            format!("svm install {}", version).cyan()
        ))?;

    let (program, args) = command
        .split_first()
        .context("exec requires a command (enforced by clap)")?;

    // Managed binaries must come from the selected version — a PATH fallback
    // could silently reach the shim and run the *active* version instead.
    let program_path = if SVM_BINARIES.contains(&program.as_str()) {
        locate_binary(&version_dir, &resolved, program)?
    } else {
        let candidate = version_dir.join(program);
        if !program.contains('/') && candidate.exists() {
            candidate
        } else {
            PathBuf::from(program)
        }
    };
    refuse_self_exec(&program_path, program, &resolved)?;

    let mut cmd = Command::new(&program_path);
    cmd.args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    // Put the version's directory first on PATH so the command and any
    // subprocesses it spawns resolve this version's binaries. A directory
    // that can't be joined (a ':' in the path) is not fatal — managed
    // binaries were already resolved to absolute paths above.
    match std::env::var_os("PATH") {
        Some(old) => {
            let mut parts = vec![version_dir.clone()];
            parts.extend(std::env::split_paths(&old));
            match std::env::join_paths(parts) {
                Ok(joined) => {
                    cmd.env("PATH", joined);
                }
                Err(_) => eprintln!(
                    "{} Could not prepend {:?} to PATH (path contains a separator) — running with the unmodified PATH.",
                    "⚠".yellow(),
                    version_dir
                ),
            }
        }
        None => {
            cmd.env("PATH", &version_dir);
        }
    }

    let err = cmd.exec();
    Err(anyhow!("Failed to execute '{}': {}", program_path.display(), err))
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
            let versions_dir = svm_dir.join("versions");
            if resolve_available_version(&v, &versions_dir).is_none() {
                println!(
                    "  {} not installed — run '{}'",
                    "⚠".yellow(),
                    format!("svm install {}", v).cyan()
                );
            }
        }
        None => println!(
            "{} No version currently in use. Run '{}' to set one.",
            "ℹ".blue(),
            "svm use <version>".cyan()
        ),
    }
    Ok(())
}

// --- GitHub API helpers ---

pub fn build_client() -> Result<reqwest::blocking::Client> {
    let mut builder = reqwest::blocking::Client::builder().user_agent(USER_AGENT);
    // Never attach the GitHub token when the endpoints are overridden: it is
    // a default header on every request this client makes, and it must not
    // leak to whatever host SVM_API_BASE/SVM_DOWNLOAD_BASE point at.
    let endpoints_overridden =
        env_override("SVM_API_BASE").is_some() || env_override("SVM_DOWNLOAD_BASE").is_some();
    if !endpoints_overridden
        && let Ok(token) = std::env::var("GITHUB_TOKEN")
        && !token.trim().is_empty()
    {
        use reqwest::header;
        let mut headers = header::HeaderMap::new();
        let mut value = header::HeaderValue::from_str(&format!("Bearer {}", token))?;
        value.set_sensitive(true);
        headers.insert(header::AUTHORIZATION, value);
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

/// Read the cached release list without touching the network.
pub fn load_cached_releases(svm_dir: &Path) -> Option<Vec<serde_json::Value>> {
    let cache_file = svm_dir.join("cache").join("releases.json");
    let content = fs::read_to_string(cache_file).ok()?;
    serde_json::from_str::<ReleaseCache>(&content)
        .ok()
        .map(|c| c.releases)
}

/// Fetch releases with ETag-based caching and pagination.
fn fetch_releases_cached(svm_dir: &Path, max_pages: u32) -> Result<Vec<serde_json::Value>> {
    let client = build_client()?;
    fetch_releases_impl(&client, &api_base(), svm_dir, max_pages)
}

fn header_etag(resp: &reqwest::blocking::Response) -> Option<String> {
    resp.headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
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

    let mut all_releases: Vec<serde_json::Value> = Vec::new();
    let mut new_etag: Option<String> = None;
    let mut start_page = 1u32;
    let mut done = false;

    // If cache has enough pages, try ETag validation
    if let Some(ref cached) = cached
        && cached.pages >= max_pages
        && let Some(ref etag) = cached.etag
    {
        let url = format!("{}?per_page=100&page=1", base_url);
        match client.get(&url).header("If-None-Match", etag.as_str()).send() {
            Ok(resp) if resp.status() == reqwest::StatusCode::NOT_MODIFIED => {
                return Ok(cached.releases[..limit.min(cached.releases.len())].to_vec());
            }
            Ok(resp) if resp.status().is_success() => {
                // ETag changed — cache is stale. Reuse this response as
                // page 1 of the refetch instead of requesting it again.
                new_etag = header_etag(&resp);
                match resp.json::<Vec<serde_json::Value>>() {
                    Ok(page_releases) => {
                        done = page_releases.len() < 100;
                        all_releases.extend(page_releases);
                        start_page = 2;
                    }
                    Err(_) => {
                        // Truncated/garbled body — fall back to the stale cache
                        eprintln!("{} Using cached release list (bad response body).", "ℹ".blue());
                        return Ok(cached.releases[..limit.min(cached.releases.len())].to_vec());
                    }
                }
            }
            _ => {
                // Network/API error — use stale cache as fallback
                eprintln!("{} Using cached release list (request failed).", "ℹ".blue());
                return Ok(cached.releases[..limit.min(cached.releases.len())].to_vec());
            }
        }
    }

    if !done {
        for page in start_page..=max_pages {
            let url = format!("{}?per_page=100&page={}", base_url, page);
            match client.get(&url).send() {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        if page == 1 && let Some(cached) = cached {
                            eprintln!(
                                "{} Using cached release list (API returned {}).",
                                "ℹ".blue(),
                                resp.status()
                            );
                            let r = &cached.releases;
                            return Ok(r[..limit.min(r.len())].to_vec());
                        }
                        return Err(anyhow!("GitHub API error: {}", resp.status()));
                    }

                    if page == 1 {
                        new_etag = header_etag(&resp);
                    }

                    let page_releases: Vec<serde_json::Value> = match resp.json() {
                        Ok(p) => p,
                        Err(e) => {
                            if page == 1 && let Some(cached) = cached {
                                eprintln!(
                                    "{} Using cached release list (bad response body).",
                                    "ℹ".blue()
                                );
                                let r = &cached.releases;
                                return Ok(r[..limit.min(r.len())].to_vec());
                            }
                            return Err(e.into());
                        }
                    };
                    let is_last = page_releases.len() < 100;
                    all_releases.extend(page_releases);

                    if is_last {
                        break;
                    }
                }
                Err(e) => {
                    if page == 1 && let Some(cached) = cached {
                        eprintln!(
                            "{} Using cached release list (network error: {}).",
                            "ℹ".blue(),
                            e
                        );
                        let r = &cached.releases;
                        return Ok(r[..limit.min(r.len())].to_vec());
                    }
                    return Err(e.into());
                }
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

// --- Release asset lookup (existence check + sha256 digest) ---

pub enum ReleaseLookup {
    /// Release exists; contains its asset objects.
    Found(Vec<serde_json::Value>),
    /// GitHub says the release/tag does not exist.
    NotFound,
    /// Could not reach the API (network error, rate limit, ...).
    Unavailable,
}

pub fn fetch_release_assets(
    client: &reqwest::blocking::Client,
    api_base: &str,
    tag: &str,
) -> ReleaseLookup {
    let url = format!("{}/tags/{}", api_base, tag);
    match client.get(&url).send() {
        Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => ReleaseLookup::NotFound,
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>() {
            Ok(release) => ReleaseLookup::Found(
                release["assets"].as_array().cloned().unwrap_or_default(),
            ),
            Err(_) => ReleaseLookup::Unavailable,
        },
        _ => ReleaseLookup::Unavailable,
    }
}

/// Extract the sha256 hex digest GitHub publishes for an asset, if any.
pub fn asset_sha256(asset: &serde_json::Value) -> Option<String> {
    asset["digest"]
        .as_str()?
        .strip_prefix("sha256:")
        .map(|s| s.to_lowercase())
}

// --- Output formatting ---

/// Table color palette (Gruvbox Material Dark truecolor).
pub struct Colors {
    green: &'static str,
    yellow: &'static str,
    blue: &'static str,
    aqua: &'static str,
    fg: &'static str,
    dim: &'static str,
    border: &'static str,
    reset: &'static str,
    bold: &'static str,
}

pub const COLORS_ON: Colors = Colors {
    green: "\x1b[38;2;169;182;101m",  // #a9b665
    yellow: "\x1b[38;2;216;166;87m",  // #d8a657
    blue: "\x1b[38;2;125;174;163m",   // #7daea3
    aqua: "\x1b[38;2;137;180;130m",   // #89b482
    fg: "\x1b[38;2;221;199;161m",     // #ddc7a1
    dim: "\x1b[38;2;141;135;125m",    // #8d877d
    border: "\x1b[38;2;80;73;69m",    // #504945
    reset: "\x1b[0m",
    bold: "\x1b[1m",
};

pub const COLORS_OFF: Colors = Colors {
    green: "",
    yellow: "",
    blue: "",
    aqua: "",
    fg: "",
    dim: "",
    border: "",
    reset: "",
    bold: "",
};

/// Colors only when stdout is a terminal and NO_COLOR is unset.
fn detect_colors() -> &'static Colors {
    if std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal() {
        &COLORS_ON
    } else {
        &COLORS_OFF
    }
}

pub fn network_label(tag: &str) -> &'static str {
    if tag.contains("mainnet") { "mainnet" }
    else if tag.contains("testnet") { "testnet" }
    else if tag.contains("devnet") { "devnet" }
    else { "other" }
}

fn network_rank(tag: &str) -> u8 {
    match network_label(tag) {
        "mainnet" => 0,
        "testnet" => 1,
        "devnet" => 2,
        _ => 3,
    }
}

fn network_color(net: &str, c: &'static Colors) -> &'static str {
    match net {
        "mainnet" => c.green,
        "testnet" => c.yellow,
        "devnet"  => c.blue,
        _         => c.dim,
    }
}

pub fn extract_version(tag: &str) -> &str {
    tag.split_once('-').map(|(_, v)| v).unwrap_or(tag)
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{} B", bytes);
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{:.1} {}", value, UNITS[unit])
}

fn prompt_yes_no(question: &str) -> Result<bool> {
    use std::io::Write;
    eprint!("{} {} [y/N] ", "?".cyan().bold(), question);
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

fn format_release_line(tag: &str, installed: &[String], c: &'static Colors) -> String {
    let net = network_label(tag);
    let nc = network_color(net, c);
    let ver = extract_version(tag);
    let mark = if installed.iter().any(|i| i == tag) {
        format!(" {}✔{}", c.green, c.reset)
    } else {
        String::new()
    };
    // Pad plain text first, then wrap with color
    format!(
        " {nc}●{r}  {aqua}{b}{ver:<12}{r}  {bd}│{r}  {nc}{net:>7}{r}  {bd}│{r}  {dim}{tag}{r}{mark}",
        r = c.reset,
        aqua = c.aqua,
        b = c.bold,
        bd = c.border,
        dim = c.dim,
    )
}

fn table_header(c: &'static Colors) -> String {
    format!(
        "    {fg}{b}{:<12}{r}  {bd}│{r}  {fg}{b}{:>7}{r}  {bd}│{r}  {fg}{b}Tag{r}",
        "Version",
        "Network",
        fg = c.fg,
        b = c.bold,
        r = c.reset,
        bd = c.border,
    )
}

fn print_release_table(tags: &[&str], installed: &[String], c: &'static Colors) {
    println!("\n{}", table_header(c));
    println!("    {}{}{}", c.border, "─".repeat(48), c.reset);
    for tag in tags {
        println!("{}", format_release_line(tag, installed, c));
    }
    println!();
}

fn list_remote(
    network_filter: Option<String>,
    svm_dir: &Path,
    max_pages: u32,
    plain: bool,
    tags_only: bool,
    cached_only: bool,
) -> Result<()> {
    let releases = if cached_only {
        let mut cached = load_cached_releases(svm_dir).unwrap_or_default();
        cached.truncate((max_pages as usize) * 100);
        cached
    } else {
        fetch_releases_cached(svm_dir, max_pages)?
    };
    let versions_dir = svm_dir.join("versions");

    if let Some(ref filter) = network_filter
        && !NETWORKS.iter().any(|n| n.contains(filter.as_str()))
    {
        eprintln!(
            "{} '{}' does not match any known network ({}) — filtering tags by substring.",
            "⚠".yellow(),
            filter,
            NETWORKS.join(", ")
        );
    }

    let installed: Vec<String> = fs::read_dir(&versions_dir)
        .ok()
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();

    let mut tags: Vec<&str> = Vec::new();
    for release in &releases {
        let tag = release["tag_name"].as_str().unwrap_or("");
        if tag.is_empty() { continue; }
        if let Some(ref filter) = network_filter
            && !tag.contains(filter.as_str())
        {
            continue;
        }
        tags.push(tag);
    }

    if tags_only {
        for tag in &tags {
            println!("{}", tag);
        }
        return Ok(());
    }

    if tags.is_empty() {
        let source = if cached_only { " in the local cache" } else { "" };
        match network_filter {
            Some(f) => println!("{} No releases matching '{}' found{}.", "ℹ".blue(), f, source),
            None => println!("{} No releases found{}.", "ℹ".blue(), source),
        }
        return Ok(());
    }

    let colors = detect_colors();

    if plain || !std::io::stdout().is_terminal() {
        print_release_table(&tags, &installed, colors);
        return Ok(());
    }

    let header = table_header(colors);
    let lines: Vec<String> = tags.iter().map(|tag| format_release_line(tag, &installed, colors)).collect();
    let input = lines.join("\n");

    let spawned = Command::new("fzf")
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
        .spawn();

    let mut child = match spawned {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // fzf missing — degrade to the plain table, as the help text promises
            eprintln!(
                "{} fzf is not installed — showing a plain list instead.",
                "ℹ".blue()
            );
            print_release_table(&tags, &installed, colors);
            return Ok(());
        }
        Err(e) => return Err(e).context("Failed to launch fzf"),
    };

    if let Some(ref mut stdin) = child.stdin {
        use std::io::Write;
        let _ = stdin.write_all(input.as_bytes());
    }
    let output = child.wait_with_output()?;

    if output.status.success() {
        let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if let Some(tag) = tags.iter().find(|t| selected.contains(**t)).copied() {
            println!("{}", tag);
            let already_installed = installed.iter().any(|i| i == tag);
            let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
            if !already_installed
                && interactive
                && prompt_yes_no(&format!("Install {}?", tag.cyan()))?
            {
                install_version(tag, &versions_dir, svm_dir, false, true)?;
            }
        }
    }
    Ok(())
}

// --- Version spec parsing and resolution ---

/// Parse "v1.63.4" or "1.63.4" into (major, minor, patch).
pub fn parse_version_triple(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Parse "v1.63" or "1.63" into (major, minor).
fn parse_version_pair(s: &str) -> Option<(u64, u64)> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor))
}

/// Extract the semantic version from a release tag ("mainnet-v1.63.4" → (1, 63, 4)).
pub fn tag_semver(tag: &str) -> Option<(u64, u64, u64)> {
    parse_version_triple(extract_version(tag))
}

#[derive(Debug, PartialEq, Eq)]
pub enum RemoteSpec {
    /// A concrete tag that can be used directly, without a release lookup.
    Exact(String),
    /// The newest release on a network ("latest", "testnet", ...).
    LatestForNetwork(String),
    /// The newest patch release for a major.minor series ("v1.63").
    Partial { network: String, major: u64, minor: u64 },
}

/// Normalize a user-provided version string into a full release tag.
/// Bare versions like "v1.63.4" or "1.63.4" get a "mainnet-" prefix.
/// Tags already containing a network prefix are left unchanged.
pub fn normalize_install_tag(version: &str) -> String {
    if let Some((major, minor, patch)) = parse_version_triple(version) {
        return format!("mainnet-v{}.{}.{}", major, minor, patch);
    }
    if version.starts_with('v') && !version.contains("net") {
        format!("mainnet-{}", version)
    } else {
        version.to_string()
    }
}

/// Classify a user-provided version spec.
pub fn parse_remote_spec(spec: &str) -> RemoteSpec {
    let spec = spec.trim();
    if spec.eq_ignore_ascii_case("latest") {
        return RemoteSpec::LatestForNetwork("mainnet".into());
    }
    if NETWORKS.contains(&spec) {
        return RemoteSpec::LatestForNetwork(spec.into());
    }
    if let Some((net, rest)) = spec.split_once('-')
        && NETWORKS.contains(&net)
    {
        if rest.eq_ignore_ascii_case("latest") {
            return RemoteSpec::LatestForNetwork(net.into());
        }
        if let Some((major, minor)) = parse_version_pair(rest) {
            return RemoteSpec::Partial { network: net.into(), major, minor };
        }
        if let Some((major, minor, patch)) = parse_version_triple(rest) {
            // "testnet-1.63.4" — insert the "v" like we do for bare versions
            return RemoteSpec::Exact(format!("{}-v{}.{}.{}", net, major, minor, patch));
        }
        return RemoteSpec::Exact(spec.into());
    }
    if let Some((major, minor)) = parse_version_pair(spec) {
        return RemoteSpec::Partial { network: "mainnet".into(), major, minor };
    }
    RemoteSpec::Exact(normalize_install_tag(spec))
}

/// Resolve a version spec against a release list into a concrete tag.
pub fn resolve_remote_spec(spec: &str, releases: &[serde_json::Value]) -> Result<String> {
    let parsed = parse_remote_spec(spec);
    let tags = || {
        releases
            .iter()
            .filter_map(|r| r["tag_name"].as_str())
    };
    match parsed {
        RemoteSpec::Exact(tag) => Ok(tag),
        RemoteSpec::LatestForNetwork(net) => tags()
            .filter(|t| network_label(t) == net)
            .filter_map(|t| tag_semver(t).map(|v| (v, t)))
            .max_by_key(|(v, _)| *v)
            .map(|(_, t)| t.to_string())
            .ok_or_else(|| anyhow!(
                "{} No {} releases found.\n{} Run '{}' to inspect available versions.",
                "error:".red().bold(),
                net.yellow(),
                "help:".blue().bold(),
                "svm remote-list".cyan()
            )),
        RemoteSpec::Partial { network, major, minor } => tags()
            .filter(|t| network_label(t) == network)
            .filter_map(|t| tag_semver(t).map(|v| (v, t)))
            .filter(|((maj, min, _), _)| *maj == major && *min == minor)
            .max_by_key(|(v, _)| *v)
            .map(|(_, t)| t.to_string())
            .ok_or_else(|| anyhow!(
                "{} No release matching {}-v{}.{}.x found in the {} most recent releases.\n{} Try '{}' for older releases.",
                "error:".red().bold(),
                network.yellow(),
                major,
                minor,
                releases.len(),
                "help:".blue().bold(),
                "svm remote-list -p 10".cyan()
            )),
    }
}

// --- Install ---

/// Map an (os, arch) pair to the parts used in Sui release asset names.
/// Note: Sui names macOS ARM assets "arm64" but Linux ARM assets "aarch64".
pub fn asset_platform(os: &str, arch: &str) -> Result<(&'static str, &'static str)> {
    let os_part = match os {
        "macos" => "macos",
        "linux" => "ubuntu",
        other => return Err(anyhow!("Unsupported OS: {} (Sui publishes prebuilt binaries for macOS and Linux)", other)),
    };
    let arch_part = match (os, arch) {
        (_, "x86_64") => "x86_64",
        ("macos", "aarch64") => "arm64",
        ("linux", "aarch64") => "aarch64",
        (_, other) => return Err(anyhow!("Unsupported architecture: {}", other)),
    };
    Ok((os_part, arch_part))
}

/// Asset name parts for the platform svm is running on.
pub fn platform_parts() -> Result<(&'static str, &'static str)> {
    asset_platform(std::env::consts::OS, std::env::consts::ARCH)
}

fn download_archive(
    client: &reqwest::blocking::Client,
    url: &str,
    dest: &Path,
    label: &str,
) -> Result<()> {
    // Status messages in the install pipeline go to stderr: the shim's
    // auto-install path may run while the caller's stdout is piped.
    eprintln!("{} Downloading {}...", "⬇".blue(), label.cyan());

    let response = client.get(url).send()?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(anyhow!(
            "{} Release asset not found: {}.\n{} Run '{}' to see available versions.",
            "error:".red().bold(),
            url.yellow(),
            "help:".blue().bold(),
            "svm remote-list".cyan()
        ));
    }
    if !response.status().is_success() {
        return Err(anyhow!(
            "{} Download failed (HTTP {}) for {}",
            "error:".red().bold(),
            response.status(),
            url
        ));
    }

    let total_size = response.content_length().unwrap_or(0);
    let pb = if total_size > 0 {
        let pb = ProgressBar::new(total_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")
                .unwrap()
                .progress_chars("#>-"),
        );
        pb
    } else {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {bytes} downloaded ({bytes_per_sec})")
                .unwrap(),
        );
        pb
    };

    // Stream to a partial file so an interrupted download is never
    // mistaken for a complete cached archive.
    let partial = dest.with_extension("partial");
    let result = (|| -> Result<u64> {
        let mut reader = pb.wrap_read(response);
        let mut file = fs::File::create(&partial)?;
        let bytes = std::io::copy(&mut reader, &mut file)?;
        Ok(bytes)
    })();
    pb.finish_and_clear();

    match result {
        Ok(bytes) => {
            fs::rename(&partial, dest)?;
            eprintln!("{} Downloaded {}.", "✔".green(), human_bytes(bytes));
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&partial);
            Err(e)
        }
    }
}

pub fn verify_sha256(path: &Path, expected: &str) -> Result<bool> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    let actual = format!("{:x}", hasher.finalize());
    Ok(actual == expected.to_lowercase())
}

/// Unpack an archive into versions_dir/<full_tag> atomically: extract into a
/// staging directory first, then rename. A failed unpack never leaves a
/// half-populated version directory behind. The staging name includes the
/// pid so concurrent installs of the same tag don't clobber each other.
pub fn unpack_archive(archive_path: &Path, versions_dir: &Path, full_tag: &str) -> Result<()> {
    let target_dir = versions_dir.join(full_tag);
    let staging = versions_dir.join(format!(".staging-{}-{}", full_tag, std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;

    let result = (|| -> Result<()> {
        let file = fs::File::open(archive_path)?;
        let tar = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
        let mut archive = tar::Archive::new(tar);
        archive.unpack(&staging)?;
        Ok(())
    })();

    if let Err(e) = result {
        let _ = fs::remove_dir_all(&staging);
        // The archive is unusable — drop it from the cache so a retry redownloads.
        let _ = fs::remove_file(archive_path);
        return Err(anyhow!(
            "{} Failed to unpack archive: {}\n{} The cached archive was removed — retry the install.",
            "error:".red().bold(),
            e,
            "help:".blue().bold()
        ));
    }

    fs::rename(&staging, &target_dir)
        .with_context(|| format!("Failed to move unpacked files into {:?}", target_dir))?;
    Ok(())
}

fn install_version(
    spec: &str,
    versions_dir: &Path,
    svm_dir: &Path,
    use_after: bool,
    auto_activate: bool,
) -> Result<String> {
    let full_tag = match parse_remote_spec(spec) {
        RemoteSpec::Exact(tag) => tag,
        _ => {
            let releases = fetch_releases_cached(svm_dir, 3)?;
            resolve_remote_spec(spec, &releases)?
        }
    };

    if !valid_version_name(&full_tag) {
        return Err(anyhow!(
            "{} Invalid version name: {}",
            "error:".red().bold(),
            full_tag.yellow()
        ));
    }

    let bin_dir = svm_dir.join("bin");
    let target_dir = versions_dir.join(&full_tag);
    if target_dir.exists() {
        eprintln!("{} Version {} is already installed.", "ℹ".blue(), full_tag.cyan());
        if use_after {
            use_version(&full_tag, versions_dir, &bin_dir, false)?;
        }
        return Ok(full_tag);
    }

    let (os_part, arch_part) = platform_parts()?;
    let asset_name = format!("sui-{}-{}-{}.tgz", full_tag, os_part, arch_part);
    let url = format!("{}/{}/{}", download_base(), full_tag, asset_name);

    let client = build_client()?;

    // Look up the release before downloading: confirms the tag exists, catches
    // releases without a build for this platform, and yields the sha256 digest
    // GitHub publishes per asset.
    let expected_sha256 = match fetch_release_assets(&client, &api_base(), &full_tag) {
        ReleaseLookup::NotFound => {
            return Err(anyhow!(
                "{} Release not found: {}.\n{} Run '{}' to see available versions.",
                "error:".red().bold(),
                full_tag.yellow(),
                "help:".blue().bold(),
                "svm remote-list".cyan()
            ));
        }
        ReleaseLookup::Found(assets) => {
            let matching = assets.iter().find(|a| a["name"].as_str() == Some(asset_name.as_str()));
            match matching {
                Some(asset) => asset_sha256(asset),
                None => {
                    let available: Vec<&str> = assets
                        .iter()
                        .filter_map(|a| a["name"].as_str())
                        .filter(|n| n.starts_with("sui-") && n.ends_with(".tgz"))
                        .collect();
                    return Err(anyhow!(
                        "{} Release {} has no prebuilt archive for {}-{}.\n  Available archives: {}\n{} Try another release, or build from source and use '{}'.",
                        "error:".red().bold(),
                        full_tag.yellow(),
                        os_part,
                        arch_part,
                        if available.is_empty() { "none".to_string() } else { available.join(", ") },
                        "help:".blue().bold(),
                        "svm link".cyan()
                    ));
                }
            }
        }
        ReleaseLookup::Unavailable => {
            eprintln!(
                "{} Could not query GitHub for release metadata — proceeding without checksum verification.",
                "⚠".yellow()
            );
            None
        }
    };

    let cache = cache_dir(svm_dir)?;
    let archive_path = cache.join(&asset_name);

    let from_cache = archive_path.exists();
    if from_cache {
        eprintln!("{} Using cached archive for {}...", "ℹ".blue(), full_tag.cyan());
    } else {
        download_archive(&client, &url, &archive_path, &full_tag)?;
    }

    if let Some(ref expected) = expected_sha256 {
        let mut ok = verify_sha256(&archive_path, expected)?;
        if !ok && from_cache {
            // Stale or corrupted cache entry — replace it and try once more.
            eprintln!(
                "{} Cached archive failed checksum verification — re-downloading.",
                "⚠".yellow()
            );
            fs::remove_file(&archive_path)?;
            download_archive(&client, &url, &archive_path, &full_tag)?;
            ok = verify_sha256(&archive_path, expected)?;
        }
        if !ok {
            let _ = fs::remove_file(&archive_path);
            return Err(anyhow!(
                "{} SHA-256 verification failed for {}.\n  The downloaded archive does not match the digest published on GitHub.\n{} Please retry; if it persists, the release asset may be corrupted.",
                "error:".red().bold(),
                asset_name.yellow(),
                "help:".blue().bold()
            ));
        }
        eprintln!("{} SHA-256 checksum verified.", "✔".green());
    }

    unpack_archive(&archive_path, versions_dir, &full_tag)?;

    if cfg!(target_os = "macos") {
        let _ = Command::new("xattr")
            .args(["-rd", "com.apple.quarantine"])
            .arg(&target_dir)
            .stderr(Stdio::null())
            .status();
    }

    eprintln!(
        "{} Successfully installed {} to {:?}",
        "✔".green(),
        full_tag.green().bold(),
        target_dir
    );

    if !target_dir.join("sui").exists() {
        eprintln!(
            "{} The archive did not contain a 'sui' binary at its top level — the layout may have changed.",
            "⚠".yellow()
        );
    }

    if use_after {
        use_version(&full_tag, versions_dir, &bin_dir, false)?;
    } else if auto_activate {
        // auto_activate is false when a caller (e.g. `svm use`) activates itself
        if resolve_version(svm_dir)?.is_none() {
            eprintln!("{} No version was active — activating {}.", "ℹ".blue(), full_tag.cyan());
            use_version(&full_tag, versions_dir, &bin_dir, false)?;
        } else {
            eprintln!("  Run '{}' to switch to it.", format!("svm use {}", full_tag).cyan());
        }
    }

    Ok(full_tag)
}

/// The network of a proper release tag ("<network>-vX.Y.Z"). Returns None for
/// anything else — unlike network_label's substring matching, a linked build
/// named "mainnet-fork" is not mistaken for a mainnet release here.
pub fn release_network(tag: &str) -> Option<&'static str> {
    let (net, rest) = tag.split_once('-')?;
    let net = NETWORKS.iter().find(|n| **n == net)?;
    parse_version_triple(rest)?;
    Some(net)
}

/// Decide whether an update is available for the (resolved) active tag.
/// Returns Ok(None) when already up to date, Ok(Some(tag)) when a newer
/// release exists, Err for non-release (linked/custom) versions.
pub fn plan_update(current: &str, releases: &[serde_json::Value]) -> Result<Option<String>> {
    let Some(net) = release_network(current) else {
        return Err(anyhow!(
            "{} The active version '{}' is a linked/custom build — '{}' only works with release versions.",
            "error:".red().bold(),
            current.yellow(),
            "svm update".cyan()
        ));
    };

    let latest = resolve_remote_spec(net, releases)?;
    if latest == current {
        return Ok(None);
    }
    if let (Some(cur), Some(new)) = (tag_semver(current), tag_semver(&latest))
        && new <= cur
    {
        return Ok(None);
    }
    Ok(Some(latest))
}

fn update_version(versions_dir: &Path, svm_dir: &Path) -> Result<()> {
    let (current_raw, source) = resolve_version(svm_dir)?.ok_or_else(|| anyhow!(
        "{} No version is currently active.\n{} Run '{}' first.",
        "error:".red().bold(),
        "help:".blue().bold(),
        "svm install latest".cyan()
    ))?;

    // A version file may hold a shorthand like "v1.63.4" or a series/channel
    // pin like "testnet" — resolve it to the installed directory name (or
    // normalized tag) before classifying it.
    let current = resolve_available_version(&current_raw, versions_dir)
        .map(|(name, _)| name)
        .unwrap_or_else(|| normalize_install_tag(&current_raw));

    let releases = fetch_releases_cached(svm_dir, 3)?;
    let Some(latest) = plan_update(&current, &releases)? else {
        println!(
            "{} {} is already the latest {} release.",
            "✔".green(),
            current.green().bold(),
            network_label(&current)
        );
        return Ok(());
    };

    println!(
        "{} Updating {} {} {}",
        "➜".cyan(),
        current.yellow(),
        "→".dimmed(),
        latest.green().bold()
    );

    // A series/channel pin ("testnet", "v1.63") resolves to the newest
    // installed match at shim time — after installing the newer release it
    // already points at it, so the pin must not be rewritten to a frozen tag.
    let pin_is_exact = matches!(parse_remote_spec(&current_raw), RemoteSpec::Exact(_));

    match source {
        VersionSource::Global => {
            if pin_is_exact {
                install_version(&latest, versions_dir, svm_dir, true, true)?;
            } else {
                install_version(&latest, versions_dir, svm_dir, false, false)?;
                println!(
                    "{} Global setting '{}' now resolves to {}.",
                    "✔".green(),
                    sanitize_display(&current_raw),
                    latest.green().bold()
                );
            }
        }
        VersionSource::Local(path) => {
            // The active version came from a .svm-version pin: update the pin,
            // not the global default — 'svm update' must take effect where it
            // was run and nowhere else.
            install_version(&latest, versions_dir, svm_dir, false, false)?;
            if pin_is_exact {
                fs::write(&path, format!("{}\n", latest))?;
                println!(
                    "{} Updated pin {} to {}.",
                    "✔".green(),
                    path.display().to_string().blue(),
                    latest.green().bold()
                );
            } else {
                println!(
                    "{} Pin '{}' in {} now resolves to {}.",
                    "✔".green(),
                    sanitize_display(&current_raw),
                    path.display().to_string().blue(),
                    latest.green().bold()
                );
            }
        }
    }
    Ok(())
}

// --- Local version management ---

/// Resolve a version name to an existing directory under versions_dir.
/// Tries the exact name first (e.g. a linked build named "v-custom"),
/// then falls back to the normalized tag (e.g. v1.63.4 → mainnet-v1.63.4).
pub fn resolve_installed_version(version: &str, versions_dir: &Path) -> Option<(String, PathBuf)> {
    if !valid_version_name(version) {
        return None;
    }
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

/// Resolve a non-exact spec ("v1.63", "testnet", "latest") against the
/// *installed* versions: the newest installed patch of a series, or the
/// newest installed release on a network. This keeps shims working offline
/// and lets series/channel pins converge instead of re-triggering installs.
fn resolve_spec_against_installed(spec: &str, versions_dir: &Path) -> Option<(String, PathBuf)> {
    let (network, series) = match parse_remote_spec(spec) {
        RemoteSpec::LatestForNetwork(net) => (net, None),
        RemoteSpec::Partial { network, major, minor } => (network, Some((major, minor))),
        RemoteSpec::Exact(_) => return None,
    };
    let (_, name) = fs::read_dir(versions_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| release_network(name) == Some(network.as_str()))
        .filter_map(|name| tag_semver(&name).map(|v| (v, name)))
        .filter(|((major, minor, _), _)| series.is_none_or(|(ma, mi)| *major == ma && *minor == mi))
        .max_by_key(|(v, _)| *v)?;
    let path = versions_dir.join(&name);
    Some((name, path))
}

/// Resolve a version string to an installed directory: the exact/normalized
/// name first, then non-exact specs against what is installed. This is the
/// resolution the shims, `svm which`, and `svm exec` share.
pub fn resolve_available_version(version: &str, versions_dir: &Path) -> Option<(String, PathBuf)> {
    resolve_installed_version(version, versions_dir)
        .or_else(|| resolve_spec_against_installed(version, versions_dir))
}

pub fn uninstall_version(version: &str, versions_dir: &Path, svm_dir: &Path) -> Result<()> {
    let (resolved, target_dir) = resolve_installed_version(version, versions_dir)
        .ok_or_else(|| anyhow!(
            "{} Version '{}' is not installed.",
            "error:".red().bold(),
            version.yellow()
        ))?;

    fs::remove_dir_all(&target_dir)?;
    println!("{} Successfully uninstalled {}.", "✔".green(), resolved.red().bold());

    // If this was the globally active version, clear the global version file so
    // shims report a clear error instead of pointing at a missing directory.
    let global_file = svm_dir.join("version");
    if global_file.exists()
        && let Some(active) = read_version_file(&global_file)?
        && (active == resolved || normalize_install_tag(&active) == resolved)
    {
        fs::remove_file(&global_file)?;
        println!(
            "{} {} was the active version — run '{}' to pick another.",
            "⚠".yellow(),
            resolved.yellow(),
            "svm use <version>".cyan()
        );
    }
    Ok(())
}

fn warn_if_shims_not_on_path(bin_dir: &Path) {
    let Ok(path_var) = std::env::var("PATH") else { return };
    if std::env::split_paths(&path_var).any(|p| p == bin_dir) {
        return;
    }
    eprintln!(
        "\n{} {} is not on your PATH — the '{}' command won't resolve to svm's shims.",
        "⚠".yellow(),
        bin_dir.display().to_string().blue(),
        "sui".cyan()
    );
    eprintln!("  Add this to your shell profile:");
    eprintln!("    {}", "export PATH=\"$HOME/.svm/bin:$PATH\"".cyan());
}

fn use_version(version: &str, versions_dir: &Path, bin_dir: &Path, local: bool) -> Result<()> {
    let resolved = match resolve_installed_version(version, versions_dir) {
        Some((resolved, _)) => resolved,
        None => {
            let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
            if interactive
                && prompt_yes_no(&format!(
                    "Version {} is not installed. Install it now?",
                    version.cyan()
                ))?
            {
                let svm_dir = versions_dir.parent().context("versions dir has no parent")?;
                let tag = install_version(version, versions_dir, svm_dir, false, false)?;
                resolve_installed_version(&tag, versions_dir)
                    .map(|(resolved, _)| resolved)
                    .ok_or_else(|| anyhow!("Installed {} but could not locate it afterwards", tag))?
            } else {
                return Err(anyhow!(
                    "{} Version '{}' not found.\n{} Run '{}' first.",
                    "error:".red().bold(),
                    version.yellow(),
                    "help:".blue().bold(),
                    format!("svm install {}", version).cyan()
                ));
            }
        }
    };

    if local {
        let cwd = std::env::current_dir()?;
        let version_file = cwd.join(".svm-version");
        fs::write(&version_file, format!("{}\n", resolved))?;
        println!(
            "{} Set local version to {} in {}",
            "✔".green(),
            resolved.green().bold(),
            version_file.display().to_string().dimmed()
        );
    } else {
        let svm_dir = versions_dir.parent().context("versions dir has no parent")?;
        fs::write(svm_dir.join("version"), format!("{}\n", resolved))?;
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

    warn_if_shims_not_on_path(bin_dir);
    Ok(())
}

/// Source path recorded by `svm link` for a copied build (the sui entry, or
/// the first entry when sui was not among the linked binaries).
fn link_source(version_dir: &Path) -> Option<String> {
    let content = fs::read_to_string(version_dir.join(LINK_MARKER)).ok()?;
    let line = content
        .lines()
        .find(|l| l.starts_with("sui "))
        .or_else(|| content.lines().next())?;
    line.split_once(' ').map(|(_, path)| path.to_string())
}

fn link_local(name: &str, path: Option<&Path>, versions_dir: &Path) -> Result<()> {
    if !valid_version_name(name) {
        return Err(anyhow!(
            "{} Invalid link name: {} (must be a plain name without '/' and not starting with '.')",
            "error:".red().bold(),
            name.yellow()
        ));
    }

    // Refuse to clobber an installed release. Only release-shaped names are
    // protected; a linked build is recognized by its .svm-link marker or, for
    // layouts made by older svm versions, by symlinked binaries. Anything
    // else release-shaped is treated as an installed release — even when the
    // archive layout hid the binaries — because a re-link now replaces the
    // whole directory.
    let target_dir = versions_dir.join(name);
    if target_dir.exists()
        && release_network(name).is_some()
        && !target_dir.join(LINK_MARKER).exists()
    {
        let is_legacy_link = SVM_BINARIES.iter().any(|bin| target_dir.join(bin).is_symlink());
        if !is_legacy_link {
            return Err(anyhow!(
                "{} '{}' is an installed release, not a linked build.\n{} Pick a different name, or run '{}' first.",
                "error:".red().bold(),
                name.yellow(),
                "help:".blue().bold(),
                format!("svm uninstall {}", name).cyan()
            ));
        }
    }

    let cwd = std::env::current_dir()?;
    let search_dirs: Vec<PathBuf> = match path {
        Some(p) => vec![p.to_path_buf()],
        None => vec![
            cwd.clone(),
            cwd.join("target/release"),
            cwd.join("target/debug"),
        ],
    };

    // Find each managed binary in the first search dir that has it. Compare
    // sources against the canonicalized target too — the sources are
    // canonicalized, and a symlinked $HOME component would otherwise let a
    // source inside the target slip past and be wiped below.
    let target_guard = target_dir.canonicalize().unwrap_or_else(|_| target_dir.clone());
    let mut found: Vec<(&str, PathBuf)> = Vec::new();
    for bin in SVM_BINARIES {
        if let Some(src) = search_dirs.iter().map(|d| d.join(bin)).find(|p| p.exists()) {
            let src = src.canonicalize()?;
            if src.starts_with(&target_guard) || src.starts_with(&target_dir) {
                return Err(anyhow!(
                    "{} Source {} is inside the link target {:?}.",
                    "error:".red().bold(),
                    src.display().to_string().yellow(),
                    target_dir
                ));
            }
            found.push((bin, src));
        }
    }

    if found.is_empty() {
        return Err(anyhow!(
            "{} No Sui binaries found.\n  Expected {:?} in one of: {}",
            "error:".red().bold(),
            SVM_BINARIES,
            search_dirs
                .iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // Copy rather than symlink: a rebuild overwrites the source binaries in
    // place, and a symlinked version would silently start pointing at a build
    // the user never registered. The copy stays exactly what was linked until
    // an explicit re-link refreshes it. Stage first, swap after — a failed
    // copy must not destroy the previous link (same pattern as unpack_archive).
    let staging = versions_dir.join(format!(".staging-link-{}-{}", name, std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;

    let result = (|| -> Result<()> {
        let mut marker = String::new();
        for (bin, src) in &found {
            fs::copy(src, staging.join(bin))
                .with_context(|| format!("Failed to copy {} from {:?}", bin, src))?;
            marker.push_str(&format!("{} {}\n", bin, src.display()));
        }
        fs::write(staging.join(LINK_MARKER), marker)?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }

    if target_dir.exists() {
        fs::remove_dir_all(&target_dir)
            .with_context(|| format!("Failed to clear {:?} for re-linking", target_dir))?;
    }
    fs::rename(&staging, &target_dir)
        .with_context(|| format!("Failed to move linked build into {:?}", target_dir))?;

    for (bin, src) in &found {
        println!(
            "  {} Copied {} {} {}",
            "↳".dimmed(),
            bin.cyan(),
            "←".dimmed(),
            src.display().to_string().dimmed()
        );
    }

    println!(
        "{} Local build copied as '{}' — re-run '{}' after a rebuild to refresh it.",
        "✔".green(),
        name.green().bold(),
        format!("svm link {}", name).cyan()
    );
    Ok(())
}

/// Sort key for installed version names: group by network (mainnet, testnet,
/// devnet, other), newest first within a group, and names without a parsable
/// version after versioned ones, alphabetically. Must be a total order —
/// mixing comparison strategies inside one group can break transitivity.
pub fn version_sort_key(name: &str) -> (u8, u8, std::cmp::Reverse<(u64, u64, u64)>, String) {
    let semver = tag_semver(name);
    (
        network_rank(name),
        semver.is_none() as u8,
        std::cmp::Reverse(semver.unwrap_or((0, 0, 0))),
        name.to_string(),
    )
}

fn list_local(versions_dir: &Path, svm_dir: &Path) -> Result<()> {
    let active = resolve_version(svm_dir)?.map(|(v, _)| {
        resolve_available_version(&v, versions_dir)
            .map(|(name, _)| name)
            .unwrap_or(v)
    });

    let mut names: Vec<String> = fs::read_dir(versions_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();

    if names.is_empty() {
        println!(
            "{} No versions installed. Run '{}' to get started.",
            "ℹ".blue(),
            "svm install latest".cyan()
        );
        return Ok(());
    }

    names.sort_by_key(|n| version_sort_key(n));

    println!("\n{:<2} {:<25}", "", "Installed Versions".bold());
    println!("{}", "-".repeat(30).dimmed());

    for name in names {
        let version_dir = versions_dir.join(&name);
        // Legacy links show the symlink target; copied links show the
        // recorded source from their .svm-link marker.
        let link_note = fs::read_link(version_dir.join("sui"))
            .ok()
            .map(|t| format!(" ↪ {}", t.display()))
            .or_else(|| link_source(&version_dir).map(|src| format!(" ↪ {} (copy)", src)))
            .unwrap_or_default();

        if active.as_deref() == Some(name.as_str()) {
            println!(
                "{} {:<25} {}{}",
                "✔".green(),
                name.green().bold(),
                "(active)".dimmed(),
                link_note.dimmed()
            );
        } else {
            println!("  {:<25}{}", name.dimmed(), link_note.dimmed());
        }
    }
    println!();
    Ok(())
}

// --- Cache management ---

fn cache_command(svm_dir: &Path, action: Option<CacheAction>) -> Result<()> {
    let cache = cache_dir(svm_dir)?;

    let archive_entries = || -> Result<Vec<(PathBuf, u64)>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&cache)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".tgz") || name.ends_with(".partial") {
                out.push((entry.path(), entry.metadata()?.len()));
            }
        }
        Ok(out)
    };

    match action {
        None => {
            let archives = archive_entries()?;
            let total: u64 = archives.iter().map(|(_, size)| size).sum();
            println!("{} Cache directory: {}", "➜".cyan(), cache.display().to_string().blue());
            println!("  Release archives: {} ({})", archives.len(), human_bytes(total));
            let releases_file = cache.join("releases.json");
            if let Ok(meta) = fs::metadata(&releases_file) {
                println!("  Release list cache: {}", human_bytes(meta.len()));
            } else {
                println!("  Release list cache: {}", "none".dimmed());
            }
            if !archives.is_empty() {
                println!(
                    "\n  Run '{}' to remove downloaded archives.",
                    "svm cache clean".cyan()
                );
            }
        }
        Some(CacheAction::Clean { all }) => {
            let archives = archive_entries()?;
            let mut freed: u64 = 0;
            let mut removed = 0usize;
            for (path, size) in archives {
                fs::remove_file(&path)?;
                freed += size;
                removed += 1;
            }
            // Sweep staging directories left behind by crashed installs
            let versions_dir = svm_dir.join("versions");
            if let Ok(entries) = fs::read_dir(&versions_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with(".staging-") {
                        fs::remove_dir_all(entry.path())?;
                        println!("{} Removed leftover staging directory {}.", "✔".green(), name.dimmed());
                    }
                }
            }
            if all {
                let releases_file = cache.join("releases.json");
                if releases_file.exists() {
                    fs::remove_file(&releases_file)?;
                    println!("{} Removed release list cache.", "✔".green());
                }
            }
            if removed > 0 {
                println!(
                    "{} Removed {} archive(s), freed {}.",
                    "✔".green(),
                    removed,
                    human_bytes(freed)
                );
            } else {
                println!("{} No archives to remove.", "ℹ".blue());
            }
        }
    }
    Ok(())
}

// --- Completions ---

fn print_completions(shell: Shell) {
    print!("{}", completion_script(shell));
}

/// Render the completion script for a shell. Zsh gets dynamic version
/// completion wired in; other shells use the stock clap output.
pub fn completion_script(shell: Shell) -> String {
    let mut cmd = Cli::command();
    let bin_name = "svm";

    let mut buffer = Vec::new();
    generate(shell, &mut cmd, bin_name, &mut buffer);
    let script = String::from_utf8_lossy(&buffer).into_owned();

    if shell != Shell::Zsh {
        return script;
    }

    let custom_funcs = r#"
_svm_local_versions() {
    local -a versions
    versions=($(command ls -1 $HOME/.svm/versions 2>/dev/null | grep -v '^\.'))
    _describe 'installed versions' versions
}
_svm_remote_versions() {
    local -a versions
    versions=($(svm remote-list --tags-only --cached 2>/dev/null))
    if [[ ${#versions[@]} -eq 0 ]]; then
        versions=($(svm remote-list --tags-only 2>/dev/null))
    fi
    _describe 'available versions' versions
}
"#;

    let mut final_script =
        script.replace("#compdef svm", &format!("#compdef svm\n{}", custom_funcs));

    // install should complete with remote versions
    final_script = final_script.replacen("':version:_default'", "':version:_svm_remote_versions'", 1);
    // use, uninstall, and exec should complete with local versions
    final_script = final_script.replace("':version:_default'", "':version:_svm_local_versions'");
    // link name gets no special completion (user picks the name)
    final_script = final_script.replace("':name:_default'", "':name:'");
    // which completes the managed binary names
    final_script = final_script.replace(
        "'::binary:_default'",
        &format!("'::binary:({})'", SVM_BINARIES.join(" ")),
    );

    final_script
}
