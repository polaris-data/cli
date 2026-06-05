![Polaris splash screen](assets/polaris-splash-screen.png)

<h1 align="center">Polaris</h1>

<p align="center">
  Access and manage high-fidelity market data from Hyperliquid, Lighter, and more.
</p>

<p align="center">
  <a href="#installation">Installation</a> |
  <a href="#quick-start">Quick Start</a> |
  <a href="#common-workflows">Common Workflows</a> |
  <a href="#cli-overview">CLI Overview</a> |
  <a href="#command-reference">Command Reference</a> |
  <a href="#configuration">Configuration</a>
</p>

---

## Why Polaris

- Browse remote exchange and asset datasets from the terminal
- Sync only the snapshot ranges you need
- Materialize full UTC-day local files automatically when a day is fully present
- Inspect the local dataset tree managed by Polaris
- Automate data workflows with plain CLI output or `--json`

![Polaris product screenshot](assets/polaris-product-screen.png)

## Requirements

- Rust toolchain with `cargo` for local development
- Network access to the Polaris API
- Optional: a Polaris API key for authenticated Polaris access

## Installation

Install the latest GitHub release:

```bash
curl -fsSL https://raw.githubusercontent.com/polaris-data/cli/main/install.sh | bash
```

Update later by re-running the same command, or from the installed CLI:

```bash
polaris update
```

Install a pinned version:

```bash
curl -fsSL https://raw.githubusercontent.com/polaris-data/cli/main/install.sh | bash -s -- --version v0.2.0
```

Install into a custom directory:

```bash
curl -fsSL https://raw.githubusercontent.com/polaris-data/cli/main/install.sh | bash -s -- --install-dir "$HOME/.local/bin"
```

Default install location for new installs:

```text
~/.polaris/bin/polaris
```

The installer adds the install directory to your shell profile if needed:

- zsh: `~/.zshenv` or `$ZDOTDIR/.zshenv`
- bash: `~/.bashrc`
- fish: `~/.config/fish/config.fish`
- ash and fallback shells: `~/.profile`

Supported prebuilt release targets:

- macOS `x86_64`
- macOS Apple Silicon
- Linux `x86_64`
- Linux `aarch64`

Compatibility notes:

- If `~/.tick/bin` already exists, the installer reuses that legacy install directory
- The installer also creates a `tick` symlink pointing at `polaris`

## Quick Start

### 1. Install Polaris

```bash
curl -fsSL https://raw.githubusercontent.com/polaris-data/cli/main/install.sh | bash
```

### 2. Configure access

If you have an API key, store it in the OS credential store:

```bash
polaris account set-key
```

Or set it per-session:

```bash
export POLARIS_API_KEY="your_api_key"
```

Check whether Polaris sees a configured credential:

```bash
polaris account status
```

### 3. Browse remote datasets

```bash
polaris list --exchange aster --asset BTCUSDT
```

### 4. Sync one time range

```bash
polaris sync \
  --exchange aster \
  --asset BTCUSDT \
  --from 2026-06-01T00:00:00Z \
  --to 2026-06-02T00:00:00Z
```

### 5. Inspect local data

```bash
polaris list local --exchange aster --asset BTCUSDT
```

After sync completes, Polaris stores the fetched snapshot files under its managed local root.

## Common Workflows

### Browse datasets interactively

Run bare `polaris` in a real terminal to open the remote dataset browser TUI. If stdin or stdout is not a terminal, Polaris falls back to plain CLI output.

```bash
polaris
```

### Search remote datasets from the CLI

```bash
polaris list --search btc --limit 25
polaris list --exchange aster --asset BTCUSDT --json
```

### Sync data for scripts and pipelines

```bash
polaris sync \
  --exchange aster \
  --asset BTCUSDT \
  --from 2026-06-01T00:00:00Z \
  --to 2026-06-02T00:00:00Z \
  --json \
  --concurrency 8
```

### Inspect what already exists locally

```bash
polaris list local --json
polaris list local --exchange aster --asset BTCUSDT --date 2026-06-01
```

### Reset local managed state

```bash
polaris reset
polaris reset --json
```

`reset` removes the local dataset state managed by Polaris under the configured root. It clears `data/`, `tmp/`, and `cache/`, but leaves the root directory and account credentials intact.

## CLI Overview

```text
polaris
├── account
│   ├── set-key
│   └── status
├── list
│   └── local
├── reset
├── sync
└── update
```

Top-level help:

```bash
polaris --help
```

Development entrypoints:

```bash
cargo run -- --help
cargo run --
cargo run -- list
cargo run -- list local
```

Build binaries directly:

```bash
cargo build
./target/debug/polaris

cargo build --release
./target/release/polaris
```

## Command Reference

### `polaris`

