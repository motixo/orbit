#!/bin/sh
set -eu

[ "$#" -eq 3 ] || { printf 'Usage: %s SOURCE BINARY OUTPUT\n' "$0" >&2; exit 2; }
orbit_source=$(realpath "$1")
orbit_binary=$(realpath "$2")
mkdir -p "$3"
orbit_output=$(realpath "$3")
orbit_version=$("$orbit_binary" --version)
orbit_version=${orbit_version#Orbit }
orbit_declared=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$orbit_source/Cargo.toml" | head -n 1)
[ "$orbit_version" = "$orbit_declared" ]
orbit_linkage=$(readelf -d "$orbit_binary")
if printf '%s\n' "$orbit_linkage" | grep -Eq '\(NEEDED\).*\[lib(heif|de265|avif|dav1d|aom|jpeg|png|webp)[^]]*\.so'; then
    printf 'Image codecs must be bundled in Orbit.\n' >&2
    exit 1
fi
case "$(uname -m)" in
    x86_64) orbit_arch=x86_64; orbit_deb_arch=amd64 ;;
    aarch64) orbit_arch=aarch64; orbit_deb_arch=arm64 ;;
    *) printf 'Unsupported architecture\n' >&2; exit 1 ;;
esac

orbit_work=$(mktemp -d)
trap 'rm -rf "$orbit_work"' EXIT HUP INT TERM
orbit_root=$orbit_work/root
orbit_appid=io.github.motixo.Orbit
install -Dm0755 "$orbit_binary" "$orbit_root/usr/bin/orbit"
for orbit_desktop in "$orbit_appid" "$orbit_appid.Applet" "$orbit_appid.Preview" "$orbit_appid.Menu" "$orbit_appid.Share" "$orbit_appid.AirDrop" "$orbit_appid.LocalSend" "$orbit_appid.Setup"; do
    install -Dm0644 "$orbit_source/resources/$orbit_desktop.desktop" "$orbit_root/usr/share/applications/$orbit_desktop.desktop"
done
install -Dm0644 "$orbit_source/resources/$orbit_appid.metainfo.xml" "$orbit_root/usr/share/metainfo/$orbit_appid.metainfo.xml"
for orbit_icon in "scalable/apps/$orbit_appid.svg" "symbolic/apps/$orbit_appid-symbolic.svg" "symbolic/apps/$orbit_appid-tray-symbolic.svg"; do
    install -Dm0644 "$orbit_source/resources/icons/hicolor/$orbit_icon" "$orbit_root/usr/share/icons/hicolor/$orbit_icon"
done
install -Dm0644 "$orbit_source/LICENSE" "$orbit_root/usr/share/licenses/orbit/LICENSE"
install -Dm0644 "$orbit_source/resources/localsend-LICENSE" "$orbit_root/usr/share/licenses/orbit/localsend-LICENSE"

cat > "$orbit_work/refresh" <<'EOF'
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database /usr/share/applications || :
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t /usr/share/icons/hicolor || :
fi
EOF

cp -a "$orbit_root" "$orbit_work/deb"
install -d "$orbit_work/deb/DEBIAN"
cat > "$orbit_work/deb/DEBIAN/control" <<EOF
Package: orbit
Version: $orbit_version
Architecture: $orbit_deb_arch
Maintainer: motixo <motixo@users.noreply.github.com>
Section: utils
Priority: optional
Homepage: https://github.com/motixo/orbit
Installed-Size: $(du -sk "$orbit_root" | cut -f1)
Depends: libc6 (>= 2.35), libgcc-s1, libxkbcommon0, libwayland-client0, libegl1, libgl1, libvulkan1, xdg-desktop-portal
Description: Native file shelf and panel applet for COSMIC
EOF
for orbit_hook in postinst postrm; do
    { printf '#!/bin/sh\n'; cat "$orbit_work/refresh"; } > "$orbit_work/deb/DEBIAN/$orbit_hook"
    chmod 0755 "$orbit_work/deb/DEBIAN/$orbit_hook"
done
dpkg-deb --root-owner-group -Zxz --build "$orbit_work/deb" "$orbit_output/orbit-$orbit_deb_arch.deb"
[ "$(dpkg-deb -f "$orbit_output/orbit-$orbit_deb_arch.deb" Version)" = "$orbit_version" ]
[ "$(dpkg-deb -f "$orbit_output/orbit-$orbit_deb_arch.deb" Architecture)" = "$orbit_deb_arch" ]

