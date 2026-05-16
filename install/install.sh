#!/usr/bin/env sh
# sidebar installer — fetch a prebuilt binary from GitHub Releases.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/cmflannery/sidebar/main/install/install.sh | sh
#
# Env overrides:
#   INSTALL_VERSION=v0.4.0       pin a specific release (default: latest)
#   BIN_DIR=/path/to/dir         override install destination
#
# By default installs to /usr/local/bin if writable, else $HOME/.local/bin.

set -eu

REPO="cmflannery/sidebar"
INSTALL_VERSION="${INSTALL_VERSION:-latest}"

# --- OS / arch detection ----------------------------------------------------
uname_s="$(uname -s)"
uname_m="$(uname -m)"
case "$uname_s" in
  Darwin) os="apple-darwin" ;;
  Linux)  os="unknown-linux-gnu" ;;
  *) printf 'unsupported OS: %s\n' "$uname_s" >&2; exit 1 ;;
esac
case "$uname_m" in
  arm64|aarch64) arch="aarch64" ;;
  x86_64|amd64)  arch="x86_64" ;;
  *) printf 'unsupported arch: %s\n' "$uname_m" >&2; exit 1 ;;
esac
# Linux on arm64 isn't a release target yet — say so cleanly.
if [ "$uname_s" = "Linux" ] && [ "$arch" = "aarch64" ]; then
  cat >&2 <<EOF
No prebuilt binary for Linux aarch64. Build from source:
  cargo install --git https://github.com/${REPO}
EOF
  exit 1
fi
target="${arch}-${os}"

# --- destination ------------------------------------------------------------
if [ -n "${BIN_DIR:-}" ]; then
  dest_dir="$BIN_DIR"
elif [ -w /usr/local/bin ] || [ "$(id -u)" = "0" ]; then
  dest_dir=/usr/local/bin
else
  dest_dir="$HOME/.local/bin"
  mkdir -p "$dest_dir"
fi

# --- resolve version --------------------------------------------------------
if [ "$INSTALL_VERSION" = "latest" ]; then
  # GitHub redirects /releases/latest to /releases/tag/vX.Y.Z — capture
  # the effective URL and pull the tag off the end. No `jq` required.
  tag=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest")
  tag="${tag##*/}"
else
  tag="$INSTALL_VERSION"
fi
case "$tag" in
  v*) ;;
  *) printf 'could not resolve a release tag (got "%s")\n' "$tag" >&2; exit 1 ;;
esac

archive="sidebar-${tag}-${target}.tar.gz"
base="https://github.com/${REPO}/releases/download/${tag}"

# --- download ---------------------------------------------------------------
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

printf '→ Downloading %s\n' "$archive"
curl -fsSL -o "$tmp/$archive" "${base}/${archive}"
curl -fsSL -o "$tmp/${archive}.sha256" "${base}/${archive}.sha256"

# --- verify checksum --------------------------------------------------------
printf '→ Verifying sha256\n'
( cd "$tmp" && \
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -c "${archive}.sha256"
  else
    sha256sum -c "${archive}.sha256"
  fi
) >/dev/null || { printf 'checksum verification failed\n' >&2; exit 1; }

# --- install ----------------------------------------------------------------
tar -C "$tmp" -xzf "$tmp/$archive"
install -m 0755 "$tmp/sidebar" "$dest_dir/sidebar"

printf '✓ Installed %s/sidebar\n' "$dest_dir"
"$dest_dir/sidebar" --version

case ":$PATH:" in
  *":$dest_dir:"*) ;;
  *) cat <<EOF

Warning: $dest_dir is not in your PATH.
Add it to your shell profile, e.g.:
  echo 'export PATH="$dest_dir:\$PATH"' >> ~/.zshrc
EOF
  ;;
esac
