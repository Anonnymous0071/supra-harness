#!/usr/bin/env bash
# Install the latest signed supra release for this platform.
#
#   curl -fsSL https://raw.githubusercontent.com/Anonnymous0071/supra-harness/main/scripts/install.sh | bash
#
# Env: PREFIX (default ~/.local/bin), SUPRA_VERSION (default: latest tag).
# Verifies SHA-256 and, when the release carries .minisig files, the minisign
# signature against the embedded public key before installing anything.
set -euo pipefail

REPO="Anonnymous0071/supra-harness"
PREFIX="${PREFIX:-$HOME/.local/bin}"
VERSION="${SUPRA_VERSION:-latest}"
# Release signing key. Published on first signed release; until then the
# installer verifies SHA-256 only and says so. Never paste a test key here:
# the only minisign key in-tree today lives in supra_update's unit tests.
PUBKEY="${SUPRA_PUBKEY:-}"

os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
case "$os-$arch" in
    linux-x86_64) target="x86_64-unknown-linux-musl" ;;
    linux-aarch64) target="aarch64-unknown-linux-gnu" ;;
    darwin-x86_64) target="x86_64-apple-darwin" ;;
    darwin-arm64) target="aarch64-apple-darwin" ;;
    *) echo "install: unsupported platform $os-$arch" >&2; exit 1 ;;
esac

need() {
    command -v "$1" >/dev/null 2>&1 || { echo "install: need $1 ($2)" >&2; exit 1; }
}
need curl "download releases"
need sha256sum "checksum verification (or set SKIP_CHECKSUM=1)"

if [ "$VERSION" = "latest" ]; then
    VERSION=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" | sed 's#.*/tag/##')
    [ -n "$VERSION" ] || { echo "install: could not resolve latest release" >&2; exit 1; }
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"

base="supra-${target}"
echo "install: fetching supra $VERSION for $target"
curl -fsSL -O "https://github.com/$REPO/releases/download/${VERSION}/${base}"
curl -fsSL -O "https://github.com/$REPO/releases/download/${VERSION}/${base}.sha256"

if [ -z "${SKIP_CHECKSUM:-}" ]; then
    sha256sum -c "${base}.sha256" || { echo "install: checksum FAILED" >&2; exit 1; }
fi

if curl -fsSL -O "https://github.com/$REPO/releases/download/${VERSION}/${base}.minisig" 2>/dev/null; then
    if [ -z "$PUBKEY" ]; then
        echo "install: release is signed but SUPRA_PUBKEY is unset; refusing (checksum passed)." >&2
        exit 1
    fi
    need minisign "signature verification"
    echo "$PUBKEY" > supra.pub
    minisign -Vm "$base" -p supra.pub -x "${base}.minisig" || { echo "install: signature FAILED" >&2; exit 1; }
else
    echo "install: no .minisig in this release; checksum only." >&2
fi

mkdir -p "$PREFIX"
install -m 755 "$base" "$PREFIX/supra"
echo "install: supra $VERSION -> $PREFIX/supra"
"$PREFIX/supra" --version
