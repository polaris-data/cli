# polaris

`polaris` is a Rust CLI for syncing Polaris standardized event snapshot files to a local canonical dataset tree.

It is intentionally narrow in v1:
- bare `polaris` opens the remote dataset browser TUI in a real terminal
- `list` prints remote exchange and asset datasets in plain CLI output
- `list local` shows the local snapshots currently present under the managed dataset root
- `reset` clears all local dataset state managed by `polaris`
- `sync` downloads missing snapshot files for a requested remote time range

## Requirements

- Rust toolchain with `cargo`
- network access to the Polaris API
- optional: a Polaris API key if you want authenticated Polaris access

## Install And Update

Install the latest GitHub Release:

```bash
curl -fsSL https://raw.githubusercontent.com/polaris-data/cli/main/install.sh | bash
```

Re-run the same command later to update to the newest release.

Or, once installed, update in place from the CLI:

```bash
polaris update
```

Install a pinned version:

```bash
curl -fsSL https://raw.githubusercontent.com/polaris-data/cli/main/install.sh | bash -s -- --version v0.1.0
```

Install into a custom directory:

```bash
curl -fsSL https://raw.githubusercontent.com/polaris-data/cli/main/install.sh | bash -s -- --install-dir "$HOME/.local/bin"
```

Default install location for new installs:

```text
~/.polaris/bin/polaris
```

Compatibility behavior:
- if `~/.tick/bin` already exists, the installer reuses that legacy install directory
- the installer also creates a `tick` symlink pointing at `polaris` for migration

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

## Build And Run

Development help:

```bash
cargo run -- --help
```

Run the TUI in development:

```bash
cargo run --
```

Bare `polaris` opens the interactive TUI in a real terminal. If stdout/stdin are not terminals, it falls back to plain output.

Run remote list output in development:

```bash
cargo run -- list
```

Run `list local` in development:

```bash
cargo run -- list local
```

Build debug and release binaries directly:

```bash
cargo build
./target/debug/polaris

cargo build --release
./target/release/polaris
```

## Commands

```text
polaris --help

polaris
polaris account set-key
polaris account status
polaris list [--exchange <EXCHANGE>] [--asset <ASSET>] [--search <QUERY>] [--limit <N>] [--json]
polaris list local [--exchange <EXCHANGE>] [--asset <ASSET>] [--date <YYYY-MM-DD>] [--json]
polaris reset [--json]
polaris sync --exchange <EXCHANGE> --asset <ASSET> --from <FROM> --to <TO> [--json] [--concurrency <N>]
polaris update [--version <TAG>] [--install-dir <DIR>]
```

### `polaris`

Opens the interactive remote dataset browser TUI when running in a real terminal. If no TUI can be rendered, it falls back to plain CLI output.

### `polaris list`

Lists remote datasets available from Polaris in plain CLI output.

Examples:

```bash
./target/debug/polaris list --json

./target/debug/polaris list \
  --exchange aster \
  --asset BTCUSDT \
  --search btc \
  --limit 25
```

### `polaris update`

Downloads the release installer and updates the current CLI in place.

By default, `polaris update` tries to preserve the current install directory when running from an installed `polaris` binary. If it is run from a legacy `tick` binary, it preserves that legacy install directory. You can override that behavior explicitly.

Examples:

```bash
polaris update
polaris update --version v0.1.0
polaris update --install-dir "$HOME/.local/bin"
```

### `polaris account set-key`

Prompts for a Polaris API key and stores it in the OS credential store.

Example:

```bash
./target/debug/polaris account set-key
```

### `polaris account status`

Prints whether `polaris` currently has a Polaris API key configured, and whether that credential came from `POLARIS_API_KEY` or the OS credential store.

Example:

```bash
./target/debug/polaris account status
```

### `polaris list local`

Lists local snapshots under the configured root.

Example:

```bash
./target/debug/polaris list local --json
```

### `polaris reset`

Removes all local dataset state managed by `polaris` under the configured root. This clears `data/`, `daily/`, `tmp/`, and `cache/`, but leaves the root directory and account credentials intact.

Examples:

```bash
./target/debug/polaris reset
./target/debug/polaris reset --json
```

### `polaris sync`

Downloads missing snapshots for the requested dataset and time range.

After sync completes, `polaris` also automatically materializes full-day local files under `daily/` for any UTC day in the effective sync range that is fully present locally. When Polaris serves a day-level standardized snapshot directly, that file is reused as the day artifact.

Examples:

```bash
./target/debug/polaris sync \
  --exchange aster \
  --asset BTCUSDT \
  --from 2026-06-01T00:00:00Z \
  --to 2026-06-02T00:00:00Z

./target/debug/polaris sync \
  --exchange aster \
  --asset BTCUSDT \
  --from 2026-06-01T00:00:00Z \
  --to 2026-06-02T00:00:00Z \
  --json \
  --concurrency 8
```

## Environment

- `POLARIS_BASE_URL`
  - default: `https://api.polaris.supply`
- `POLARIS_API_KEY`
  - optional bearer token for authenticated Polaris requests
  - takes precedence over the stored credential from `polaris account set-key`
- `POLARIS_ROOT`
  - overrides the local dataset root directory
- `POLARIS_CONCURRENCY`
  - default sync concurrency when `--concurrency` is not provided
- `POLARIS_TIMEOUT_SECS`
  - request timeout in seconds

Legacy compatibility:
- `TICK_ROOT`, `TICK_CONCURRENCY`, and `TICK_TIMEOUT_SECS` are still accepted
- if `POLARIS_API_KEY` is unset, `polaris` also falls back to the legacy `tick` OS credential entry when needed
- if the new default data root does not exist but the legacy `tick` data root does, `polaris` keeps using the legacy root automatically

Example:

```bash
export POLARIS_BASE_URL="https://api.polaris.supply"
export POLARIS_ROOT="$HOME/.local/share/polaris-dev"
export POLARIS_CONCURRENCY="8"
export POLARIS_TIMEOUT_SECS="60"

./target/debug/polaris
./target/debug/polaris list
./target/debug/polaris list local
./target/debug/polaris sync --exchange aster --asset BTCUSDT --from 2026-06-01T00:00:00Z --to 2026-06-02T00:00:00Z
```

## Local Layout

By default `polaris` stores data under the platform app-data directory unless `POLARIS_ROOT` is set.

New default paths:
- macOS: `~/Library/Application Support/polaris`
- Linux: `$XDG_DATA_HOME/polaris` or `~/.local/share/polaris`
- Windows: `%APPDATA%\polaris`

Within that root, `polaris` owns this layout:

```text
<root>/
  data/
  daily/
  tmp/
  cache/
```

If a legacy `tick` data root already exists and the new `polaris` root does not, the CLI reuses the legacy root during migration.

## Release

`polaris` release assets are built by a single GitHub Actions workflow from the crate version in `Cargo.toml`.

Expected assets:
- `polaris-v{version}-x86_64-apple-darwin.tar.gz`
- `polaris-v{version}-aarch64-apple-darwin.tar.gz`
- `polaris-v{version}-x86_64-unknown-linux-gnu.tar.gz`
- `polaris-v{version}-aarch64-unknown-linux-gnu.tar.gz`
- `polaris-v{version}-checksums.txt`
