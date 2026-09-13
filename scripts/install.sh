#!/usr/bin/env bash
# Install one explicitly pinned, signed supra release.
#
# SUPRA_VERSION=vX.Y.Z SUPRA_PUBKEY='<published minisign public key>' \
#   curl -fsSL https://raw.githubusercontent.com/Anonnymous0071/supra-harness/vX.Y.Z/scripts/install.sh | bash
set -euo pipefail

REPO="Anonnymous0071/supra-harness"
PREFIX="${PREFIX:-$HOME/.local/bin}"
VERSION="${SUPRA_VERSION:?install: SUPRA_VERSION must pin a release tag}"
PUBKEY="${SUPRA_PUBKEY:?install: SUPRA_PUBKEY is required}"
[[ "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]] || {
    echo "install: invalid release version $VERSION" >&2
    exit 1
}

os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
case "$os-$arch" in
    linux-x86_64) target="x86_64-unknown-linux-musl" ;;
    linux-aarch64) target="aarch64-unknown-linux-gnu" ;;
    darwin-x86_64) target="x86_64-apple-darwin" ;;
    darwin-arm64) target="aarch64-apple-darwin" ;;
    *) echo "install: unsupported platform $os-$arch" >&2; exit 1 ;;
esac

need() { command -v "$1" >/dev/null 2>&1 || { echo "install: need $1 ($2)" >&2; exit 1; }; }
need curl "download releases"
need minisign "verify manifest signature"
need python3 "validate manifest binding"
need tar "extract the verified executable"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
base="supra-${target}"
archive="${base}.tar.gz"
manifest="${base}.manifest.json"
signature="${manifest}.minisig"
url="https://github.com/$REPO/releases/download/${VERSION}"
for file in "$archive" "$manifest" "$signature"; do
    curl -fsSLO "$url/$file"
done
printf '%s\n' "$PUBKEY" > supra.pub
minisign -Vm "$manifest" -p supra.pub -x "$signature" || { echo "install: manifest signature FAILED" >&2; exit 1; }

python3 - "$manifest" "$archive" "${VERSION#v}" "$target" <<'PY'
import hashlib, json, pathlib, sys, tarfile
manifest_path, archive_path, version, target = sys.argv[1:]
manifest_path = pathlib.Path(manifest_path)
archive_path = pathlib.Path(archive_path)
raw = manifest_path.read_bytes()
data = json.loads(raw)
if raw != (json.dumps(data, sort_keys=True, separators=(",", ":")) + "\n").encode():
    raise SystemExit("install: manifest is not canonical")
if set(data) != {"archive", "archive_sha256", "archive_size", "executable", "schema", "target", "version"}:
    raise SystemExit("install: manifest schema is not strict")
if set(data["executable"]) != {"path", "sha256", "size"} or data["schema"] != 1:
    raise SystemExit("install: manifest schema is not supported")
expected_member = f"supra-{target}/{'supra.exe' if '-windows-' in target else 'supra'}"
if data["version"] != version or data["target"] != target or data["archive"] != archive_path.name:
    raise SystemExit("install: manifest release binding mismatch")
blob = archive_path.read_bytes()
if len(blob) != data["archive_size"] or hashlib.sha256(blob).hexdigest() != data["archive_sha256"]:
    raise SystemExit("install: archive binding mismatch")
with tarfile.open(archive_path, "r:gz") as bundle:
    members = bundle.getmembers()
    if len(members) != 1 or members[0].name != expected_member or not members[0].isfile():
        raise SystemExit("install: unsafe archive members")
    executable = bundle.extractfile(members[0]).read(128 * 1024 * 1024 + 1)
if data["executable"]["path"] != expected_member or len(executable) != data["executable"]["size"]:
    raise SystemExit("install: executable size binding mismatch")
if hashlib.sha256(executable).hexdigest() != data["executable"]["sha256"]:
    raise SystemExit("install: executable digest binding mismatch")
pathlib.Path("supra").write_bytes(executable)
PY

mkdir -p "$PREFIX"
install -m 755 supra "$PREFIX/supra"
echo "install: supra ${VERSION#v} -> $PREFIX/supra"
"$PREFIX/supra" --version
