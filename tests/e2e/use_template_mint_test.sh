#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
#
# use_template_mint_test.sh — mint a repository created the way GitHub's
# "Use this template" button creates one, and check what the mint wrote.
#
# Why. On 2026-10-14 metadatastician/lintel was created with that button (one
# root commit whose tree equals rsr-template-repo c9bce25, none of its history)
# and minted in place with RSR_NON_INTERACTIVE=1 just repo-init. Every defect
# below needed a hand correction there, and no test had ever run a full mint:
# tests/e2e/template_instantiation_test.sh runs nowhere in CI, and it mints an
# uncommitted copy, which is not how real children are born.
#
#   (a) provenance named the CHILD's own HEAD as the template commit;
#   (b) the self-name and strip passes rewrote template-machinery tests, and
#       tests/workflows/self_name_pass_test.sh then failed in the child;
#   (c) the strip pass edited git-ignored sibling checkouts under _build/;
#   (d) the closing advice armed a drift gate that a fresh mint cannot pass;
#   plus a repository map generated from the stale index (Estate Rules red).
#
# This test drives the REAL recipe end to end, then asserts each property,
# with a negative control beside each new check so it cannot pass vacuously.
# It needs just, rustc, uuidgen, nickel and file, and fails (never skips)
# without them. Network-free: the template's history is read from a local
# snapshot via RSR_TEMPLATE_URL.
#
# Usage: bash tests/e2e/use_template_mint_test.sh [--keep]
set -euo pipefail
repo=$(git rev-parse --show-toplevel)
work=$(mktemp -d)
if [ "${1:-}" = "--keep" ]; then echo "keeping $work"; else trap 'rm -rf "$work"' EXIT; fi

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS: $*"; }
g() { git -c user.name=t -c user.email=t@example.org -c commit.gpgsign=false "$@"; }

for tool in just rustc uuidgen nickel file; do
    command -v "$tool" >/dev/null 2>&1 \
        || fail "$tool is required to run a real mint (not skipping: a skipped mint test is how this went untested)"
done

# The four tests the lintel mint broke or rewrote.
FIXTURE_TESTS=(
    tests/workflows/self_name_pass_test.sh
    tests/workflows/template_conformance_test.sh
    tests/e2e/template_instantiation_test.sh
    tests/workflows/mint_cleanup_test.sh
)

# ── 1. The template: this checkout's tree, committed or not, on main ────────
T="$work/template"
g clone -q --no-local "$repo" "$T"
g -C "$T" checkout -q -B main
(cd "$repo" && git ls-files -z --cached --others --exclude-standard \
    | while IFS= read -r -d '' f; do { [ -e "$f" ] || [ -L "$f" ]; } && printf '%s\0' "$f"; done \
    | tar --null -T - -cf -) > "$work/tree.tar"
(cd "$T" && git ls-files -z | xargs -0 rm -f && tar xf "$work/tree.tar")
g -C "$T" add -A
g -C "$T" commit -q --allow-empty -m "template under test"
T_COMMIT=$(g -C "$T" rev-parse HEAD)
T_TREE=$(g -C "$T" rev-parse 'HEAD^{tree}')
T_OWNER=$(sed -n -E 's/^OWNER[[:space:]]*:=[[:space:]]*"([^"]+)".*/\1/p' "$T/Justfile" | head -1)
TEMPLATE_REPO=$(sed -n -E 's/^REPO[[:space:]]*:=[[:space:]]*"([^"]+)".*/\1/p' "$T/Justfile" | head -1)
T_SLUG="$T_OWNER/$TEMPLATE_REPO"

# use_template DIR — what "Use this template" creates: one root commit with
# the template's tree, no shared history, origin = the new repository.
use_template() {
    rm -rf "$1"; mkdir -p "$1"
    g -C "$T" archive "$T_COMMIT" | tar x -C "$1"
    g -C "$1" init -q -b main
    g -C "$1" add -A
    g -C "$1" commit -q -m "Initial commit"
    g -C "$1" remote add origin git@github.com:example-owner/mint-check.git
}

# mint DIR LOG — the non-interactive mint, isolated from this host's defaults
# file and global git config (an ORCID or signing key there would leak in).
mint() {
    local cfg="$work/cfg"; mkdir -p "$cfg"; : > "$cfg/gitconfig"
    (cd "$1" && env -i PATH="$PATH" HOME="$HOME" TMPDIR="${TMPDIR:-/tmp}" \
        ${RUSTUP_HOME:+RUSTUP_HOME="$RUSTUP_HOME"} ${CARGO_HOME:+CARGO_HOME="$CARGO_HOME"} \
        XDG_CONFIG_HOME="$cfg" GIT_CONFIG_GLOBAL="$cfg/gitconfig" GIT_CONFIG_NOSYSTEM=1 \
        RSR_NON_INTERACTIVE=1 RSR_TEMPLATE_URL="$T" RSR_MINT_TOOL_CACHE="$work/tools" \
        PROJECT_NAME="Mint Check" REPO=mint-check OWNER=example-owner \
        AUTHOR="Example Author" AUTHOR_EMAIL=author@example.org \
        timeout 900 just repo-init </dev/null) > "$2" 2>&1
}

