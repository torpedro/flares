#!/usr/bin/env bash
# Interactive release helper for Flares.
#
# Shows the current version, prompts for the next one, updates every file that
# records it, then optionally updates the changelog, commits, verifies and publishes
# to crates.io and PyPI, and tags. Every step is a prompt, and nothing is pushed or
# published without one; see docs/releases.md.

set -euo pipefail

PROJECT_NAME="Flares"
# Used for both the release commit subject and the tag message.
RELEASE_NAME="Flares"
PUBLISH_WARNING="This uploads three crates to crates.io. Published versions are permanent:
they cannot be replaced or deleted, only yanked."

cd "$(dirname "${BASH_SOURCE[0]}")/.."

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

# confirm <prompt>; returns 0 for yes, 1 for no. There is no default: the answer must
# be y, yes, n or no, in any case, and anything else asks again. A closed stdin answers
# no, so a non-interactive run never takes a publishing or tagging step by falling
# through.
confirm() {
    local prompt=$1 reply
    while true; do
        read -r -p "$prompt [y/n] " reply || return 1
        case $reply in
        [Yy] | [Yy][Ee][Ss]) return 0 ;;
        [Nn] | [Nn][Oo]) return 1 ;;
        esac
        printf 'Please answer y or n.\n'
    done
}

current_version() {
    sed -n '/^\[workspace\.package\]/,/^\[/p' Cargo.toml |
        sed -n 's/^version = "\(.*\)"$/\1/p' | head -1
}

# bump <version> <major|minor|patch>
bump() {
    local core=${1%%-*} part=$2 major minor patch
    IFS=. read -r major minor patch <<<"$core"
    case $part in
    major) printf '%d.0.0\n' "$((major + 1))" ;;
    minor) printf '%d.%d.0\n' "$major" "$((minor + 1))" ;;
    patch) printf '%d.%d.%d\n' "$major" "$minor" "$((patch + 1))" ;;
    esac
}

git rev-parse --git-dir >/dev/null 2>&1 || die "not a git repository"

current=$(current_version)
[ -n "$current" ] || die "could not read the workspace version from Cargo.toml"

printf '\n%s release\n\n  Current version: %s\n\n' "$PROJECT_NAME" "$current"
printf '  1) patch   %s\n' "$(bump "$current" patch)"
printf '  2) minor   %s\n' "$(bump "$current" minor)"
printf '  3) major   %s\n' "$(bump "$current" major)"
printf '  4) custom\n  q) quit\n\n'

read -r -p 'Select [1-4/q]: ' choice
case $choice in
1) target=$(bump "$current" patch) ;;
2) target=$(bump "$current" minor) ;;
3) target=$(bump "$current" major) ;;
4) read -r -p 'Version: ' target ;;
q | Q) exit 0 ;;
*) die "no such option: $choice" ;;
esac