Opens the interactive remote dataset browser TUI in a real terminal. If no TUI can be rendered, it falls back to plain CLI output.

### `polaris account set-key`

Prompts for a Polaris API key and stores it in the OS credential store.

```bash
polaris account set-key
```

### `polaris account status`

Prints whether Polaris currently has a Polaris API key configured, and whether that credential came from `POLARIS_API_KEY` or the OS credential store.

```bash
polaris account status
```

### `polaris list`

Lists remote datasets available from Polaris.

```bash
polaris list --json

polaris list \
  --exchange aster \
  --asset BTCUSDT \
  --search btc \
  --limit 25
```

### `polaris list local`

Lists local snapshots under the configured root.

```bash
polaris list local --json
```

### `polaris sync`

Downloads missing snapshots for the requested dataset and time range.

After sync completes, the fetched snapshots are stored under `data/` within the configured local root.
```bash
polaris sync \
  --exchange aster \
  --asset BTCUSDT \
  --from 2026-06-01T00:00:00Z \
  --to 2026-06-02T00:00:00Z

polaris sync \
  --exchange aster \
  --asset BTCUSDT \
  --from 2026-06-01T00:00:00Z \
  --to 2026-06-02T00:00:00Z \
  --json \
  --concurrency 8
```

### `polaris reset`

Removes all local dataset state managed by Polaris under the configured root.

```bash
polaris reset
polaris reset --json
```

### `polaris update`

Downloads the release installer and updates the current CLI in place.

By default, `polaris update` tries to preserve the current install directory when running from an installed `polaris` binary. If it is run from a legacy `tick` binary, it preserves that legacy install directory. You can override that behavior explicitly.

```bash
polaris update
polaris update --version v0.2.0
polaris update --install-dir "$HOME/.local/bin"
```

## Configuration

### Environment variables

| Variable | Default | Purpose |
| --- | --- | --- |
| `POLARIS_BASE_URL` | `https://api.polaris.supply` | Base URL for Polaris API requests |
| `POLARIS_API_KEY` | unset | Optional bearer token for authenticated Polaris requests |
| `POLARIS_ROOT` | platform app-data directory | Override the local dataset root directory |
| `POLARIS_CONCURRENCY` | unset | Default sync concurrency when `--concurrency` is not provided |
| `POLARIS_TIMEOUT_SECS` | unset | Request timeout in seconds |

`POLARIS_API_KEY` takes precedence over the stored credential from `polaris account set-key`.

Example:

```bash
export POLARIS_BASE_URL="https://api.polaris.supply"
export POLARIS_ROOT="$HOME/.local/share/polaris-dev"
export POLARIS_CONCURRENCY="8"
export POLARIS_TIMEOUT_SECS="60"

polaris list
polaris list local
polaris sync --exchange aster --asset BTCUSDT --from 2026-06-01T00:00:00Z --to 2026-06-02T00:00:00Z
```

Compatibility notes:

- `TICK_ROOT`, `TICK_CONCURRENCY`, and `TICK_TIMEOUT_SECS` are still accepted
- If `POLARIS_API_KEY` is unset, Polaris also falls back to the legacy `tick` OS credential entry when needed

### JSON and automation

Use `--json` when you want structured output for scripts or agents.

Commands with `--json` support:

- `polaris list`
- `polaris list local`
- `polaris sync`
- `polaris reset`

Examples:

```bash
polaris list --json
polaris list local --json
polaris sync --exchange aster --asset BTCUSDT --from 2026-06-01T00:00:00Z --to 2026-06-02T00:00:00Z --json
polaris reset --json
```

## Data Layout

By default, Polaris stores data under the platform app-data directory unless `POLARIS_ROOT` is set.

Default paths:

- macOS: `~/Library/Application Support/polaris`
- Linux: `$XDG_DATA_HOME/polaris` or `~/.local/share/polaris`
- Windows: `%APPDATA%\\polaris`

Within that root, Polaris owns this layout:

```text
<root>/
  data/
  tmp/
  cache/
```

- `data/` stores snapshot files fetched from Polaris
- `tmp/` stores temporary sync state
- `cache/` stores local cache state used by the CLI

Compatibility note:

- If the new default Polaris root does not exist but the legacy `tick` data root does, Polaris keeps using the legacy root automatically

## Development

Useful local commands:

```bash
cargo run -- --help
cargo test
```

## Release

Polaris release assets are built by a single GitHub Actions workflow from the crate version in `Cargo.toml`.

Expected assets:

- `polaris-v{version}-x86_64-apple-darwin.tar.gz`
- `polaris-v{version}-aarch64-apple-darwin.tar.gz`
- `polaris-v{version}-x86_64-unknown-linux-gnu.tar.gz`
- `polaris-v{version}-aarch64-unknown-linux-gnu.tar.gz`
- `polaris-v{version}-checksums.txt`
