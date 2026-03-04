#!/bin/sh
set -eu

REPO="aetherwing-io/slipstream"
INSTALL_DIR="${SLIPSTREAM_INSTALL_DIR:-$HOME/.local/bin}"

main() {
  os=$(uname -s)
  arch=$(uname -m)

  case "$os" in
    Darwin) os_target="apple-darwin" ;;
    Linux)
      if ldd --version 2>&1 | grep -qi musl; then
        os_target="unknown-linux-musl"
      else
        os_target="unknown-linux-gnu"
      fi
      ;;
    *)      err "Unsupported OS: $os (slipstream supports macOS and Linux)" ;;
  esac

  case "$arch" in
    arm64|aarch64) arch_target="aarch64" ;;
    x86_64)        arch_target="x86_64" ;;
    *)             err "Unsupported architecture: $arch" ;;
  esac

  target="${arch_target}-${os_target}"

  printf "  detecting platform... %s\n" "$target"

  # Fetch latest release tag
  tag=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | head -1 | cut -d'"' -f4)

  if [ -z "$tag" ]; then
    err "Could not determine latest release"
  fi

  printf "  latest release... %s\n" "$tag"

  url="https://github.com/${REPO}/releases/download/${tag}/slipstream-${tag}-${target}.tar.gz"

  # Download and extract
  tmpdir=$(mktemp -d)
  trap 'rm -rf "$tmpdir"' EXIT

  printf "  downloading... "
  curl -fsSL "$url" -o "$tmpdir/slipstream.tar.gz"
  printf "ok\n"

  tar xzf "$tmpdir/slipstream.tar.gz" -C "$tmpdir"

  # Install both binaries
  mkdir -p "$INSTALL_DIR"
  mv "$tmpdir/slipstream" "$INSTALL_DIR/slipstream"
  mv "$tmpdir/slipstream-mcp" "$INSTALL_DIR/slipstream-mcp"
  chmod +x "$INSTALL_DIR/slipstream" "$INSTALL_DIR/slipstream-mcp"

  printf "  installed to %s\n" "$INSTALL_DIR"

  # Verify
  if "$INSTALL_DIR/slipstream-mcp" --version >/dev/null 2>&1; then
    version=$("$INSTALL_DIR/slipstream-mcp" --version 2>&1)
    printf "\n  + %s\n" "$version"
  else
    printf "\n  + binaries installed (could not verify version)\n"
  fi

  # PATH check
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
      printf "\n  note: add %s to your PATH:\n" "$INSTALL_DIR"
      printf "    export PATH=\"%s:\$PATH\"\n" "$INSTALL_DIR"
      ;;
  esac

  printf "\n  next: add to Claude Code:\n"
  printf "    claude mcp add slipstream -- slipstream-mcp\n"
  printf "\n"
}

err() {
  printf "  ! %s\n" "$1" >&2
  exit 1
}

main
