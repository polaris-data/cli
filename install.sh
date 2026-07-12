#!/usr/bin/env bash
set -euo pipefail

REPO="polaris-data/cli"
PROJECT="polaris"
DEFAULT_INSTALL_DIR="${HOME}/.polaris/bin"
LEGACY_INSTALL_DIR="${HOME}/.tick/bin"
DEFAULT_BRANCH="main"
MIN_NODE_MAJOR=22

INSTALL_DIR="$DEFAULT_INSTALL_DIR"
RUNTIME_DIR=""
REQUESTED_VERSION=""
SOURCE_DIR_OVERRIDE="${POLARIS_INSTALL_SOURCE_DIR:-}"

usage() {
  cat <<'EOF'
Usage: install.sh [--version <tag>] [--install-dir <path>] [--runtime-dir <path>]

Build and install the Polaris TypeScript CLI from source.

By default, installs the current main branch. Use --version to install a specific tag.

Options:
  --version <tag>      Build a specific Git tag, for example: v0.7.0
  --install-dir <dir>  Install directory for the polaris launcher
  --runtime-dir <dir>  Runtime directory for the built workspace
  -h, --help           Show this help text

Environment:
  POLARIS_INSTALL_SOURCE_DIR  Use a local repo checkout instead of downloading GitHub source
EOF
}

log() {
  printf 'polaris-install: %s\n' "$*" >&2
}

fail() {
  printf 'polaris-install: %s\n' "$*" >&2
  exit 1
}

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    fail "required command not found: $1"
  fi
}

normalize_version() {
  if [[ -z "$1" ]]; then
    return 0
  fi

  if [[ "$1" == v* ]]; then
    printf '%s\n' "$1"
  else
    printf 'v%s\n' "$1"
  fi
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --version)
        [[ $# -ge 2 ]] || fail "--version requires a value"
        REQUESTED_VERSION="$(normalize_version "$2")"
        shift 2
        ;;
      --install-dir)
        [[ $# -ge 2 ]] || fail "--install-dir requires a value"
        INSTALL_DIR="$2"
        shift 2
        ;;
      --runtime-dir)
        [[ $# -ge 2 ]] || fail "--runtime-dir requires a value"
        RUNTIME_DIR="$2"
        shift 2
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        fail "unknown argument: $1"
        ;;
    esac
  done
}

apply_default_install_dir() {
  if [[ "$INSTALL_DIR" == "$DEFAULT_INSTALL_DIR" && -d "$LEGACY_INSTALL_DIR" ]]; then
    INSTALL_DIR="$LEGACY_INSTALL_DIR"
    log "reusing legacy install directory ${LEGACY_INSTALL_DIR}"
  fi
}

default_runtime_dir() {
  if [[ -n "$RUNTIME_DIR" ]]; then
    printf '%s\n' "$RUNTIME_DIR"
    return 0
  fi

  case "$INSTALL_DIR" in
    "$DEFAULT_INSTALL_DIR"|"$DEFAULT_INSTALL_DIR"/*)
      printf '%s\n' "${HOME}/.polaris/lib/${PROJECT}"
      ;;
    "$LEGACY_INSTALL_DIR"|"$LEGACY_INSTALL_DIR"/*)
      printf '%s\n' "${HOME}/.tick/lib/${PROJECT}"
      ;;
    *)
      mkdir -p "$(dirname "$INSTALL_DIR")"
      printf '%s\n' "$(cd "$(dirname "$INSTALL_DIR")/.." && pwd)/lib/${PROJECT}"
      ;;
  esac
}

ensure_node_version() {
  require_command node

  local major
  major="$(node -p "process.versions.node.split('.')[0]")"
  [[ "$major" =~ ^[0-9]+$ ]] || fail "could not determine Node.js version"

  if (( major < MIN_NODE_MAJOR )); then
    fail "Node.js ${MIN_NODE_MAJOR}+ is required; found $(node -v)"
  fi
}

resolve_ref() {
  if [[ -n "$REQUESTED_VERSION" ]]; then
    printf '%s\n' "$REQUESTED_VERSION"
    return 0
  fi

  printf '%s\n' "$DEFAULT_BRANCH"
}

download_source_archive() {
  local ref="$1"
  local archive_path="$2"
  local archive_url

  if [[ "$ref" == "$DEFAULT_BRANCH" ]]; then
    archive_url="https://codeload.github.com/${REPO}/tar.gz/refs/heads/${ref}"
  else
    archive_url="https://codeload.github.com/${REPO}/tar.gz/refs/tags/${ref}"
  fi

  log "downloading source for ${ref}"
  curl -fsSL "$archive_url" -o "$archive_path"
}

stage_source_tree() {
  local ref="$1"
  local work_dir="$2"
  local source_dir="$3"

  if [[ -n "$source_dir" ]]; then
    [[ -d "$source_dir" ]] || fail "POLARIS_INSTALL_SOURCE_DIR does not exist: ${source_dir}"
    local staged_dir="${work_dir}/source"
    mkdir -p "$staged_dir"
    tar \
      -C "$source_dir" \
      --exclude='.context' \
      --exclude='.git' \
      --exclude='node_modules' \
      --exclude='package-lock.json' \
      --exclude='packages/*/dist' \
      --exclude='packages/*/node_modules' \
      -cf - . | tar -C "$staged_dir" -xf -
    printf '%s\n' "$staged_dir"
    return 0
  fi

  local archive_path="${work_dir}/${PROJECT}-${ref}.tar.gz"
  download_source_archive "$ref" "$archive_path"
  tar -xzf "$archive_path" -C "$work_dir"
  find "$work_dir" -mindepth 1 -maxdepth 1 -type d | head -n 1
}

resolve_pnpm() {
  if command -v pnpm >/dev/null 2>&1; then
    PNPM_CMD=(pnpm)
    return 0
  fi

  if command -v corepack >/dev/null 2>&1; then
    PNPM_CMD=(corepack pnpm)
    return 0
  fi

  fail "pnpm not found and corepack is unavailable"
}

build_workspace() {
  local source_dir="$1"
  resolve_pnpm

  log "installing workspace dependencies"
  if ! (cd "$source_dir" && CI=1 "${PNPM_CMD[@]}" install --frozen-lockfile); then
    log "frozen lockfile install failed; retrying without --frozen-lockfile"
    (cd "$source_dir" && CI=1 "${PNPM_CMD[@]}" install --no-frozen-lockfile)
  fi

  log "building TypeScript workspace"
  (cd "$source_dir" && "${PNPM_CMD[@]}" build:ts)

  [[ -f "${source_dir}/packages/cli/dist/cli/src/index.js" ]] \
    || fail "built CLI entrypoint not found at packages/cli/dist/cli/src/index.js"
}

install_runtime_tree() {
  local source_dir="$1"
  local resolved_runtime_dir="$2"
  local runtime_parent runtime_tmp

  runtime_parent="$(dirname "$resolved_runtime_dir")"
  runtime_tmp="${resolved_runtime_dir}.tmp.$$"

  mkdir -p "$runtime_parent"
  rm -rf "$runtime_tmp"
  mkdir -p "$runtime_tmp"
  tar -C "$source_dir" -cf - . | tar -C "$runtime_tmp" -xf -

  rm -rf "$resolved_runtime_dir"
  mv "$runtime_tmp" "$resolved_runtime_dir"
}

install_launcher() {
  local resolved_runtime_dir="$1"
  local launcher_tmp launcher_path cli_entry cli_entry_escaped

  cli_entry="${resolved_runtime_dir}/packages/cli/dist/cli/src/index.js"
  [[ -f "$cli_entry" ]] || fail "installed CLI entrypoint not found: ${cli_entry}"

  mkdir -p "$INSTALL_DIR"
  launcher_path="${INSTALL_DIR}/${PROJECT}"
  launcher_tmp="${launcher_path}.tmp.$$"
  cli_entry_escaped="$(printf '%q' "$cli_entry")"

  cat >"$launcher_tmp" <<EOF
#!/usr/bin/env bash
set -euo pipefail
exec node ${cli_entry_escaped} "\$@"
EOF

  chmod 0755 "$launcher_tmp"
  mv "$launcher_tmp" "$launcher_path"
  ln -sfn "${PROJECT}" "${INSTALL_DIR}/tick"
}

detect_profile() {
  case "${SHELL:-}" in
    */zsh)
      printf '%s\n' "${ZDOTDIR:-$HOME}/.zshenv:zsh"
      ;;
    */bash)
      printf '%s\n' "${HOME}/.bashrc:bash"
      ;;
    */fish)
      printf '%s\n' "${HOME}/.config/fish/config.fish:fish"
      ;;
    */ash)
      printf '%s\n' "${HOME}/.profile:ash"
      ;;
    *)
      printf '%s\n' "${HOME}/.profile:sh"
      ;;
  esac
}

