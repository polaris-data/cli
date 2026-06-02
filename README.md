# tick

`tick` is a Rust CLI for syncing Polaris standardized event snapshot files to a local canonical dataset tree.

It is intentionally narrow in v1:
- bare `tick` opens the remote dataset browser TUI in a real terminal.
- `list` prints remote exchange and asset datasets in plain CLI output.
- `list local` shows the local snapshots currently present under the managed dataset root.
- `reset` clears all local dataset state managed by `tick`.
- `sync` downloads missing snapshot files for a requested remote time range.

## Requirements

- Rust toolchain with `cargo`
- Network access to the Polaris API
- Optional: a Polaris API key if you want to use authenticated Polaris access

## Install And Update

Install the latest GitHub Release:

```bash
curl -fsSL https://raw.githubusercontent.com/spectrum-ec/tick/main/install.sh | bash
```

Re-run the same command later to update to the newest release.

Install a pinned version:

```bash
curl -fsSL https://raw.githubusercontent.com/spectrum-ec/tick/main/install.sh | bash -s -- --version v0.1.0
```

Install into a custom directory:

```bash
curl -fsSL https://raw.githubusercontent.com/spectrum-ec/tick/main/install.sh | bash -s -- --install-dir "$HOME/.local/bin"
```

Default install location:

```text
~/.tick/bin/tick
```

The installer adds `~/.tick/bin` to your shell profile if needed:
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

Bare `tick` opens the interactive TUI in a real terminal. If stdout/stdin are not terminals, it falls back to plain output.

Run remote list output in development:

```bash
cargo run -- list
```

Run `list local` in development:

```bash
cargo run -- list local
```

Run `sync` in development:

```bash
cargo run -- sync \
  --exchange aster \
  --asset BTCUSDT \
  --from 2026-06-01T00:00:00Z \
  --to 2026-06-02T00:00:00Z
```

Build a development binary:

```bash
cargo build
```

The binary will be available at:

```bash
./target/debug/tick
```

Build an optimized release binary:

```bash
cargo build --release
```

The release binary will be available at:

```bash
./target/release/tick
```

Use the compiled binary directly:

```bash
./target/release/tick --help
./target/release/tick list --help
./target/release/tick list local --help
./target/release/tick reset --help
./target/release/tick sync --help
```

## Commands

Top-level help:

```bash
tick --help
```

Current command surface:

```text
tick
tick account set-key
tick account status
tick list [--exchange <EXCHANGE>] [--asset <ASSET>] [--search <QUERY>] [--limit <N>] [--json]
tick list local [--exchange <EXCHANGE>] [--asset <ASSET>] [--date <YYYY-MM-DD>] [--json]
tick reset [--json]
tick sync --exchange <EXCHANGE> --asset <ASSET> --from <FROM> --to <TO> [--json] [--concurrency <N>]
```

### `tick`

Opens the interactive remote dataset browser TUI.

Keys:
- type to search
- `Backspace` to edit search
- `Up` / `Down` to move selection
- `Enter` on a dataset to open its day-coverage view
- in dataset view: `Enter` syncs the selected UTC day
- in dataset view: `Left` / `Right` moves by day, `Up` / `Down` by week
- `q` or `Esc` to quit or back out

### `list`

Lists remote datasets available from Polaris in plain CLI output.

Each result is an `exchange:asset` pair with remote coverage timestamps.

Optional filters:
- `--exchange <EXCHANGE>`
- `--asset <ASSET>`
- `--search <QUERY>`
- `--limit <N>`

Example:

```bash
./target/debug/tick list --json
```

Filtered example:

```bash
./target/debug/tick list \
  --exchange aster \
  --search btc
```

### `account set-key`

Prompts for a Polaris API key and stores it in the OS credential store. Stored credentials are used automatically when `POLARIS_API_KEY` is not set.

Example:

```bash
./target/debug/tick account set-key
```

### `account status`

Prints whether `tick` currently has a Polaris API key configured, and whether that credential came from `POLARIS_API_KEY` or the OS credential store.

Example:

```bash
./target/debug/tick account status
```

### `list local`

Lists local snapshots already stored under `data/` in the managed root. Snapshot metadata is deduced from the file path and filename pattern when possible, including daily snapshot files where the UTC date is encoded in the filename instead of a directory segment.

Optional filters:
- `--exchange <EXCHANGE>`
- `--asset <ASSET>`
- `--date <YYYY-MM-DD>`

Example:

