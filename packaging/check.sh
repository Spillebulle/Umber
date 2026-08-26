#!/bin/sh
# Check that the packaging metadata agrees with itself.
#
#   sh packaging/check.sh
#
# Everything here was a real failure of a real release build, found in CI ten
# minutes at a time. None of it needs flatpak, rpm or WiX installed, so it can
# run on every push and on any machine.
#
# The rule these all serve: the AppStream component, the desktop entry and the
# icons must share one name — the application ID. `appstreamcli compose`
# follows the component's `launchable` to the desktop entry and the desktop
# entry's `Icon` key to the icon, and if either hop misses it reports
# `gui-app-without-icon` and discards the whole component, which fails the
# Flatpak build. Nothing else notices, so this is only ever discovered in the
# one place it is expensive to discover.

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

fail() { printf 'packaging: %s\n' "$1" >&2; exit 1; }
ok()   { printf '  ok  %s\n' "$1"; }

APP_ID=io.github.spillebulle.umber

metainfo="packaging/$APP_ID.metainfo.xml"
desktop="packaging/$APP_ID.desktop"

[ -f "$metainfo" ] || fail "no AppStream file at $metainfo"
[ -f "$desktop" ]  || fail "no desktop entry at $desktop"
ok "both files are named for $APP_ID"

# The component id must be the application id.
id=$(sed -n 's/.*<id>\(.*\)<\/id>.*/\1/p' "$metainfo" | head -1)
[ "$id" = "$APP_ID" ] || fail "<id> is '$id', expected '$APP_ID'"
ok "<id> is $APP_ID"

# The launchable must name a desktop entry that is actually installed.
launchable=$(sed -n 's/.*<launchable[^>]*>\(.*\)<\/launchable>.*/\1/p' "$metainfo" | head -1)
[ -n "$launchable" ] || fail "the AppStream file has no <launchable>"
[ "$launchable" = "$APP_ID.desktop" ] || \
    fail "<launchable> is '$launchable' but the desktop entry installs as '$APP_ID.desktop'"
ok "<launchable> resolves to the installed desktop entry"

# The desktop entry's icon must name an icon that is actually installed.
icon=$(sed -n 's/^Icon=\(.*\)$/\1/p' "$desktop" | head -1)
[ -n "$icon" ] || fail "the desktop entry has no Icon key"
[ "$icon" = "$APP_ID" ] || \
    fail "Icon is '$icon' but the icons install as '$APP_ID.png'"
ok "Icon= resolves to the installed icons"

# AppStream wants a 64x64 at minimum, and every packaging script installs the
# same six sizes; a missing source file would fail the build much later.
for size in 16 32 48 64 128 256; do
    [ -f "assets/icons/umber-$size.png" ] || \
        fail "assets/icons/umber-$size.png is missing, and every package installs it"
done
ok "all six icon sizes are present"

# The Exec key names the binary, which is not renamed with everything else —
# `umber` is what a person types.
exec_line=$(sed -n 's/^Exec=\([^ ]*\).*$/\1/p' "$desktop" | head -1)
[ "$exec_line" = "umber" ] || fail "Exec runs '$exec_line', expected 'umber'"
ok "Exec runs umber"

# Every packaging script must agree on the application id. A rename that missed
# one of them is exactly how this check came to exist.
for f in packaging/linux/build-packages.sh packaging/linux/PKGBUILD \
         packaging/linux/$APP_ID.yml; do
    [ -f "$f" ] || fail "$f is missing"
    grep -q "$APP_ID" "$f" || fail "$f never mentions $APP_ID"
done
ok "every packaging script names $APP_ID"

# --- the house archive -------------------------------------------------------
#
# The path the `.deb` writes and the path the updater looks for have to be the
# same string, and they are written in two files in two languages that never
# see each other. Get it wrong and nothing breaks: the packages still install,
# the updater still runs, and it simply tells every user of the archive to go
# and download a file by hand for ever. That is the shape of defect this whole
# script exists for.
for path in /etc/apt/sources.list.d/spillebulle.sources \
            /etc/yum.repos.d/spillebulle.repo; do
    grep -q "$path" packaging/linux/build-packages.sh || \
        fail "packaging/linux/build-packages.sh never writes $path"
    grep -q "$path" crates/umber-app/src/update/install.rs || \
        fail "the updater never looks for $path"
done
ok "the packages and the updater agree on where the archive is configured"

# Not a failure: the public key is checked in when the archive has one, and
# until then the packages build exactly as they did before it existed. Said out
# loud on every run, because a package that quietly stopped enrolling machines
# would look identical to one that never did.
if [ -f packaging/linux/spillebulle-archive.asc ]; then
    ok "the archive key is checked in, so packages will enrol the machine"
else
    printf '  --  no packaging/linux/spillebulle-archive.asc: packages will not\n'
    printf '      enrol the machine in the archive (see ../Packages/README.md)\n'
fi

printf 'packaging metadata is consistent\n'