ensure_path() {
  local profile_info profile shell_kind line legacy_line

  case ":${PATH:-}:" in
    *":${INSTALL_DIR}:"*)
      return 0
      ;;
  esac

  profile_info="$(detect_profile)"
  profile="${profile_info%%:*}"
  shell_kind="${profile_info##*:}"

  mkdir -p "$(dirname "$profile")"
  touch "$profile"

  if [[ "$shell_kind" == fish ]]; then
    line="fish_add_path -a \"$INSTALL_DIR\""
    legacy_line=""
  else
    line="export PATH=\"$INSTALL_DIR:\$PATH\""
    legacy_line="export PATH=\"\$PATH:$INSTALL_DIR\""
  fi

  if grep -Fxqs "$line" "$profile"; then
    return 0
  fi
  if [[ -n "$legacy_line" ]] && grep -Fxqs "$legacy_line" "$profile"; then
    return 0
  fi

  printf '\n%s\n' "$line" >>"$profile"
  log "added ${INSTALL_DIR} to PATH in ${profile}"
}

main() {
  local ref work_dir source_dir resolved_runtime_dir version_label

  parse_args "$@"
  require_command curl
  require_command tar
  apply_default_install_dir
  ensure_node_version

  ref="$(resolve_ref)"
  version_label="$ref"
  resolved_runtime_dir="$(default_runtime_dir)"
  work_dir="$(mktemp -d)"
  trap "rm -rf '$work_dir'" EXIT

  source_dir="$(stage_source_tree "$ref" "$work_dir" "$SOURCE_DIR_OVERRIDE")"
  build_workspace "$source_dir"
  install_runtime_tree "$source_dir" "$resolved_runtime_dir"
  install_launcher "$resolved_runtime_dir"
  ensure_path

  log "installed ${PROJECT} (${version_label}) to ${INSTALL_DIR}/${PROJECT}"
  log "runtime installed at ${resolved_runtime_dir}"
  log "run '${PROJECT} --help' to get started"
}

main "$@"
