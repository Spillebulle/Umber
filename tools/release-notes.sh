#!/bin/sh
# Print one version's section of CHANGELOG.md, which is what gets published as
# the GitHub release's notes.
#
#   sh tools/release-notes.sh 0.0.1
#
# The rule: a section starts at `## <version>` — either alone on the line or
# followed by a space and a date — and runs to the next line beginning `## `.
# `crates/umber-desktop/tests/release.rs` states the same rule in Rust and fails
# CI when the section for the version in Cargo.toml is missing, empty, or not
# the newest, so this script can assume a well-formed file and is only ever
# asked to find it.
#
# `tools/release.ps1` implements this again in PowerShell, because a Windows
# maintainer previewing the notes should not need a POSIX shell. The two must
# stay in step; the pair is the same arrangement `tools/fetch-brushes.*` uses.

set -eu

if [ $# -ne 1 ]; then
    echo "usage: $0 <version>" >&2
    exit 2
fi

version=$1
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
changelog="$root/CHANGELOG.md"

if [ ! -f "$changelog" ]; then
    echo "no CHANGELOG.md at $changelog" >&2
    exit 1
fi

notes=$(awk -v want="$version" '
    # Leaving a section: stop at the next version heading.
    inside && /^## / { exit }
    inside { print }
    # Entering: the heading is `## <version>` exactly, or `## <version> <date>`.
    !inside && /^## / {
        head = substr($0, 4)
        split(head, parts, " ")
        if (parts[1] == want) { inside = 1 }
    }
' "$changelog")

# Trim blank lines from both ends without stripping the blank lines between
# paragraphs, which carry the formatting GitHub renders.
notes=$(printf '%s\n' "$notes" | sed -e '/./,$!d' | sed -e ':a' -e '/^\n*$/{$d;N;};/\n$/ba')

if [ -z "$notes" ]; then
    echo "CHANGELOG.md has no notes under '## $version'" >&2
    exit 1
fi

printf '%s\n' "$notes"
