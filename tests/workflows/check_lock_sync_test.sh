#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Regression controls for PR #253's POSIX awk lock parser. No network used.
# Usage: bash tests/workflows/check_lock_sync_test.sh
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fail() { echo "FAIL: $*" >&2; cat "$work/out" >&2; exit 1; }
command -v mawk >/dev/null || { echo 'FAIL: mawk is required for portability regression' >&2; exit 1; }
# The production script prefers gawk. Hide it so this catches the original
# Ubuntu/mawk interval-regex failure even when gawk is installed on the host.
mkdir -p "$work/bin" "$work/base"
for tool in bash mawk sort; do ln -s "$(command -v "$tool")" "$work/bin/$tool"; done
cat > "$work/base/ci.yml" <<'YML'
name: Test
on: pull_request
jobs:
  reusable:
    uses: Owner/Repo/.github/workflows/check.yml@v1
  local:
    runs-on: ubuntu-latest
    steps:
      - uses: ./local-action
      - uses: 'OWNER/REPO/subdir@v1' # same repository pin
YML
cat > "$work/base/actions.lock" <<'LOCK'
version: 'v0.0.3'
workflows:
    '.github/workflows/ci.yml':
        - 'owner/repo@v1'
dependencies:
    'owner/repo@v1':
        ref: 'v1'
        commit: '1111111111111111111111111111111111111111'
LOCK
fresh() { rm -rf "$work/current"; cp -R "$work/base" "$work/current"; }
expect() { # <exit> <diagnostic> <label>
  local want=$1 pattern=$2 label=$3 got=0
  BASH_ENV=/dev/null PATH="$work/bin" bash "$repo/scripts/check-lock-sync.sh" "$work/current" > "$work/out" 2>&1 || got=$?
  [[ "$got" -eq "$want" ]] || fail "$label: expected $want, got $got"
  if [[ "$want" -eq 0 ]]; then pattern='is in sync and transitively closed'; fi
  grep -qF -- "$pattern" "$work/out" || fail "$label: missing diagnostic '$pattern'"
  echo "PASS: $label"
}

fresh
expect 0 'OK' 'mawk reads job and step uses, subpaths, comments, and owner/repo case'
fresh
sed -i "s/ref: 'v1'/ref: '0123456789abcdef0123456789abcdef012345'/" "$work/current/actions.lock"
expect 0 'OK' 'a 38-character hexadecimal tag is not a full SHA'
fresh
sed -i "s/ref: 'v1'/ref: '0123456789abcdef0123456789abcdef01234567'/" "$work/current/actions.lock"
expect 1 'SELF-SHA REFS' 'mawk detects exactly 40 hexadecimal characters in dependency ref'
for ref in abcdef0123456789abcdef0123456789abcdef01 ABCDEF0123456789ABCDEF0123456789ABCDEF01 aBcDeF0123456789aBcDeF0123456789aBcDeF01; do
  [[ "${#ref}" -eq 40 ]]
  fresh
  sed -i "s/ref: 'v1'/ref: '$ref'/" "$work/current/actions.lock"
  expect 1 'SELF-SHA REFS' "mawk rejects self-SHA refs regardless of hexadecimal case: $ref"
done
fresh
sed -i "s/ref: 'v1'/ref: '0123456789abcdef0123456789abcdef0123456789'/" "$work/current/actions.lock"
expect 0 'OK' 'a 42-character tag is not misclassified as a full SHA'
fresh
sed -i 's/@v1/@V1/g' "$work/current/ci.yml"
expect 1 'refs missing from the lockfile' 'refs remain case sensitive'
fresh
sed -i '/commit:/a\        uses:\n            - '\''other/action@v2'\''' "$work/current/actions.lock"
expect 1 'DANGLING EDGES' 'nested dependency edge must resolve to a record'
cat >> "$work/current/actions.lock" <<'LOCK'
    'other/action@v2':
        ref: 'v2'
        commit: '2222222222222222222222222222222222222222'
LOCK
expect 0 'OK' 'a transitively closed lock is accepted'
fresh
sed -i 's/^dependencies:/unused:/' "$work/current/actions.lock"
expect 1 'DANGLING EDGES' 'workflow lock pin needs a top-level dependency record'
fresh
sed -i "s/        - 'owner\/repo@v1'/        - 'owner\/repo@v2'/" "$work/current/actions.lock"
expect 1 'stale lockfile entries' 'orphan pins fail in the reverse direction'
fresh
printf 'name: Inline\non: pull_request\njobs: {}\n' > "$work/current/empty.yaml"
expect 1 'UNLISTED WORKFLOWS' 'zero-uses yaml workflow still needs a lock key'
sed -i "/^dependencies:/i\    '.github/workflows/empty.yaml': []" "$work/current/actions.lock"
expect 0 'OK' 'empty-list onboarding covers zero-uses workflows'
fresh
sed -i 's#./local-action#$/local-action#' "$work/current/ci.yml"
expect 1 'invalid local-action rewrite' 'corrupt dollar-slash local action fails'
fresh
sed -i "s/'v0.0.3'/'v0.0.2'/" "$work/current/actions.lock"
expect 1 'INVALID VERSION' 'old lock format is refused'
fresh
rm "$work/current/actions.lock"
expect 1 'no lockfile' 'missing lock is an explicit failure'

# Check the immediate length boundaries of the POSIX replacement for {40}.
for length in 39 41; do
  fresh
  printf -v ref '%*s' "$length" ''; ref=${ref// /a}
  sed -i "s/ref: 'v1'/ref: '$ref'/" "$work/current/actions.lock"
  expect 0 'OK' "a $length-character hexadecimal tag remains legal"
done
fresh
sed -i "s/ref: 'v1'/ref: 'g123456789abcdef0123456789abcdef0123456789'/" "$work/current/actions.lock"
expect 0 'OK' 'a 40-character tag containing nonhexadecimal text is legal'
fresh
sed -i '/^version:/d' "$work/current/actions.lock"
expect 1 'INVALID VERSION (missing' 'a missing format version is an explicit failure'
fresh
sed -i "/^dependencies:/i\    '.github/workflows/deleted.yml': []" "$work/current/actions.lock"
expect 1 'workflow file that does not exist' 'a removed workflow cannot leave an orphan lock key'

# Coverage is per workflow, even when another workflow locks the same ref.
fresh
cp "$work/current/ci.yml" "$work/current/second.yaml"
sed -i "/^dependencies:/i\    '.github/workflows/second.yaml': []" "$work/current/actions.lock"
expect 1 'refs missing from the lockfile' 'another workflow pin cannot satisfy this workflow'
sed -i "/'\.github\/workflows\/second.yaml':/c\    '.github/workflows/second.yaml':\n        - 'owner/repo@v1'" "$work/current/actions.lock"
expect 0 'OK' 'both workflow keys can share one resolved dependency record'

fresh
cat >> "$work/current/actions.lock" <<'LOCK'
    'unused/action@v2':
        ref: 'v2'
        commit: '2222222222222222222222222222222222222222'
LOCK
expect 0 'OK' 'an unreferenced leaf dependency is informational, not fatal'
sed -i "s/ref: 'v2'/ref: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'/" "$work/current/actions.lock"
expect 1 'SELF-SHA REFS' 'self-SHA refusal also covers unreferenced dependency records'