```bash
./target/debug/tick list local --json
```

### `reset`

Removes all local dataset state managed by `tick` under the configured root. This clears `data/`, `daily/`, `tmp/`, and `cache/`, but leaves the root directory and account credentials intact.

Example:

```bash
./target/debug/tick reset
```

JSON output:

```bash
./target/debug/tick reset --json
```

Filtered example:

```bash
./target/debug/tick list local \
  --exchange aster \
  --asset BTCUSDT \
  --date 2026-06-01
```

### `sync`

Fetches the remote standardized snapshot catalog for the requested range, compares it to the local dataset tree, then downloads only the missing snapshots.

After sync completes, `tick` also automatically materializes full-day local files under `daily/` for any UTC day in the effective sync range that is fully present locally. When Polaris serves a day-level standardized snapshot directly, that file is reused as the day artifact.

Example:

```bash
./target/debug/tick sync \
  --exchange aster \
  --asset BTCUSDT \
  --from 2026-06-01T00:00:00Z \
  --to 2026-06-02T00:00:00Z \
  --concurrency 4
```

JSON output:

```bash
./target/debug/tick sync \
  --exchange aster \
  --asset BTCUSDT \
  --from 2026-06-01T00:00:00Z \
  --to 2026-06-02T00:00:00Z \
  --json
```

## Environment Variables

- `POLARIS_BASE_URL`
  - Default: `https://api.polaris.supply`
- `POLARIS_API_KEY`
  - Optional bearer token for authenticated Polaris requests
  - Takes precedence over the stored credential from `tick account set-key`
- `TICK_ROOT`
  - Optional override for the local dataset root
- `TICK_CONCURRENCY`
  - Default: `4`
- `TICK_TIMEOUT_SECS`
  - Default: `60`

If `POLARIS_API_KEY` is unset, `tick` falls back to the Polaris API key stored in the OS credential store.

Example:

```bash
export POLARIS_BASE_URL="https://api.polaris.supply"
export POLARIS_API_KEY="your_key_here"
export TICK_ROOT="$HOME/.local/share/tick-dev"
export TICK_CONCURRENCY="8"
export TICK_TIMEOUT_SECS="60"
```

Then run:

```bash
./target/debug/tick
./target/debug/tick list
./target/debug/tick list local
./target/debug/tick sync --exchange aster --asset BTCUSDT --from 2026-06-01T00:00:00Z --to 2026-06-02T00:00:00Z
```

## Local Storage Layout

By default `tick` stores data under the platform app-data directory unless `TICK_ROOT` is set.

Examples:
- macOS: `~/Library/Application Support/tick`
- Linux: `$XDG_DATA_HOME/tick` or `~/.local/share/tick`
- Windows: `%APPDATA%\tick`

Within that root, `tick` owns this layout:

- `data/<remote-key>`
- `daily/<exchange>/<asset>/<date>.jsonl.zst`
- `tmp/<sha256(remote-key)>.part`
- `locks/sync.lock`
- `cache/catalog/<exchange>/<asset>.json`

Example snapshot path:

```text
data/bronze/aster/BTCUSDT/2026-06-01/aster_BTCUSDT_s20260601T000000Z_e20260601T000959Z.jsonl.zst
```

Example day-level snapshot path:

```text
data/events/aster/BTCUSDT/aster_BTCUSDT_2026-06-01.jsonl.zst
```

Example daily materialized path:

```text
daily/aster/BTCUSDT/2026-06-01.jsonl.zst
```

## Exit Codes

- `0`: success
- `1`: runtime, config, lock, or network error
- `2`: dataset unavailable or requested range has no overlap with remote coverage

## Test

Run formatting and tests locally:

```bash
cargo fmt
cargo test
```

## Release Process

`tick` release assets are built from Git tags by GitHub Actions.

Maintainership flow:

1. Update the crate version in `Cargo.toml`.
2. Commit the release change.
3. Create a matching tag, for example `git tag v0.1.0`.
4. Push the commit and tag: `git push origin main --tags`.
5. GitHub Actions publishes:
   - `tick-v{version}-x86_64-apple-darwin.tar.gz`
   - `tick-v{version}-aarch64-apple-darwin.tar.gz`
   - `tick-v{version}-x86_64-unknown-linux-gnu.tar.gz`
   - `tick-v{version}-aarch64-unknown-linux-gnu.tar.gz`
   - `tick-v{version}-checksums.txt`

The release workflow fails if the git tag does not match the version declared in `Cargo.toml`.
