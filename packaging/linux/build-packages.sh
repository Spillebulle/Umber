#!/bin/bash
# Build the Linux packages from an already-compiled binary.
#
#   packaging/linux/build-packages.sh <version> <binary> <arch> [outdir]
#
#   version   0.0.1
#   binary    path to the compiled `umber`
#   arch      amd64 | arm64   (Debian spelling; the others are derived)
#
# Emits a .deb, an .rpm and an AppImage into <outdir> (default: dist/).
#
# Written with `dpkg-deb` and `rpmbuild` directly rather than with `cargo-deb`
# and `cargo-generate-rpm`. Two reasons, and the second is the one that decides
# it: the package trees are laid out here where they can be read, rather than
# inferred from manifest keys with their own relative-path rules; and the
# libraries that matter to this application are **dlopened**, not linked, so no
# amount of automatic dependency detection will find them. See DEPENDS below.
#
# Runnable on any Debian-ish box with the tools installed, not only in CI, which
# is the point — a release process only a robot can run cannot be rehearsed.

set -euo pipefail

if [ $# -lt 3 ]; then
    sed -n '2,12p' "$0" >&2
    exit 2
fi

version=$1
binary=$2
arch=$3
outdir=${4:-dist}

root=$(cd -- "$(dirname -- "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p "$outdir"
outdir=$(cd "$outdir" && pwd)

case "$arch" in
    amd64) rpm_arch=x86_64;  appimage_arch=x86_64  ;;
    arm64) rpm_arch=aarch64; appimage_arch=aarch64 ;;
    *) echo "unknown arch '$arch' (want amd64 or arm64)" >&2; exit 2 ;;
esac

if [ ! -x "$binary" ]; then
    echo "no executable at '$binary'" >&2
    exit 1
fi

# Every one of these is opened at runtime by winit or wgpu rather than being
# recorded in the ELF, so `dpkg-shlibdeps` and rpm's own scanner cannot see any
# of them. A package that omitted them would install cleanly and then fail to
# open a window, which is the worst shape a packaging bug can take.
#
# **The clipboard added nothing to either list, and that was checked rather
# than assumed** — it is exactly the kind of change that usually does. Umber
# reaches the desktop's clipboard through `arboard` (pictures, `sysclip.rs`)
# and through egui-winit's own `clipboard` feature (text in the interface's
# fields). On X11 that is `x11rb`, on default features with neither `xcb_ffi`
# nor `dl-libxcb`, so it speaks the protocol over the socket itself and links no
# libxcb; on Wayland it is `wl-clipboard-rs` and `smithay-clipboard`, both of
# which reach `libwayland-client.so.0` and nothing else — and winit already
# opens that, so it is already below.
#
# One near miss, recorded so nobody has to work it out twice: `wl-clipboard-rs`
# carries `tree_magic_mini`, which without its (GPL-2.0-data) `with-gpl-data`
# feature reads `/usr/share/mime/magic` at runtime — a `shared-mime-info`
# dependency. It is **not** declared because that path is unreachable from
# here: it is used only for `MimeType::Autodetect`, and arboard names an
# explicit MIME type on every call in both directions. That rests on arboard's
# internals rather than on its API, so **an arboard bump has to re-check it.**
#
# What the clipboard did cost, since no `.so` is not no cost: second copies of
# `smithay-client-toolkit`, `calloop`, `calloop-wayland-source`, `rustix` and
# `thiserror` at versions beside winit's, plus `arboard`, `wl-clipboard-rs`,
# `clipboard-win` and `image`'s PNG codec. Build time and binary size, not
# dependencies.
DEB_DEPENDS="libc6, libgcc-s1, libx11-6, libxcursor1, libxrandr2, libxi6, libxkbcommon0, libwayland-client0, libvulkan1"

# RPM requirements are stated as **sonames**, not as package names.
#
# Package names differ between rpm distributions for the same library — Fedora
# calls it `libX11` and `vulkan-loader`, openSUSE calls the same things
# `libX11-6` and `libvulkan1` — so a package naming one will refuse to install
# on the other. Every rpm distribution, though, records the sonames a package
# provides, so requiring `libvulkan.so.1` resolves correctly on all of them
# without this script knowing which one it is being installed on.
#
# The `()(64bit)` marker is rpm's own way of distinguishing a 64-bit provider
# from a 32-bit one, and both architectures Umber builds for are 64-bit.
RPM_SONAMES="libX11.so.6 libXcursor.so.1 libXrandr.so.2 libXi.so.6 libxkbcommon.so.0 libwayland-client.so.0 libvulkan.so.1"

