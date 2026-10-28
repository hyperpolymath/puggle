#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Verify that inert generator inputs do not exempt active placeholder leaks.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
git -C "$fixture" init -q
git -C "$fixture" config remote.origin.url https://example.invalid/owner/application.git
# Keep the test independent of the caller's Actions repository identity.
export GITHUB_REPOSITORY=owner/application

checker="$repo/scripts/check-no-placeholders.sh"
fail() { echo "FAIL: $*" >&2; exit 1; }
token=$(printf '\173\173AUTHOR\175\175')

# These sources are copied to active paths before repo-init renders them.
inputs=(
    archetypes/julia-library/overlay/Project.toml.in
    archetypes/julia-library/overlay/test/cases/smoke.jl
    build/docs-seed/architecture.adoc
    build/templates/identity.deed.in
)
for input in "${inputs[@]}"; do
    mkdir -p "$(dirname "$fixture/$input")"
    printf '%s\n' "$token" > "$fixture/$input"
done
bash "$checker" "$fixture" || fail 'inert generator inputs were rejected'

# Exact-path exemptions must not follow a template into an active file, or
# turn into blanket exemptions for .in files, source, or documentation.
for active in Project.toml test/cases/smoke.jl docs/architecture.adoc other.in; do
    mkdir -p "$(dirname "$fixture/$active")"
    printf '%s\n' "$token" > "$fixture/$active"
    if output=$(bash "$checker" "$fixture" 2>&1); then
        fail "active leak passed: $active"
    fi
    [[ "$output" == *"$active"* ]] || fail "active leak was not identified: $active"
    rm "$fixture/$active"
done

# The settings identity guard must also run for template repositories.
mkdir -p "$fixture/.github"
printf 'repository:\n  name: "%s"\n' "$token" > "$fixture/.github/settings.yml"
if output=$(GITHUB_REPOSITORY=owner/rsr-template-repo bash "$checker" "$fixture" 2>&1); then
    fail 'template identity bypassed the settings guard'
fi
[[ "$output" == *'settings.yml'* ]] || fail 'settings leak was not identified'
echo 'PASS: generator inputs are preserved; active leaks and settings identity still fail'
