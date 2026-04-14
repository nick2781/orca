#!/usr/bin/env bash
set -euo pipefail

REPO="Nick2781/orca"
INSTALL_DIR="$HOME/.orca/bin"
BINARY="orca"

# --- Helpers ---------------------------------------------------------------

info()  { printf '\033[1;34m==> %s\033[0m\n' "$*"; }
err()   { printf '\033[1;31mError: %s\033[0m\n' "$*" >&2; exit 1; }

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || err "'$1' is required but not found"
}

# --- Detect OS & arch ------------------------------------------------------

detect_os() {
  case "$(uname -s)" in
    Linux*)  echo "linux"  ;;
    Darwin*) echo "darwin" ;;
    *)       err "Unsupported OS: $(uname -s)" ;;
  esac
}

detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64)       echo "x86_64"  ;;
    arm64|aarch64)       echo "aarch64" ;;
    *)                   err "Unsupported architecture: $(uname -m)" ;;
  esac
}

# --- Map to Rust target triple ---------------------------------------------

rust_target() {
  local os="$1" arch="$2"
  case "${os}-${arch}" in
    linux-x86_64)   echo "x86_64-unknown-linux-gnu"   ;;
    linux-aarch64)  echo "aarch64-unknown-linux-gnu"   ;;
    darwin-x86_64)  echo "x86_64-apple-darwin"         ;;
    darwin-aarch64) echo "aarch64-apple-darwin"         ;;
    *)              err "No pre-built binary for ${os}/${arch}" ;;
  esac
}

# --- Fetch latest release tag ----------------------------------------------

latest_tag() {
  need_cmd curl
  local response=""
  response="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null || true)"
  [ -n "$response" ] || return 0
  printf '%s\n' "$response" \
    | grep '"tag_name"' \
    | head -1 \
    | sed -E 's/.*"tag_name":\s*"([^"]+)".*/\1/'
}

install_binary() {
  local binary_path="$1"
  mkdir -p "$INSTALL_DIR"
  mv "$binary_path" "${INSTALL_DIR}/${BINARY}"
  chmod +x "${INSTALL_DIR}/${BINARY}"
  info "Installed to ${INSTALL_DIR}/${BINARY}"
}

install_from_release() {
  local tag="$1" target="$2" tmpdir="$3"
  local url="https://github.com/${REPO}/releases/download/${tag}/orca-${target}.tar.gz"

  info "Latest release: ${tag}"
  info "Downloading ${url}"

  curl -fsSL "$url" -o "${tmpdir}/orca.tar.gz"
  tar -xzf "${tmpdir}/orca.tar.gz" -C "$tmpdir"
  install_binary "${tmpdir}/orca"
}

install_from_main() {
  local tmpdir="$1"
  local archive="${tmpdir}/orca-main.tar.gz"
  local source_dir="${tmpdir}/orca-main"

  need_cmd cargo

  info "No GitHub release found. Building from the current main branch."
  curl -fsSL "https://github.com/${REPO}/archive/refs/heads/main.tar.gz" -o "$archive"
  tar -xzf "$archive" -C "$tmpdir"
  cargo build --release --manifest-path "${source_dir}/Cargo.toml"
  install_binary "${source_dir}/target/release/${BINARY}"
}

# --- Main ------------------------------------------------------------------

main() {
  need_cmd curl
  need_cmd tar

  local os arch target tag

  os="$(detect_os)"
  arch="$(detect_arch)"
  target="$(rust_target "$os" "$arch")"

  info "Detected platform: ${os}/${arch} (${target})"

  tag="$(latest_tag)"
  [ -n "$tag" ] || err "Could not determine latest release"
  info "Latest release: ${tag}"

  url="https://github.com/${REPO}/releases/download/${tag}/orca-${target}.tar.gz"
  info "Downloading ${url}"

  local tmpdir
  tmpdir="$(mktemp -d)"
  trap 'rm -rf "$tmpdir"' EXIT

  if [ -n "$tag" ]; then
    install_from_release "$tag" "$target" "$tmpdir"
  else
    install_from_main "$tmpdir"
  fi

  # --- Add to PATH if needed ------------------------------------------------

  if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
    local shell_rc=""
    case "$(basename "${SHELL:-/bin/bash}")" in
      zsh)  shell_rc="$HOME/.zshrc"  ;;
      bash) shell_rc="$HOME/.bashrc" ;;
    esac

    if [ -n "$shell_rc" ]; then
      if ! grep -q "$INSTALL_DIR" "$shell_rc" 2>/dev/null; then
        printf '\n# Orca\nexport PATH="%s:$PATH"\n' "$INSTALL_DIR" >> "$shell_rc"
        info "Added ${INSTALL_DIR} to PATH in ${shell_rc}"
      fi
    fi
  fi

  # --- Quick start -----------------------------------------------------------

  printf '\n'
  info "Installation complete!"
  printf '\n'
  printf '  Quick start:\n'
  printf '\n'
  printf '    # Reload your shell (or open a new terminal)\n'
  printf '    source %s\n' "${shell_rc:-"~/.bashrc"}"
  printf '\n'
  printf '    # Initialize in your project\n'
  printf '    cd my-project\n'
  printf '    orca init\n'
  printf '\n'
  printf '    # Set up Claude Code MCP integration\n'
  printf '    orca setup mcp\n'
  printf '\n'
  printf '    # Start the daemon\n'
  printf '    orca daemon start\n'
  printf '\n'
}

main "$@"