# The application ID. The desktop entry, the icons and the AppStream file are
# all named for it, and the AppStream `launchable` points at that desktop entry.
# `appstreamcli compose` follows that reference to find the icon, so a name that
# does not line up costs the whole component: `gui-app-without-icon`, and the
# build fails. It reads as pedantry right up until it does that.
APP_ID=io.github.spillebulle.umber

# --- the shared install tree -------------------------------------------------
#
# One layout, used by all three formats. /usr for the packages; the AppImage
# gets the same tree with its own root, which is what AppImage expects.
stage_tree() {
    local prefix=$1
    install -Dm755 "$binary" "$prefix/bin/umber"
    install -Dm644 "$root/packaging/$APP_ID.desktop" \
        "$prefix/share/applications/$APP_ID.desktop"
    install -Dm644 "$root/packaging/$APP_ID.metainfo.xml" \
        "$prefix/share/metainfo/$APP_ID.metainfo.xml"
    # The `.clip` type, which the shared MIME database does not know. Without
    # it the desktop entry's MimeType line has nothing to match for a Clip
    # Studio document and Umber never appears in "Open with" for one. The other
    # four types Umber reads are already in shared-mime-info.
    install -Dm644 "$root/packaging/$APP_ID.mime.xml" \
        "$prefix/share/mime/packages/$APP_ID.xml"
    # Thumbnails for the four formats no desktop can already draw. This one
    # needs no cache rebuild of its own: a file manager reads
    # share/thumbnailers directly, which is why there is no third `update-*`
    # command beside the two in the scriptlets below.
    install -Dm644 "$root/packaging/$APP_ID.thumbnailer" \
        "$prefix/share/thumbnailers/$APP_ID.thumbnailer"
    for size in 16 32 48 64 128 256; do
        install -Dm644 "$root/assets/icons/umber-$size.png" \
            "$prefix/share/icons/hicolor/${size}x${size}/apps/$APP_ID.png"
    done
    install -Dm644 "$root/LICENSE" "$prefix/share/doc/umber/LICENSE"
    install -Dm644 "$root/README.md" "$prefix/share/doc/umber/README.md"
    install -Dm644 "$root/CHANGELOG.md" "$prefix/share/doc/umber/CHANGELOG.md"
}

# --- .deb --------------------------------------------------------------------

echo "==> building umber_${version}_${arch}.deb"
deb="$work/deb"
stage_tree "$deb/usr"
mkdir -p "$deb/DEBIAN"
# Installed-Size is in kibibytes and Debian's own tools warn without it.
size=$(du -ks "$deb/usr" | cut -f1)
cat > "$deb/DEBIAN/control" <<EOF
Package: umber
Version: $version
Section: graphics
Priority: optional
Architecture: $arch
Depends: $DEB_DEPENDS
Installed-Size: $size
Maintainer: Spillebulle <spillebulle@gmail.com>
Homepage: https://github.com/Spillebulle/umber
Description: GPU-accelerated painting application built for latency
 Umber is a painting application written in Rust, designed so that the path
 between a pen moving and pixels changing is as short as possible. Strokes are
 composited on the GPU with a wet-layer scheme, so overlapping dabs never
 compound.
 .
 It ships 239 brush presets, reads brushes written for MyPaint, GIMP, Krita and
 Photoshop, and saves documents as OpenRaster.
EOF
# **Two caches decide whether Umber is offered for a file, and neither is built
# by copying files into place.** `update-desktop-database` builds
# `mimeinfo.cache`, which is what a file manager actually reads to answer "what
# opens this?" — without it the desktop entry's MimeType line is inert and Umber
# appears in "Open with" for nothing at all, however correct the entry is. And
# `update-mime-database` is what folds our `.clip` type into the shared
# database, without which a `.clip` is `application/octet-stream` and matches
# nothing. Both are cheap, both are idempotent, and both are guarded with
# `command -v`: a minimal system may have neither, and a painting application's
# package must not fail to install over a menu entry.
cat > "$deb/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if command -v update-mime-database >/dev/null 2>&1; then
    update-mime-database /usr/share/mime || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
EOF
# The same on the way out, so a removed Umber stops being offered rather than
# leaving a dead entry in every file manager's menu.
cat > "$deb/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
if command -v update-mime-database >/dev/null 2>&1; then
    update-mime-database /usr/share/mime || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
EOF
chmod 755 "$deb/DEBIAN/postinst" "$deb/DEBIAN/postrm"
dpkg-deb --build --root-owner-group "$deb" "$outdir/umber_${version}_${arch}.deb" >/dev/null

