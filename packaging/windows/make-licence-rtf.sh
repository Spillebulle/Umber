#!/bin/sh
# Wrap a plain-text licence as RTF, which is the only format WiX's licence page
# will display.
#
#   packaging/windows/make-licence-rtf.sh LICENSE out/licence.rtf
#
# A script rather than a few lines inside the workflow because the escaping is
# exactly the sort of thing that reads fine in YAML and does something else in a
# shell — the inline version failed differently in CI and on a developer's
# machine, which is the worst way for a build step to be wrong.
#
# Two things it has to get right, both learned from a failed release build:
#
#   - **The byte-order mark.** `LICENSE` begins with a UTF-8 BOM. Passed
#     through, it lands inside the RTF as three high bytes and WiX rejects the
#     whole string as not representable in the installer's code page (WIX0311).
#   - **Anything else non-ASCII.** The GPL text is pure ASCII once the BOM is
#     gone, but a licence file is the kind of thing that acquires a curly quote
#     one day, and a release that fails at the packaging step because somebody
#     fixed a typo is a bad trade. Non-ASCII is dropped rather than transcoded:
#     this is a legal text that must be reproduced exactly or not at all, and
#     silently mapping a character to a different one would be worse than
#     losing it.

set -eu

if [ $# -ne 2 ]; then
    echo "usage: $0 <licence-file> <output.rtf>" >&2
    exit 2
fi

src=$1
out=$2

[ -f "$src" ] || { echo "no licence at '$src'" >&2; exit 1; }

mkdir -p "$(dirname "$out")"

{
    # \fs16 is 8pt: the GPL is long and the dialog is small.
    printf '{\\rtf1\\ansi\\ansicpg1252\\deff0{\\fonttbl{\\f0\\fnil Courier New;}}\\fs16\n'

    # 1. drop a leading byte-order mark
    # 2. drop every remaining byte outside printable ASCII, tab included
    # 3. escape RTF's three special characters
    # 4. end every line with a paragraph mark
    sed '1s/^\xEF\xBB\xBF//' "$src" \
        | LC_ALL=C tr -cd '\11\12\40-\176' \
        | sed -e 's/\\/\\\\/g' -e 's/{/\\{/g' -e 's/}/\\}/g' -e 's/$/\\par/'

    printf '}\n'
} > "$out"

# A licence page showing nothing would pass every check here and be obvious
# only to whoever runs the installer.
if [ "$(wc -c < "$out")" -lt 200 ]; then
    echo "the generated RTF is suspiciously small; is '$src' empty?" >&2
    exit 1
fi
