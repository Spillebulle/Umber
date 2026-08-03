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
#   5. pushes the branch and waits for CI to pass on that very commit
#   6. writes an annotated tag and pushes it
#
# Step 5 is why this needs the GitHub CLI. The gates in step 4 run on one
# machine, and every release that has gone wrong went wrong on a platform that
# machine is not — so a green run there is not evidence, and the tag waits for
# one that is. `--skip-ci` opts out; nothing else about the order is optional.
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
skip_ci=0
for arg in "$@"; do
    case "$arg" in
        --dry-run)    dry_run=1 ;;
        --skip-tests) skip_tests=1 ;;
        --skip-ci)    skip_ci=1 ;;
        -*)           fail "unknown option '$arg'" ;;
        *)            version=$arg ;;
    esac
done

[ -n "$version" ] || fail "usage: $0 <version> [--dry-run] [--skip-tests] [--skip-ci]"
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

# --- 5a. wait for CI on the commit being tagged ------------------------------
#
# **The gates above run on one machine, and that is the whole problem.** Every
# release that has failed so far failed on a platform this machine is not:
# 0.0.2 on a timing assertion on macOS, 0.0.4 on code that only compiled on
# Windows, 0.0.5 on a GPU test that only rounds that way on hardware. Each was
# green locally, each was tagged, and each was found out afterwards — which is
# exactly what the header says must not happen, because a tag spent on a broken
# workflow is one somebody may already have fetched.
#
# So the branch is pushed, CI is watched on that very commit, and the tag is
# written only once it is green. Nothing is spent if it is not: the commit is on
# main either way, and the fix is another commit and another run of this script.
if [ "$skip_ci" = 1 ]; then
    printf 'not waiting for CI as asked — the tag may land on a red commit\n'
else
    command -v gh >/dev/null 2>&1 || fail \
'the GitHub CLI (gh) is not installed, so CI cannot be checked before tagging.
Install it, or pass --skip-ci to tag without waiting — knowing that every
release that has gone wrong so far went wrong on a platform this machine is not.'

    sha=$(git rev-parse HEAD)
    step "waiting for CI on $(printf '%.8s' "$sha")"

    # 45 minutes, in 20-second steps. Long enough for a queued run on a busy
    # morning; short enough that a workflow which never starts is reported
    # rather than waited on for ever.
    tries=135
    verdict=
    while [ "$tries" -gt 0 ]; do
        # `--json` rather than the human-readable listing, whose columns move.
        # An empty answer means GitHub has not created the run yet, which is
        # ordinary for the first few seconds after a push.
        line=$(gh run list --workflow=ci.yml --limit 20 \
                   --json headSha,status,conclusion,url \
                   --jq "[.[] | select(.headSha == \"$sha\")][0]
                         | \"\(.status) \(.conclusion) \(.url)\"" 2>/dev/null)
        case "$line" in
            completed*) verdict=$line; break ;;
            ''|null*)   printf '    waiting for the run to appear...\n' ;;
            *)          printf '    %s...\n' "${line%% *}" ;;
        esac
        tries=$((tries - 1))
        sleep 20
    done

    [ -n "$verdict" ] || fail \
'CI has not finished within 45 minutes. Nothing has been tagged; look at the run
and run this again.'

    case "$verdict" in
        'completed success '*) printf '    CI is green\n' ;;
        *) fail "CI on this commit concluded '$(echo "$verdict" | cut -d' ' -f2)'.
Nothing has been tagged.
  $(echo "$verdict" | cut -d' ' -f3)
Fix it, commit, and run this again — the version and the notes are already in
place, so there is nothing to redo but the fix." ;;
    esac
fi

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
