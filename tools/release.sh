#!/bin/sh
# Cut a release. The POSIX-shell twin of tools/release.ps1; the two must stay in
# step, the same arrangement tools/fetch-brushes.* uses.
#
#   sh tools/release.sh 0.0.2
#   sh tools/release.sh 0.0.2 --dry-run
#
# What it does, in order, stopping at the first thing that is not right:
#
#   1. checks the working tree is clean and on main
#   2. checks Cargo.toml's version matches the one asked for
#   3. checks CHANGELOG.md has a section for it, and prints the notes
#   4. runs fmt, clippy and the tests — the same gates CI runs
#   5. pushes the branch, writes an annotated tag, pushes the tag
#
# Pushing the tag is the whole of "make a release": the Release workflow builds
# the binaries, packages them and publishes the notes. Nothing here uploads
# anything, so this script cannot half-publish.

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

fail() { printf 'release: %s\n' "$1" >&2; exit 1; }
step() { printf '==> %s\n' "$1"; }

version=
dry_run=0
skip_tests=0
for arg in "$@"; do
    case "$arg" in
        --dry-run)    dry_run=1 ;;
        --skip-tests) skip_tests=1 ;;
        -*)           fail "unknown option '$arg'" ;;
        *)            version=$arg ;;
    esac
done

[ -n "$version" ] || fail "usage: $0 <version> [--dry-run] [--skip-tests]"
case "$version" in
    v*) fail "give the version as ${version#v}, without the leading v" ;;
esac
tag="v$version"

# --- 1. the tree -------------------------------------------------------------

step 'checking the working tree'
[ -z "$(git status --porcelain)" ] || \
    fail 'the working tree has uncommitted changes. A tag points at a commit, so anything not committed is not in the release.'

branch=$(git rev-parse --abbrev-ref HEAD)
[ "$branch" = main ] || fail "on branch '$branch', not main. Releases are cut from main."

[ -z "$(git tag -l "$tag")" ] || \
    fail "$tag already exists. Bump the version rather than moving a tag somebody may already have fetched."

# --- 2. the version ----------------------------------------------------------

step 'checking the version'
declared=$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)
[ -n "$declared" ] || fail 'no version found in Cargo.toml'
[ "$declared" = "$version" ] || \
    fail "Cargo.toml says $declared but you asked for $version. Edit [workspace.package] version, commit it, then run this again."

# --- 3. the notes ------------------------------------------------------------

step 'reading the release notes'
notes=$(sh tools/release-notes.sh "$version") || \
    fail "CHANGELOG.md has no notes under '## $version'. The release notes come from there, so there is nothing to publish."
printf '%s\n' "$notes" | grep -q '^\s*- ' || \
    fail "the '## $version' section has no bullet points. Release notes are a list of what the release brings."

printf '\n--- notes that will be published -------------------------------\n'
printf '%s\n' "$notes"
printf -- '----------------------------------------------------------------\n\n'

# --- 4. the gates ------------------------------------------------------------

if [ "$skip_tests" = 1 ]; then
    printf 'skipping fmt, clippy and tests as asked\n'
else
    step 'cargo fmt --all --check'
    cargo fmt --all --check || fail 'formatting is not clean'

    step 'cargo clippy --workspace --all-targets'
    RUSTFLAGS='-D warnings' cargo clippy --workspace --all-targets || fail 'clippy is not clean'

    step 'cargo test --workspace'
    cargo test --workspace || fail 'tests are failing'
fi

# --- 5. tag and push ---------------------------------------------------------

if [ "$dry_run" = 1 ]; then
    printf '\ndry run: everything passed. Re-run without --dry-run to push %s.\n' "$tag"
    exit 0
fi

step "pushing $branch"
git push origin "$branch" || fail 'pushing the branch failed'

step "tagging $tag"
git tag -a "$tag" -m "Umber $version" || fail 'tagging failed'

step "pushing $tag"
if ! git push origin "$tag"; then
    git tag -d "$tag" >/dev/null
    fail 'pushing the tag failed; the local tag has been removed so this can be run again'
fi

printf '\n%s is pushed. The Release workflow is building it:\n' "$tag"
printf '  https://github.com/Spillebulle/umber/actions/workflows/release.yml\n'
printf 'It publishes the release itself when the packages are built.\n'
