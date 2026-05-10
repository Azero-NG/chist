#!/usr/bin/env sh
# chist installer — downloads a prebuilt binary for your platform.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Azero-NG/chist/master/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/Azero-NG/chist/master/install.sh | sh -s -- v0.1.0
#
# Environment overrides:
#   CHIST_VERSION       pin a specific version (e.g. v0.1.0). Defaults to the latest release.
#   CHIST_INSTALL_DIR   install destination. Defaults to $HOME/.local/bin.

set -eu

REPO="Azero-NG/chist"
BIN="chist"
INSTALL_DIR="${CHIST_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${1:-${CHIST_VERSION:-}}"

err()  { printf 'error: %s\n' "$*" >&2; exit 1; }
info() { printf '%s\n' "$*"; }

need() { command -v "$1" >/dev/null 2>&1 || err "missing required tool: $1"; }

need curl
need tar
need uname
need mktemp

# --- detect platform -------------------------------------------------------
case "$(uname -s)" in
  Darwin) os="apple-darwin" ;;
  Linux)  os="unknown-linux-musl" ;;
  *) err "unsupported OS: $(uname -s). Build from source: https://github.com/${REPO}" ;;
esac

case "$(uname -m)" in
  x86_64|amd64)  arch="x86_64" ;;
  aarch64|arm64) arch="aarch64" ;;
  *) err "unsupported architecture: $(uname -m)" ;;
esac

target="${arch}-${os}"
asset="${BIN}-${target}.tar.gz"

# --- resolve version -------------------------------------------------------
if [ -z "${VERSION}" ]; then
  info "Resolving latest version..."
  # GitHub redirects /releases/latest to /releases/tag/<TAG>; capture the final URL.
  resolved=$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
    "https://github.com/${REPO}/releases/latest" 2>/dev/null) \
    || err "could not contact github.com to resolve latest version"
  VERSION="${resolved##*/tag/}"
  case "$VERSION" in
    v*) ;;
    *)  err "could not parse latest version (got: '$VERSION')" ;;
  esac
fi

url="https://github.com/${REPO}/releases/download/${VERSION}/${asset}"
sha_url="${url}.sha256"

# --- download --------------------------------------------------------------
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM HUP

info "Downloading ${asset} (${VERSION})..."
curl -fsSL --retry 3 "$url" -o "$tmp/$asset" \
  || err "download failed: $url"

# --- verify sha256 (best-effort: skip if no sidecar published) -------------
if curl -fsSL --retry 3 "$sha_url" -o "$tmp/$asset.sha256" 2>/dev/null; then
  info "Verifying sha256..."
  if command -v shasum >/dev/null 2>&1; then
    (cd "$tmp" && shasum -a 256 -c "$asset.sha256" >/dev/null) \
      || err "sha256 verification failed"
  elif command -v sha256sum >/dev/null 2>&1; then
    (cd "$tmp" && sha256sum -c "$asset.sha256" >/dev/null) \
      || err "sha256 verification failed"
  else
    info "  (no shasum/sha256sum found, skipping verification)"
  fi
else
  info "  (no sha256 sidecar published for this release, skipping verification)"
fi

# --- install ---------------------------------------------------------------
tar -xzf "$tmp/$asset" -C "$tmp" \
  || err "failed to extract $asset"
[ -f "$tmp/$BIN" ] || err "extracted archive does not contain '$BIN'"

mkdir -p "$INSTALL_DIR" \
  || err "could not create $INSTALL_DIR"

mv "$tmp/$BIN" "$INSTALL_DIR/$BIN" \
  || err "could not write $INSTALL_DIR/$BIN (try setting CHIST_INSTALL_DIR)"
chmod +x "$INSTALL_DIR/$BIN"

info ""
info "Installed: $INSTALL_DIR/$BIN ($VERSION)"

case ":${PATH}:" in
  *":${INSTALL_DIR}:"*)
    info "Run '$BIN --help' to get started."
    ;;
  *)
    info ""
    info "Note: ${INSTALL_DIR} is not in your PATH."
    info "Add it to your shell rc (e.g. ~/.zshrc or ~/.bashrc):"
    info ""
    info "    export PATH=\"${INSTALL_DIR}:\$PATH\""
    info ""
    info "Or invoke with the full path: ${INSTALL_DIR}/${BIN}"
    ;;
esac