# ── 2. The child, with an ignored cache and an untracked probe ──────────────
C="$work/child"
use_template "$C"
[ "$(g -C "$C" rev-parse 'HEAD^{tree}')" = "$T_TREE" ] || fail 'fixture: the child tree is not the template tree'
C_HEAD=$(g -C "$C" rev-parse HEAD)
# The probe carries all three things the rewrite passes act on.
LB='{{'; RB='}}'
probe="<!-- TEMPLATE INSTRUCTIONS: delete this block -->
${TEMPLATE_REPO} by ${LB}OWNER${RB}"
# An ignored sibling checkout, like lintel's _build/dependencies/<sibling>/.
mkdir -p "$C/_build/dependencies/sibling"
printf '%s\n' "$probe" > "$C/_build/dependencies/sibling/README.md"
g -C "$C" check-ignore -q _build/dependencies/sibling/README.md || fail 'fixture: _build/ is not ignored'
# Positive twin of the ignored cache: the same content in an untracked,
# not-ignored file IS the repository's and must be rewritten. Without it, an
# untouched cache could mean "the passes never ran" rather than "scoped".
printf '%s\n' "$probe" > "$C/mint-probe.md"
ignored_list() { (cd "$C" && git ls-files -o -i --exclude-standard | LC_ALL=C sort); }
IGN_LIST=$(ignored_list)
IGN_BEFORE=$(cd "$C" && printf '%s\n' "$IGN_LIST" | xargs -d '\n' sha256sum)
[ -n "$IGN_LIST" ] || fail 'fixture: no ignored files to protect'

# ── 3. Negative control for (a): an unrecognisable tree aborts, untouched ───
N="$work/no-match"
use_template "$N"
printf 'edited before minting\n' >> "$N/README.adoc"
g -C "$N" commit -q --amend -am "Initial commit"
if mint "$N" "$work/no-match.log"; then fail 'a tree matching no template commit was minted'; fi
grep -q 'mint aborted before writing anything' "$work/no-match.log" \
    || { cat "$work/no-match.log" >&2; fail 'the no-match abort did not say why'; }
[ -z "$(g -C "$N" status --porcelain)" ] || fail 'the aborted mint wrote to the tree'
pass "(a-) a child whose tree matches no template commit aborts before writing"

# ── 4. Negative control for the manifest: a stale entry aborts, untouched ───
M="$work/stale"
use_template "$M"
printf 'tests/workflows/no_such_test.sh\n' >> "$M/.machine_readable/mint-verbatim.txt"
g -C "$M" commit -q --amend -am "Initial commit"
if mint "$M" "$work/stale.log"; then fail 'a stale mint-verbatim entry was accepted'; fi
grep -q 'stale entry' "$work/stale.log" || { cat "$work/stale.log" >&2; fail 'the stale-entry abort did not say why'; }
[ -z "$(g -C "$M" status --porcelain)" ] || fail 'the stale-manifest mint wrote to the tree'
pass "(b-) a stale mint-verbatim entry stops the mint before it writes"

# ── 5. The mint ─────────────────────────────────────────────────────────────
mint "$C" "$work/mint.log" || { cat "$work/mint.log" >&2; fail 'just repo-init failed on a Use-this-template child'; }
pass "the non-interactive mint completes on a Use-this-template child"
DEED="$C/mint-check_chora.deed"
[ -f "$DEED" ] || fail 'the child deed was not renamed to mint-check_chora.deed'
field() { bash "$C/scripts/deed-field.sh" "$DEED" "$@"; }

# ── (a) provenance names the template's commit and tree ─────────────────────
[ "$(field provenance template-repo)" = "$T_SLUG" ]     || fail "template-repo is $(field provenance template-repo), want $T_SLUG"
[ "$(field provenance template-commit)" = "$T_COMMIT" ] || fail "template-commit is $(field provenance template-commit), want $T_COMMIT"
[ "$(field provenance template-tree)" = "$T_TREE" ]     || fail "template-tree is $(field provenance template-tree), want $T_TREE"
[ "$(field provenance template-branch)" = main ]        || fail "template-branch is $(field provenance template-branch), want main"
[ "$(field variant parent-pin)" = "$T_COMMIT" ]         || fail "variant parent-pin is $(field variant parent-pin), want $T_COMMIT"
if grep -qF "$C_HEAD" "$DEED"; then fail "the deed records the child's own commit $C_HEAD"; fi
pass "(a) provenance and variant name template commit ${T_COMMIT:0:12} and its tree, never the child's HEAD"

# ── (b) the fixture tests are carried unchanged, and pass in the child ──────
while IFS= read -r v; do
    g -C "$C" diff --quiet HEAD -- "$v" || fail "the mint modified $v, listed in mint-verbatim.txt"