mkdir -p "$orbit_work/rpm/BUILD" "$orbit_work/rpm/BUILDROOT" "$orbit_work/rpm/RPMS" "$orbit_work/rpm/SOURCES" "$orbit_work/rpm/SPECS" "$orbit_work/rpm/SRPMS"
cat > "$orbit_work/rpm/SPECS/orbit.spec" <<EOF
%global debug_package %{nil}
%global _build_id_links none
Name: orbit
Version: $orbit_version
Release: 1
Summary: Native file shelf and panel applet for COSMIC
License: GPL-3.0-only
URL: https://github.com/motixo/orbit
Requires: libxkbcommon.so.0()(64bit), libwayland-client.so.0()(64bit), libEGL.so.1()(64bit), libGL.so.1()(64bit), libvulkan.so.1()(64bit), xdg-desktop-portal

%description
Collect files, text and links with drag and drop on COSMIC.

%install
mkdir -p "%{buildroot}"
cp -a "$orbit_root/." "%{buildroot}/"

%files
/usr/bin/orbit
/usr/share/applications/$orbit_appid.desktop
/usr/share/applications/$orbit_appid.Applet.desktop
/usr/share/applications/$orbit_appid.Preview.desktop
/usr/share/applications/$orbit_appid.Menu.desktop
/usr/share/applications/$orbit_appid.Share.desktop
/usr/share/applications/$orbit_appid.AirDrop.desktop
/usr/share/applications/$orbit_appid.LocalSend.desktop
/usr/share/applications/$orbit_appid.Setup.desktop
/usr/share/metainfo/$orbit_appid.metainfo.xml
/usr/share/icons/hicolor/scalable/apps/$orbit_appid.svg
/usr/share/icons/hicolor/symbolic/apps/$orbit_appid-symbolic.svg
/usr/share/icons/hicolor/symbolic/apps/$orbit_appid-tray-symbolic.svg
%license /usr/share/licenses/orbit/LICENSE
%license /usr/share/licenses/orbit/localsend-LICENSE

%post
$(cat "$orbit_work/refresh")

%postun
$(cat "$orbit_work/refresh")
EOF
rpmbuild -bb --define "_topdir $orbit_work/rpm" "$orbit_work/rpm/SPECS/orbit.spec"
cp "$orbit_work/rpm/RPMS/$orbit_arch/orbit-$orbit_version-1.$orbit_arch.rpm" "$orbit_output/orbit-$orbit_arch.rpm"
[ "$(rpm -qp --qf '%{VERSION}' "$orbit_output/orbit-$orbit_arch.rpm")" = "$orbit_version" ]
[ "$(rpm -qp --qf '%{ARCH}' "$orbit_output/orbit-$orbit_arch.rpm")" = "$orbit_arch" ]

cp -a "$orbit_root" "$orbit_work/arch"
cat > "$orbit_work/arch/.PKGINFO" <<EOF
pkgname = orbit
pkgbase = orbit
pkgver = $orbit_version-1
pkgdesc = Native file shelf and panel applet for COSMIC
url = https://github.com/motixo/orbit
builddate = $(date +%s)
packager = motixo <motixo@users.noreply.github.com>
size = $(du -sb "$orbit_root" | cut -f1)
arch = $orbit_arch
license = GPL-3.0-only
xdata = pkgtype=pkg
depend = glibc>=2.35
depend = gcc-libs
depend = libxkbcommon
depend = wayland
depend = libglvnd
depend = vulkan-icd-loader
depend = xdg-desktop-portal
EOF
{
    printf 'post_install() {\n'
    cat "$orbit_work/refresh"
    printf '}\npost_upgrade() { post_install; }\npost_remove() { post_install; }\n'
} > "$orbit_work/arch/.INSTALL"
(
    cd "$orbit_work/arch"
    bsdtar --uid 0 --gid 0 --format=mtree --options='!all,use-set,type,uid,gid,mode,time,size,sha256,link' -czf .MTREE .PKGINFO .INSTALL usr
    bsdtar --uid 0 --gid 0 --zstd -cf "$orbit_output/orbit-$orbit_arch.pkg.tar.zst" .PKGINFO .INSTALL .MTREE usr
)
bsdtar -xOf "$orbit_output/orbit-$orbit_arch.pkg.tar.zst" .PKGINFO | grep -Fx "pkgver = $orbit_version-1"
