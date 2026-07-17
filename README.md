# svm — Sui Version Manager

A fast, `nvm`-style version manager for the [Sui](https://github.com/MystenLabs/sui) toolchain.
Install prebuilt releases, switch between them globally or per-project, and link your own
`cargo build` outputs — all through lightweight shims.

```console
$ svm install latest
⬇ Downloading mainnet-v1.74.1...
✔ Downloaded 360.3 MiB.
✔ SHA-256 checksum verified.
✔ Successfully installed mainnet-v1.74.1
✨ Active version set to: mainnet-v1.74.1

$ sui --version
sui 1.74.1-8fc60f1fa966
```

## Installation

```sh
cargo install --path .
```

Then put the shim directory on your `PATH` (svm will remind you if you forget):

```sh
export PATH="$HOME/.svm/bin:$PATH"
```

`svm` manages two binaries: `sui` and `move-analyzer`. Both resolve through
`~/.svm/bin` shims that dispatch to whichever version is currently active.

## Quick start

```sh
svm install latest        # newest mainnet release (auto-activates if nothing is)
svm install testnet       # newest testnet release
svm install v1.63         # newest 1.63.x patch on mainnet
svm install v1.63.4 --use # exact version, switch to it immediately
svm use testnet-v1.74.1   # switch (offers to install if missing)
svm list                  # what's installed, active one marked
svm update                # move the active version to its network's latest
```

## Version specs

Anywhere a version is accepted, these all work:

| Spec | Meaning |
|---|---|
| `latest` | newest mainnet release |
| `mainnet` / `testnet` / `devnet` | newest release on that network |
| `testnet-latest` | same, alternative spelling |
| `v1.63.4` or `1.63.4` | exact version, mainnet |
| `v1.63` or `1.63` | newest `1.63.x` patch, mainnet |
| `testnet-v1.63` | newest `1.63.x` patch on testnet |
| `mainnet-v1.63.4` | full release tag, used as-is |
| `my-dev` | a linked local build (see below) |

Bare versions default to **mainnet** (note: `suiup` defaults to testnet).

## Per-project versions

`svm use --local <version>` writes a `.svm-version` file in the current directory.
The shims walk up from wherever you run `sui`, so each project can pin its own
toolchain — the global version applies everywhere else.

```sh
cd my-dapp
svm use --local testnet-v1.73.1
sui --version   # 1.73.1 here, global version elsewhere
```

A pin doesn't have to be a full tag: a series (`v1.63`) or channel (`testnet`)
pin resolves to the newest **installed** match every time a shim runs — so
`svm update` moves such projects forward without touching their `.svm-version`.

When a pinned version isn't installed yet (e.g. right after cloning a repo),
the shim offers to install it on the spot. Control this with `SVM_AUTO_INSTALL`:

| Value | Behavior |
|---|---|
| unset | ask interactively (never prompts without a TTY) |
| `1` / `true` / `yes` / `on` | install silently — clone-and-go, good for CI |
| `0` / `false` / `no` / `off` | never install from the shim |

Auto-install writes all progress to stderr, so even a piped `sui ... \| jq`
stays clean on first use.

## Linking local builds

Working on Sui itself? Register your build output under a name:

```sh
cd ~/code/sui
svm link my-fork              # finds ./target/release or ./target/debug automatically
svm link my-fork ~/some/dir   # or point at an explicit directory
svm use my-fork
```

`svm link` **copies** the binaries into `~/.svm/versions/<name>/`. A later
`cargo build` overwrites your `target/release` in place — the copy keeps the
registered version exactly as it was until you re-run `svm link <name>` to
refresh it, so a rebuild can never silently change what a linked version runs.
`svm list` shows where each linked build was copied from.

## Commands

| Command | Description |
|---|---|
| `svm remote-list` | Browse releases (interactive fzf picker with install prompt; falls back to a plain table) |
| `svm remote-list --plain` | Plain table (also used automatically when piped) |
| `svm remote-list --tags-only` | Bare tags, one per line, for scripting |
| `svm remote-list --cached` | Offline: read the local release cache only |
| `svm remote-list -n testnet -p 5` | Filter by network, fetch up to 5 pages (100 releases each) |
| `svm install <spec> [--use]` | Install a version, optionally switch to it |
| `svm update` | Update the active version to its network's latest release (updates the `.svm-version` pin when run in a pinned directory, the global default otherwise) |
| `svm use <spec> [--local]` | Switch version globally or pin it for the current directory |
| `svm exec <spec> <cmd...>` | Run one command under a specific installed version, without switching |
| `svm which [binary]` | Print the full path the shim resolves to (default: `sui`) |
| `svm uninstall <spec>` | Remove a version (clears the global setting if it was active) |
| `svm link <name> [path]` | Register a local build as a switchable version (copies the binaries) |
| `svm list` | Installed versions, grouped by network, newest first |
| `svm show` | Active version and where it's set from |
| `svm unset` | Remove shims and the global version |
| `svm cache` | Show cache location and size |
| `svm cache clean [--all]` | Delete downloaded archives (`--all`: also the release list) |
| `svm completions <shell>` | Shell completions for zsh, bash, or fish |

## Integrity & resilience

- Downloads stream to disk (constant memory, even for 1 GB Linux archives) and are
  verified against the **SHA-256 digest GitHub publishes** for each release asset.
- Archives unpack into a staging directory and are moved into place atomically — an
  interrupted or failed install never leaves a half-broken version behind.
- The release list is cached with ETag revalidation; when GitHub is unreachable or
  rate-limits you, svm falls back to the cached list.
- Set `GITHUB_TOKEN` to raise API rate limits (useful in CI).
- `SVM_API_BASE` / `SVM_DOWNLOAD_BASE` override the GitHub endpoints (mirrors,
  proxies, air-gapped CI). Checksums are verified against whatever the API side
  publishes, so only mirror the API from a source you trust. `GITHUB_TOKEN` is
  **not** sent when these overrides are active — it never leaves github.com.

Downloaded archives stay in `~/.svm/cache` so reinstalls are instant; run
`svm cache clean` when you want the disk back.

## Shell completions

```sh
# zsh (with dynamic version completion for install/use/uninstall)
svm completions zsh > ~/.zfunc/_svm

# bash
svm completions bash > /usr/local/etc/bash_completion.d/svm

# fish
svm completions fish > ~/.config/fish/completions/svm.fish
```

## How it compares to suiup

[`suiup`](https://github.com/MystenLabs/suiup) is MystenLabs' official installer and
manages many tools (walrus, mvr, seal, …). `svm` is deliberately smaller — just the
`sui` CLI and `move-analyzer` — and does a few things suiup doesn't:

- **Per-directory pinning** via `.svm-version` (suiup has no per-directory story),
  with **auto-install** of pinned-but-missing versions
- **Partial version resolution** (`v1.63` → newest patch)
- **Checksum verification** of downloads (via GitHub's published sha256 digests)
- **Registered local builds** — `svm link` snapshots your `cargo build` output
  under a name; refresh it explicitly when you want the new build
- **One-shot runs** under any installed version via `svm exec`

## Layout

```
~/.svm/
├── bin/          # shims (sui, move-analyzer) — put this on PATH
├── versions/     # one directory per installed version / linked build
├── cache/        # downloaded archives + release list cache
└── version       # globally active version
```

## Requirements

- macOS or Linux (x86_64 / ARM)
- [`fzf`](https://github.com/junegunn/fzf) (optional) for the interactive `remote-list` picker