[[ $target =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] ||
    die "not a version: $target"
[ "$target" != "$current" ] || die "already at $target"

tag="v$target"
git rev-parse -q --verify "refs/tags/$tag" >/dev/null &&
    die "tag $tag already exists; published tags must not move"

if [ -n "$(git status --porcelain)" ]; then
    printf '\nThe working tree has uncommitted changes:\n\n'
    git status --short
    printf '\n'
    confirm 'Continue anyway?' || exit 0
fi

printf '\nSetting version %s ...\n\n' "$target"
uv run --project clients/python python scripts/set_version.py "$target"

changelog_heading="## $target - $(date +%F)"
if grep -q '^## Unreleased$' CHANGELOG.md; then
    printf '\n'
    if confirm "Move the CHANGELOG \"Unreleased\" entries under $target?"; then
        sed -i "0,/^## Unreleased$/s//## Unreleased\n\n$changelog_heading/" CHANGELOG.md
        printf 'CHANGELOG.md: entries moved under %s\n' "${changelog_heading#\#\# }"
    fi
fi

printf '\nChanged files:\n\n'
git status --short
printf '\n'

if confirm "Commit as \"$RELEASE_NAME $target\"?"; then
    git add -A
    git commit -q -m "$RELEASE_NAME $target"
    printf 'Committed %s\n' "$(git rev-parse --short HEAD)"
else
    printf 'Left uncommitted. A tag would not include these changes.\n'
fi

# Publishing needs a committed tree: cargo refuses to package uncommitted changes,
# and a published version must correspond to a commit that exists.
published_crates=0
published_pypi=0
if [ -n "$(git status --porcelain)" ]; then
    printf '\nWorking tree is not clean, so the package cannot be verified or published.\n'
    printf 'Commit the changes, then follow docs/releases.md from step 3.\n'
else
    printf '\nVerifying the package ...\n\n'
    cargo publish --workspace --dry-run --locked --registry crates-io ||
        die "packaging failed; fix it before publishing"

    upstream=$(git rev-parse --abbrev-ref '@{upstream}' 2>/dev/null || true)
    if [ -n "$upstream" ] && [ -n "$(git log --oneline "$upstream"..HEAD)" ]; then
        printf '\nNote: HEAD is ahead of %s. Publishing a commit that is not pushed\n' "$upstream"
        printf 'leaves the registry pointing at source nobody else can fetch.\n'
        confirm 'Push it now?' && git push
    fi

    printf '\n%s\n' "$PUBLISH_WARNING"
    if confirm "Publish the $RELEASE_NAME crates at $target to crates.io?"; then
        cargo publish --workspace --locked --registry crates-io
        published_crates=1
        printf '\nPublished the %s crates at %s.\n' "$RELEASE_NAME" "$target"
    else
        printf '\nCrates not published. Resume at docs/releases.md step 4.\n'
    fi

    # The Python distributions come from scripts/build_artifacts.py (step 3). Only
    # the wheel and sdist are uploaded; dist/ also holds crates and tarballs.
    if [ ! -e "dist/flares_client-$target-py3-none-any.whl" ]; then
        printf '\n'
        if confirm "Build the release artifacts for $target now?"; then
            uv run --project clients/python python scripts/build_artifacts.py
        fi
    fi

    shopt -s nullglob
    wheels=(dist/flares_client-*-py3-none-any.whl)
    sdists=(dist/flares_client-*.tar.gz)
    shopt -u nullglob

    if [ ${#wheels[@]} -eq 0 ] || [ ${#sdists[@]} -eq 0 ]; then
        printf '\nNo Python distributions in dist/, so PyPI is skipped.\n'
        printf 'Build them with scripts/build_artifacts.py; see docs/releases.md step 3.\n'
    elif [ ${#wheels[@]} -gt 1 ] || [ ${#sdists[@]} -gt 1 ]; then
        printf '\ndist/ holds more than one build of the Python client:\n'
        printf '  %s\n' "${wheels[@]}" "${sdists[@]}"
        printf 'Remove the stale ones before uploading; see docs/releases.md step 4.\n'
    else
        printf '\nPython distributions to upload:\n  %s\n  %s\n' "${wheels[0]}" "${sdists[0]}"
        printf 'PyPI releases are permanent too: a filename cannot be reused once uploaded.\n'
        if confirm 'Publish these to PyPI?'; then
            read -r -s -p 'PyPI token: ' UV_PUBLISH_TOKEN
            printf '\n'
            export UV_PUBLISH_TOKEN
            # Clear the token even when the upload fails.
            upload_status=0
            uv publish "${wheels[0]}" "${sdists[0]}" || upload_status=$?
            unset UV_PUBLISH_TOKEN
            [ "$upload_status" -eq 0 ] || die "PyPI upload failed; see docs/releases.md"
            published_pypi=1
            printf '\nPublished the Python client at %s.\n' "$target"
        else
            printf '\nPyPI skipped. Resume at docs/releases.md step 4.\n'
        fi
    fi
fi

if [ "$published_crates" -eq 0 ] || [ "$published_pypi" -eq 0 ]; then
    printf '\nThis release is not fully published:'
    [ "$published_crates" -eq 1 ] || printf ' crates.io pending.'
    [ "$published_pypi" -eq 1 ] || printf ' PyPI pending.'
    printf '\nA tag should name a release that is on both registries; see docs/releases.md\n'
    printf 'steps 4 and 5.\n\n'
fi

if confirm "Create tag $tag?"; then
    git tag -a "$tag" -m "$RELEASE_NAME $target"
    printf '\nCreated %s locally. It is not pushed.\n' "$tag"
    printf '  push:   git push origin %s\n' "$tag"
    printf '  undo:   git tag -d %s\n' "$tag"
else
    printf '\nNo tag created. After publishing:\n'
    printf '  git tag -a %s -m "%s %s" && git push origin %s\n' \
        "$tag" "$RELEASE_NAME" "$target" "$tag"
fi

printf '\nNext: docs/releases.md, from the first step this run did not cover.\n\n'
