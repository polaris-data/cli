#!/usr/bin/env bash
set -euo pipefail

REPO="polaris-data/cli"
PROJECT="polaris"
DEFAULT_INSTALL_DIR="${HOME}/.polaris/bin"
LEGACY_INSTALL_DIR="${HOME}/.tick/bin"

INSTALL_DIR="$DEFAULT_INSTALL_DIR"
REQUESTED_VERSION=""

usage() {
  cat <<'EOF'
Usage: install.sh [--version <tag>] [--install-dir <path>]

Install or update the latest Polaris CLI release from GitHub Releases.

Options:
  --version <tag>      Install a specific release tag, for example: v0.1.0
  --install-dir <dir>  Install directory for the polaris binary
  -h, --help           Show this help text
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

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "${os}:${arch}" in
    Darwin:x86_64)
      printf '%s\n' 'x86_64-apple-darwin'
      ;;
    Darwin:arm64)
      printf '%s\n' 'aarch64-apple-darwin'
      ;;
    Linux:x86_64)
      printf '%s\n' 'x86_64-unknown-linux-gnu'
      ;;
    Linux:aarch64|Linux:arm64)
      printf '%s\n' 'aarch64-unknown-linux-gnu'
      ;;
    *)
      fail "unsupported platform: ${os} ${arch}"
      ;;
  esac
}

resolve_latest_version() {
  local response
  response="$(
    curl -fsSL \
      -H 'Accept: application/vnd.github+json' \
      -H 'X-GitHub-Api-Version: 2022-11-28' \
      "https://api.github.com/repos/${REPO}/releases/latest"
  )"

  printf '%s' "$response" \
    | tr -d '\n' \
    | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p'
}

resolve_version() {
  if [[ -n "$REQUESTED_VERSION" ]]; then
    printf '%s\n' "$REQUESTED_VERSION"
    return 0
  fi

  log "resolving latest release"
  local latest
  latest="$(resolve_latest_version)"

  if [[ -z "$latest" ]]; then
    fail "could not resolve the latest release tag from GitHub"
  fi

  printf '%s\n' "$latest"
}

download_release_assets() {
  local version="$1"
  local target="$2"
  local work_dir="$3"
  local archive_name checksums_name base_url

  archive_name="${PROJECT}-${version}-${target}.tar.gz"
  checksums_name="${PROJECT}-${version}-checksums.txt"
  base_url="https://github.com/${REPO}/releases/download/${version}"

  log "downloading ${archive_name}"
  curl -fsSL "${base_url}/${archive_name}" -o "${work_dir}/${archive_name}"

  log "downloading ${checksums_name}"
  curl -fsSL "${base_url}/${checksums_name}" -o "${work_dir}/${checksums_name}"

  printf '%s\n' "${work_dir}/${archive_name}"
}

verify_checksum() {
  local archive_path="$1"
  local checksums_path="$2"
  local archive_name expected actual

  archive_name="$(basename "$archive_path")"
  expected="$(awk -v file="$archive_name" '$2 == file { print $1 }' "$checksums_path")"

  if [[ -z "$expected" ]]; then
    fail "checksum for ${archive_name} not found in $(basename "$checksums_path")"
  fi

  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$archive_path" | awk '{ print $1 }')"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$archive_path" | awk '{ print $1 }')"
  else
    log "warning: sha256sum/shasum not found; skipping checksum verification"
    return 0
  fi

  if [[ "$actual" != "$expected" ]]; then
    fail "checksum mismatch for ${archive_name}"
  fi

  log "verified checksum for ${archive_name}"
}

install_binary() {
  local archive_path="$1"
  local work_dir="$2"
  local extracted_dir install_tmp

  extracted_dir="${work_dir}/extracted"
  mkdir -p "$extracted_dir"
  tar -xzf "$archive_path" -C "$extracted_dir" "${PROJECT}"

  [[ -f "${extracted_dir}/${PROJECT}" ]] || fail "archive did not contain ${PROJECT}"

  mkdir -p "$INSTALL_DIR"
  install_tmp="${INSTALL_DIR}/.${PROJECT}.tmp.$$"
  cp "${extracted_dir}/${PROJECT}" "$install_tmp"
  chmod 0755 "$install_tmp"
  mv "$install_tmp" "${INSTALL_DIR}/${PROJECT}"
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
  local target version work_dir archive_path checksums_path installed_version

  require_command curl
  require_command tar
  parse_args "$@"
  apply_default_install_dir

  target="$(detect_target)"
  version="$(resolve_version)"
  work_dir="$(mktemp -d)"
  trap "rm -rf '$work_dir'" EXIT

  archive_path="$(download_release_assets "$version" "$target" "$work_dir")"
  checksums_path="${work_dir}/${PROJECT}-${version}-checksums.txt"

  verify_checksum "$archive_path" "$checksums_path"
  install_binary "$archive_path" "$work_dir"
  ensure_path

  installed_version="$("${INSTALL_DIR}/${PROJECT}" --version 2>/dev/null || true)"

  log "installed ${PROJECT} to ${INSTALL_DIR}/${PROJECT}"
  if [[ -n "$installed_version" ]]; then
    printf '%s\n' "$installed_version"
  fi

  case ":${PATH:-}:" in
    *":${INSTALL_DIR}:"*)
      ;;
    *)
      log "open a new shell or source your profile to use ${PROJECT} from PATH"
      ;;
  esac
}

main "$@"
