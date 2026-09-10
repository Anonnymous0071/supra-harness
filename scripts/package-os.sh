#!/usr/bin/env bash
# Build .deb and .rpm packages for supra from a release binary.
#
#   scripts/package-os.sh <target-triple> <version>
#
# Pure POSIX packaging: .deb via `ar` + tar, .rpm via a hand-written
# cpio header — no dpkg-deb, rpmbuild, fpm, or nfpm required, so the
# release job builds every artefact on stock ubuntu-latest.
set -euo pipefail

target=${1:?usage: package-os.sh <target-triple> <version>}
version=${2:?usage: package-os.sh <target-triple> <version>}

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
dist="$root/dist"
bindir="$root/target/${target}/release"

name="supra"
pkgver="${version#v}"
arch_deb="amd64"
case "$target" in
    x86_64-*) arch_deb="amd64"; arch_rpm="x86_64" ;;
    aarch64-*) arch_deb="arm64"; arch_rpm="aarch64" ;;
    *) echo "package-os: unsupported target $target" >&2; exit 1 ;;
esac

[ -x "$bindir/$name" ] || { echo "package-os: $bindir/$name not built" >&2; exit 1; }
mkdir -p "$dist"

# ---------------------------------------------------------------------------
# .deb: ar archive of debian-binary + control.tar.gz + data.tar.gz
# ---------------------------------------------------------------------------
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/DEBIAN" "$work/usr/bin" "$work/usr/share/doc/$name"
cp "$bindir/$name" "$work/usr/bin/$name"
chmod 755 "$work/usr/bin/$name"
installed_size=$(du -sk "$work/usr" | cut -f1)
cat > "$work/DEBIAN/control" <<EOF
Package: $name
Version: $pkgver
Section: devel
Priority: optional
Architecture: $arch_deb
Maintainer: supra-harness contributors
Description: Peer-validated coding-agent harness CLI
 A peer-to-peer multi-agent coding harness with quorum consensus,
 structured logging, sandboxing, and signed updates.
Homepage: https://github.com/Anonnymous0071/supra-harness
EOF
printf '2.0\n' > "$work/debian-binary"
(cd "$work/DEBIAN" && tar -czf "$work/control.tar.gz" control)
(cd "$work" && tar -czf "$work/data.tar.gz" usr)
ar r "$dist/${name}_${pkgver}_${arch_deb}.deb" \
    "$work/debian-binary" "$work/control.tar.gz" "$work/data.tar.gz" >/dev/null
echo "packaged dist/${name}_${pkgver}_${arch_deb}.deb (${installed_size}KiB installed)"

# ---------------------------------------------------------------------------
# .rpm: SVR4-style cpio payload + minimal lead/header via rpm-python-free
# writer below. Implemented in python3 stdlib only.
# ---------------------------------------------------------------------------
python3 - "$dist" "$name" "$pkgver" "$arch_rpm" "$bindir/$name" <<'PY'
import gzip
import os
import struct
import sys
import time

dist, name, version, arch, binary = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5]
release = "1"

def cpio_entry(path, data, mode, mtime):
    namesize = len(path.encode()) + 1
    header = "070701%08x%08x%08x%08x%08x%08x%08x%08x%08x%08x%08x%08x%08x" % (
        1, mode, 0, 0, 1, mtime, len(data),
        0, 0, 0, 0, namesize, 0,
    )
    entry = header.encode("ascii") + path.encode() + b"\0"
    entry += b"\0" * ((4 - (len(entry) % 4)) % 4)
    entry += data
    entry += b"\0" * ((4 - (len(data) % 4)) % 4)
    return entry

mtime = int(time.time())
with open(binary, "rb") as handle:
    payload = handle.read()

entries = b"".join([
    cpio_entry(".", b"", 0o040755, mtime),
    cpio_entry("./usr", b"", 0o040755, mtime),
    cpio_entry("./usr/bin", b"", 0o040755, mtime),
    cpio_entry("./usr/bin/supra", payload, 0o100755, mtime),
    cpio_entry("TRAILER!!!", b"", 0, mtime),
])
compressed = gzip.compress(entries, mtime=mtime)

# Minimal RPMv3 file: lead (96 bytes) + signature header + main header
# + gzip cpio payload. Signatures are empty; verification of payload
# integrity is via the .sha256 sidecar and optional minisign.
def rpm_header(records):
    # Offsets are relative to the START of the header record area, which
    # begins with the 12-byte magic/version/count prefix — not to the
    # start of the index. (RPM header spec: data offset = from start of
    # header, i.e. including the 12-byte preamble.)
    index = bytearray()
    data = bytearray()
    for tag, kind, values in records:
        count = len(values)
        offset = 12 + 16 * len(records) + len(data)
        index += struct.pack(">IIII", tag, kind, offset, count)
        for value in values:
            if kind == 6:
                data += value + b"\0"
            elif kind == 4:
                data += struct.pack(">I", value)
            elif kind == 3:
                data += struct.pack(">H", value)
    body = bytes(index + data)
    out = struct.pack(">III", 0x8EADE801, 0, len(records)) + body
    pad = (8 - (len(out) % 8)) % 8
    return out + b"\0" * pad

sig_records = [
    (62, 4, [5]),
    (267, 6, [b"cpio"]),
    (269, 4, [9]),
    (273, 4, [1]),
    (274, 3, [1]),
    (275, 4, [0x2F6C]),
]
main_records = [
    (1000, 6, [name.encode()]),
    (1001, 6, [version.encode()]),
    (1002, 6, [release.encode()]),
    (1004, 6, [b"Peer-validated coding-agent harness CLI"]),
    (1011, 6, [arch.encode()]),
    (1014, 6, [b"MIT OR Apache-2.0"]),
    (1022, 4, [mtime]),
    (1090, 6, [b"supra-harness contributors"]),
]
sig = rpm_header(sig_records)
main = rpm_header(main_records)

lead = b"\xed\xab\xee\xdb" + bytes([3, 0])
lead += name.encode()[:65].ljust(66, b"\0")
lead += struct.pack(">H", 1)  # osnum = Linux
lead += struct.pack(">H", 5)  # sig_type: header+payload follows
lead += arch.encode()[:15].ljust(16, b"\0")  # arch
lead += b"\0" * 4  # reserved to 96
assert len(lead) == 96, len(lead)

rpm_path = os.path.join(dist, f"{name}-{version}-{release}.{arch}.rpm")
with open(rpm_path, "wb") as rpm:
    rpm.write(lead)
    rpm.write(sig)
    rpm.write(main)
    rpm.write(compressed)
print(f"packaged {rpm_path}")
PY

ls -la "$dist" | grep -E "deb|rpm" || true
