#!/usr/bin/env bash
# Install the latest signed supra release for this platform.
#
#   curl -fsSL https://raw.githubusercontent.com/Anonnymous0071/supra-harness/vX.Y.Z/scripts/install.sh |
#     SUPRA_VERSION=vX.Y.Z SUPRA_PUBKEY='<published minisign public key>' bash
#
# Env: PREFIX (default ~/.local/bin), SUPRA_VERSION (required release tag),
# SUPRA_PUBKEY (required independently published Minisign public key).
# Verifies SHA-256 and requires a minisign signature against SUPRA_PUBKEY before
# installing anything. Official releases are never installed checksum-only.
set -euo pipefail

REPO="Anonnymous0071/supra-harness"
PREFIX="${PREFIX:-$HOME/.local/bin}"
VERSION="${SUPRA_VERSION:-latest}"
# Release signing key. Pass the published project key explicitly until a key is
# embedded in a future release; absence is a refusal, never checksum-only fallback.
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

if [ "$VERSION" = "latest" ]; then
    echo "install: SUPRA_VERSION must pin the release tag used to fetch this installer" >&2
    exit 1
fi
if ! [[ "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]]; then
    echo "install: invalid release version $VERSION" >&2
    exit 1
fi

need() {
    command -v "$1" >/dev/null 2>&1 || { echo "install: need $1 ($2)" >&2; exit 1; }
}
need curl "download releases"
if command -v sha256sum >/dev/null 2>&1; then
    checksum() { sha256sum -c "$1"; }
elif command -v shasum >/dev/null 2>&1; then
    checksum() { shasum -a 256 -c "$1"; }
else
    echo "install: need sha256sum or shasum (checksum verification)" >&2
    exit 1
fi
need minisign "signature verification"
[ -n "$PUBKEY" ] || {
    echo "install: SUPRA_PUBKEY is required to verify official releases" >&2
    exit 1
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"

base="supra-${target}"
echo "install: fetching supra $VERSION for $target"
curl -fsSL -O "https://github.com/$REPO/releases/download/${VERSION}/${base}"
curl -fsSL -O "https://github.com/$REPO/releases/download/${VERSION}/${base}.sha256"

checksum "${base}.sha256" || { echo "install: checksum FAILED" >&2; exit 1; }
curl -fsSL -O "https://github.com/$REPO/releases/download/${VERSION}/${base}.minisig"
echo "$PUBKEY" > supra.pub
minisign -Vm "$base" -p supra.pub -x "${base}.minisig" || {
    echo "install: signature FAILED" >&2
    exit 1
}

mkdir -p "$PREFIX"
install -m 755 "$base" "$PREFIX/supra"
echo "install: supra $VERSION -> $PREFIX/supra"
"$PREFIX/supra" --version