# --- .rpm --------------------------------------------------------------------

echo "==> building umber-${version}-1.${rpm_arch}.rpm"
rpmroot="$work/rpm"
mkdir -p "$rpmroot"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
buildroot="$work/rpmtree"
stage_tree "$buildroot/usr"

{
    echo "Name:           umber"
    echo "Version:        $version"
    echo "Release:        1"
    echo "Summary:        GPU-accelerated painting application built for latency"
    echo "License:        GPL-3.0-or-later"
    echo "URL:            https://github.com/Spillebulle/umber"
    echo "BuildArch:      $rpm_arch"
    for so in $RPM_SONAMES; do echo "Requires:       ${so}()(64bit)"; done
    # The binary is already built and stripped; rpm's debuginfo pass would try
    # to rebuild it from sources that are not here.
    echo "%global debug_package %{nil}"
    echo
    echo "%description"
    echo "Umber is a painting application written in Rust, designed so that the"
    echo "path between a pen moving and pixels changing is as short as possible."
    echo
    echo "%install"
    echo "cp -a $buildroot/usr %{buildroot}/"
    echo
    # The same two caches the `.deb` rebuilds, and for the same reason: without
    # `mimeinfo.cache` the desktop entry's MimeType line is inert, and without
    # the shared database the `.clip` type does not exist. Guarded, because a
    # package must not fail to install over a menu entry.
    echo "%post"
    echo "command -v update-mime-database >/dev/null 2>&1 && \\"
    echo "    update-mime-database /usr/share/mime || :"
    echo "command -v update-desktop-database >/dev/null 2>&1 && \\"
    echo "    update-desktop-database -q /usr/share/applications || :"
    echo
    echo "%postun"
    echo "command -v update-mime-database >/dev/null 2>&1 && \\"
    echo "    update-mime-database /usr/share/mime || :"
    echo "command -v update-desktop-database >/dev/null 2>&1 && \\"
    echo "    update-desktop-database -q /usr/share/applications || :"
    echo
    echo "%files"
    echo "/usr/bin/umber"
    echo "/usr/share/applications/$APP_ID.desktop"
    echo "/usr/share/metainfo/$APP_ID.metainfo.xml"
    echo "/usr/share/mime/packages/$APP_ID.xml"
    echo "/usr/share/thumbnailers/$APP_ID.thumbnailer"
    echo "/usr/share/icons/hicolor/*/apps/$APP_ID.png"
    echo "/usr/share/doc/umber/"
} > "$rpmroot/SPECS/umber.spec"

rpmbuild --define "_topdir $rpmroot" \
         --define "_buildhost umber-release" \
         -bb "$rpmroot/SPECS/umber.spec" >/dev/null
find "$rpmroot/RPMS" -name '*.rpm' -exec cp {} "$outdir/" \;

# --- AppImage ----------------------------------------------------------------
#
# The one format that has to run on a distribution nobody chose, so it carries
# its libraries with it. linuxdeploy walks the ELF and copies what it finds;
# the dlopened set above is deliberately *not* bundled — the Vulkan loader and
# the display client must be the host's, or the AppImage would talk to the
# wrong driver.

echo "==> building Umber-${version}-${appimage_arch}.AppImage"
appdir="$work/AppDir"
stage_tree "$appdir/usr"
# linuxdeploy wants these at the AppDir root as well as under usr/share.
cp "$root/packaging/$APP_ID.desktop" "$appdir/$APP_ID.desktop"
cp "$root/assets/icons/umber-256.png" "$appdir/$APP_ID.png"

tools="${APPIMAGE_TOOL_DIR:-$work/tools}"
mkdir -p "$tools"
fetch_tool() {
    local name=$1 url=$2
    if [ ! -x "$tools/$name" ]; then
        curl -fsSL -o "$tools/$name" "$url"
        chmod +x "$tools/$name"
    fi
}
base=https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous
fetch_tool linuxdeploy "$base/linuxdeploy-${appimage_arch}.AppImage"

# `--appimage-extract-and-run` because a CI container has no FUSE, and an
# AppImage tool is itself an AppImage.
export APPIMAGE_EXTRACT_AND_RUN=1
export OUTPUT="$outdir/Umber-${version}-${appimage_arch}.AppImage"
export VERSION="$version"
"$tools/linuxdeploy" \
    --appdir "$appdir" \
    --desktop-file "$appdir/$APP_ID.desktop" \
    --icon-file "$appdir/$APP_ID.png" \
    --output appimage

echo
echo "built into $outdir:"
ls -1 "$outdir"
