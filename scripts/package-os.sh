#!/usr/bin/env bash
# Build standard .deb and .rpm packages from an already-built Linux release binary.
set -euo pipefail

target=${1:?usage: package-os.sh <target-triple> <version>}
version=${2:?usage: package-os.sh <target-triple> <version>}

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
dist="$root/dist"
binary="$root/target/${target}/release/supra"
pkgver=${version#v}
epoch=${SOURCE_DATE_EPOCH:-0}

[[ "$pkgver" =~ ^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]] || {
    echo "package-os: invalid semantic version $version" >&2
    exit 1
}
case "$target" in
    x86_64-*) arch_deb=amd64; arch_rpm=x86_64 ;;
    aarch64-*) arch_deb=arm64; arch_rpm=aarch64 ;;
    *) echo "package-os: unsupported target $target" >&2; exit 1 ;;
esac

for tool in dpkg-deb rpmbuild rpm; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "package-os: need $tool to build and validate standard packages" >&2
        exit 1
    }
done
[ -x "$binary" ] || { echo "package-os: $binary not built" >&2; exit 1; }
mkdir -p "$dist"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# dpkg-deb owns the archive format, metadata validation, and root ownership.
debroot="$work/deb"
mkdir -p "$debroot/DEBIAN" "$debroot/usr/bin" \
    "$debroot/usr/share/doc/supra"
install -m 755 "$binary" "$debroot/usr/bin/supra"
install -m 644 "$root/LICENSE-MIT" "$debroot/usr/share/doc/supra/copyright"
gzip -n -9 < "$root/CHANGELOG.md" > "$debroot/usr/share/doc/supra/changelog.gz"
installed_size=$(du -sk "$debroot/usr" | cut -f1)
cat > "$debroot/DEBIAN/control" <<EOF
Package: supra
Version: $pkgver
Section: devel
Priority: optional
Architecture: $arch_deb
Installed-Size: $installed_size
Maintainer: supra-harness contributors
Description: Peer-validated coding-agent harness CLI
 A peer-to-peer multi-agent coding harness with quorum consensus,
 structured logging, sandboxing, and signed updates.
Homepage: https://github.com/Anonnymous0071/supra-harness
EOF
find "$debroot" -exec touch --date="@$epoch" {} +
deb="$dist/supra_${pkgver}_${arch_deb}.deb"
rm -f "$deb"
SOURCE_DATE_EPOCH=$epoch dpkg-deb --root-owner-group -Zgzip -z9 --build "$debroot" "$deb" >/dev/null
dpkg-deb --info "$deb" >/dev/null
deb_contents="$work/deb.contents"
dpkg-deb --contents "$deb" > "$deb_contents"
grep -qE '[[:space:]]\./usr/bin/supra$' "$deb_contents"
grep -qE '[[:space:]]\./usr/share/doc/supra/changelog.gz$' "$deb_contents"
grep -qE '[[:space:]]\./usr/share/doc/supra/copyright$' "$deb_contents"
echo "packaged ${deb#$root/} (${installed_size}KiB installed)"

# rpmbuild owns the RPM headers, digests, payload metadata, and file manifest.
rpm_version=${pkgver%%[-+]*}
rpm_release=1
if [[ "$pkgver" == *-* ]]; then
    prerelease=${pkgver#*-}
    prerelease=${prerelease%%+*}
    prerelease=${prerelease//-/.}
    rpm_release="0.${prerelease}.1"
fi
rpmtop="$work/rpmbuild"
mkdir -p "$rpmtop"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}
install -m 755 "$binary" "$rpmtop/SOURCES/supra"
install -m 644 "$root/LICENSE-MIT" "$rpmtop/SOURCES/LICENSE-MIT"
install -m 644 "$root/LICENSE-APACHE" "$rpmtop/SOURCES/LICENSE-APACHE"
install -m 644 "$root/CHANGELOG.md" "$rpmtop/SOURCES/CHANGELOG.md"
cat > "$rpmtop/SPECS/supra.spec" <<EOF
%global debug_package %{nil}
%global __os_install_post %{nil}
Name: supra
Version: $rpm_version
Release: $rpm_release
Summary: Peer-validated coding-agent harness CLI
License: MIT OR Apache-2.0
URL: https://github.com/Anonnymous0071/supra-harness
Source0: supra
Source1: LICENSE-MIT
Source2: LICENSE-APACHE
Source3: CHANGELOG.md
BuildArch: $arch_rpm

%description
A peer-to-peer multi-agent coding harness with quorum consensus,
structured logging, sandboxing, and signed updates.

%prep
%build
%install
install -Dpm 0755 %{SOURCE0} %{buildroot}/usr/bin/supra
install -Dpm 0644 %{SOURCE1} %{buildroot}/usr/share/licenses/supra/LICENSE-MIT
install -Dpm 0644 %{SOURCE2} %{buildroot}/usr/share/licenses/supra/LICENSE-APACHE
install -Dpm 0644 %{SOURCE3} %{buildroot}/usr/share/doc/supra/CHANGELOG.md

%files
/usr/bin/supra
%license /usr/share/licenses/supra/LICENSE-MIT
%license /usr/share/licenses/supra/LICENSE-APACHE
%doc /usr/share/doc/supra/CHANGELOG.md
EOF
SOURCE_DATE_EPOCH=$epoch rpmbuild -bb "$rpmtop/SPECS/supra.spec" \
    --dbpath "$rpmtop/rpmdb" \
    --define "_topdir $rpmtop" \
    --define "_buildhost supra.invalid" \
    --define "source_date_epoch $epoch" \
    --define "use_source_date_epoch_as_buildtime 1" \
    --define "build_mtime_policy clamp_to_source_date_epoch" \
    --define "_binary_payload w9.gzdio" >/dev/null
mapfile -t rpms < <(find "$rpmtop/RPMS" -type f -name '*.rpm')
[ "${#rpms[@]}" -eq 1 ] || {
    echo "package-os: expected one binary RPM, found ${#rpms[@]}" >&2
    exit 1
}
rpm_path="$dist/$(basename "${rpms[0]}")"
install -m 644 "${rpms[0]}" "$rpm_path"
rpmdb="$work/rpmdb"
mkdir -p "$rpmdb"
rpm --dbpath "$rpmdb" --initdb
rpm --dbpath "$rpmdb" --checksig "$rpm_path" >/dev/null
rpm --dbpath "$rpmdb" -qip "$rpm_path" >/dev/null
rpm_contents="$work/rpm.contents"
rpm --dbpath "$rpmdb" -qlp "$rpm_path" > "$rpm_contents"
grep -qx '/usr/bin/supra' "$rpm_contents"
grep -qx '/usr/share/doc/supra/CHANGELOG.md' "$rpm_contents"
grep -qx '/usr/share/licenses/supra/LICENSE-APACHE' "$rpm_contents"
echo "packaged ${rpm_path#$root/}"