done < <(sed -e 's/#.*//' -e 's/[[:space:]]*$//' "$C/.machine_readable/mint-verbatim.txt" | grep -v '^$')
for t in "${FIXTURE_TESTS[@]}"; do
    g -C "$C" diff --quiet HEAD -- "$t" || fail "the mint modified $t"
    (cd "$C" && RSR_MINT_TOOL_CACHE="$work/tools" bash "$t") > "$work/fixture.log" 2>&1 \
        || { cat "$work/fixture.log" >&2; fail "$t fails in the minted child"; }
done
pass "(b) the four template-machinery tests are byte-identical and pass in the child"

# ── (c) nothing outside the repository's own files changed ──────────────────
[ "$(cd "$C" && printf '%s\n' "$IGN_LIST" | xargs -d '\n' sha256sum)" = "$IGN_BEFORE" ] \
    || fail 'the mint changed git-ignored files'
# The one ignored file the mint may CREATE is the arrival pack's declared
# build artefact (.gitignore, arrival-pack/README.adoc), which `just claude-md`
# writes on every run. Any other new ignored file is a write outside the repo.
new_ignored=$(comm -13 <(printf '%s\n' "$IGN_LIST") <(ignored_list) \
    | grep -vxF .machine_readable/arrival-pack/claude-md-data.json || true)
[ -z "$new_ignored" ] || { echo "$new_ignored" >&2; fail 'the mint created files outside the repository'; }
if grep -q 'TEMPLATE INSTRUCTIONS' "$C/mint-probe.md"; then fail 'probe: the strip pass skipped a repository file'; fi
if grep -qF "$TEMPLATE_REPO" "$C/mint-probe.md"; then fail 'probe: the self-name pass skipped a repository file'; fi
if grep -qF "${LB}OWNER${RB}" "$C/mint-probe.md"; then fail 'probe: the substitution pass skipped a repository file'; fi
# The probe stays: it was a repository file at mint, so the map lists it.
pass "(c) ignored files are byte-identical, none created but the arrival-pack artefact; the same content in a repository file was rewritten"

# ── (d) the drift gate is no longer recommended for a minted repo ───────────
if grep -q 'arm the drift gate' "$work/mint.log"; then fail 'repo-init still recommends arming the drift gate'; fi
grep -q 'scripts/check-template-conformance.sh' "$work/mint.log" || fail 'repo-init does not point at the conformance check'
g -C "$C" add -A
g -C "$C" commit -q -m "mint"
(cd "$C" && bash scripts/check-template-conformance.sh --quiet) > "$work/conf.log" 2>&1 \
    || { cat "$work/conf.log" >&2; fail 'the conformance check fails on a fresh mint'; }
# The gate itself must still FAIL here (no silent green), say why, and not
# list the mint's own removals, which the (variant) clause now declares.
if (cd "$C" && bash scripts/check-variant-drift.sh "$T") > "$work/drift.log" 2>&1; then
    fail 'the drift gate turned green on a minted repo without (normalise) support: suspicious'
fi
grep -q 'records :role "minted-repo"' "$work/drift.log" || fail 'the drift gate does not explain its minted-repo findings'
if grep -q 'declare in paths.removed' "$work/drift.log"; then
    grep 'declare in paths.removed' "$work/drift.log" >&2; fail 'the mint removed paths it did not declare'
fi
pass "(d) stage 8 points at the conformance check (green); the drift gate still fails loudly, removals declared"

# ── The repository map is regenerated last ──────────────────────────────────
# Same check Estate Rules runs, on the committed mint: regenerate from the
# index and require no difference.
(cd "$C" && bash scripts/gen-repo-map.sh . >/dev/null)
g -C "$C" diff --quiet -- docs/architecture/REPOSITORY-MAP.adoc \
    || { g -C "$C" diff -- docs/architecture/REPOSITORY-MAP.adoc | head -20 >&2; fail 'the committed map is stale'; }
# Negative control: index mode, run where the mint runs it (before the first
# commit, after a removal), reproduces the stale map lintel shipped.
SM="$work/stale-map"
use_template "$SM"
(cd "$SM" && rm -rf archetypes && bash scripts/gen-repo-map.sh . >/dev/null)
grep -q '^| `archetypes' "$SM/docs/architecture/REPOSITORY-MAP.adoc" \
    || fail 'control: index mode no longer shows the pre-commit staleness; this check is vacuous'
pass "the committed map is fresh (the index-mode control reproduces lintel's stale map)"

(cd "$C" && env -u GITHUB_REPOSITORY bash scripts/check-no-placeholders.sh .) > "$work/ph.log" 2>&1 \
    || { cat "$work/ph.log" >&2; fail 'the minted child fails its own placeholder gate'; }
pass "the minted child passes its own placeholder gate"

echo "PASS: Use-this-template mint"
