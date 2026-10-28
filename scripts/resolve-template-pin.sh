#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
#
# resolve-template-pin.sh — name the TEMPLATE commit a tree was minted from.
#
# `just repo-init` records that commit in the deed's (provenance) and
# (variant) clauses. It used to read it from local git (`git rev-parse HEAD`),
# which is right only inside a checkout OF the template. GitHub's "Use this
# template" button instead creates a new repository whose one root commit has
# the template's tree but none of its history, and repo-init then runs inside
# THAT repository. Local HEAD is the child's own commit. Measured on
# metadatastician/lintel, 2026-10-14: the mint recorded
#     :template-branch "claude/mint-lintel"  :template-commit "197ebf0…"
# a branch and commit of lintel itself, absent from rsr-template-repo. The
# answer-file then pointed at nothing, and the drift gate it feeds had no
# parent to diff against.
#
# Two cases, decided by the checkout's own origin, never guessed:
#
#   origin IS the template   → this is a template checkout. Local HEAD is the
#                              truth about what is in hand, including unpushed
#                              work (the #203 rationale), so it is recorded.
#   origin is anything else  → this is a child (or an exported copy). The
#                              template commit is found by TREE EQUALITY: a
#                              commit of the template whose tree equals the
#                              child's root-commit tree, or failing that the
#                              tree of the work tree as it stands. Equal trees
#                              are equal content, byte for byte — the same
#                              proof check-template-conformance.sh uses for
#                              leaked branches. No match is a loud failure;
#                              the child's own HEAD is never recorded.
#
# Usage:
#   resolve-template-pin.sh --slug OWNER/REPO [--url URL] [--commit SHA] [--dir DIR]
#
#   --slug    the template's owner/repo (repo-init derives it from the Justfile)
#   --url     where to read the template's history
#             (default https://github.com/OWNER/REPO.git; a local path works)
#   --commit  assert the template commit instead of searching; it must exist in
#             the template's history, and its own tree is what gets recorded
#   --dir     the checkout being minted (default .)
#
# Output on success, one per line:  branch=… commit=… tree=… source=…
#   source is one of template-checkout | root-commit | work-tree | asserted
# Exit: 0 resolved; 1 not resolvable (reason on stderr); 2 usage.
set -euo pipefail

SLUG="" URL="" COMMIT="" DIR="."
while [ $# -gt 0 ]; do
    case "$1" in
        --slug)   SLUG="${2:-}"; shift 2 ;;
        --url)    URL="${2:-}"; shift 2 ;;
        --commit) COMMIT="${2:-}"; shift 2 ;;
        --dir)    DIR="${2:-}"; shift 2 ;;
        *) echo "resolve-template-pin: unknown argument: $1" >&2; exit 2 ;;
    esac
done
[[ "$SLUG" =~ ^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]] \
    || { echo "resolve-template-pin: --slug OWNER/REPO is required (got '${SLUG}')" >&2; exit 2; }
URL="${URL:-https://github.com/${SLUG}.git}"

die() { echo "resolve-template-pin: $*" >&2; exit 1; }

git -C "$DIR" rev-parse --is-inside-work-tree >/dev/null 2>&1 \
    || die "$DIR is not a git work tree; repo-init needs one to know what it is minting"

# slug_of URL — owner/repo of a git URL, lower-cased; nothing for a bare path
# with fewer than two components. Same normalisation as
# check-template-conformance.sh (strip .git first, then scheme, then git@).
slug_of() {
    printf '%s' "$1" \
        | sed -E 's|/+$||; s|\.git$||; s|^[a-zA-Z][a-zA-Z0-9+.-]*://||; s|^[^@/]*@||; s|:|/|' \
        | awk -F/ 'NF>=2 {print tolower($(NF-1)"/"$NF)}'
}
ORIGIN="$(git -C "$DIR" config --get remote.origin.url 2>/dev/null || true)"
HEAD_SHA="$(git -C "$DIR" rev-parse --verify -q HEAD 2>/dev/null || true)"
want_slug="$(printf '%s' "$SLUG" | tr '[:upper:]' '[:lower:]')"

# ── Case 1: a checkout of the template itself ──────────────────────────────
if [ -z "$COMMIT" ] && [ -n "$ORIGIN" ] && [ "$(slug_of "$ORIGIN")" = "$want_slug" ]; then
    [ -n "$HEAD_SHA" ] || die "template checkout $DIR has no commit at HEAD"
    branch="$(git -C "$DIR" rev-parse --abbrev-ref HEAD)"
    [ "$branch" != HEAD ] || branch=UNASSIGNED
    printf 'branch=%s\ncommit=%s\ntree=%s\nsource=template-checkout\n' \
        "$branch" "$HEAD_SHA" "$(git -C "$DIR" rev-parse 'HEAD^{tree}')"
    exit 0
fi

# ── Case 2: a child, or an exported copy ───────────────────────────────────
# Candidate trees, strongest first. A root commit is what "Use this template"
# wrote; it stays the template's tree even after the child grows commits.
CANDS=()
if [ -n "$HEAD_SHA" ]; then
    while IFS= read -r root; do
        [ -n "$root" ] && CANDS+=("$(git -C "$DIR" rev-parse "${root}^{tree}")")
    done < <(git -C "$DIR" rev-list --max-parents=0 HEAD)
fi
# The work tree as `git add -A` would commit it (ignored paths excluded). Its
# objects go to a scratch store, so the checkout's own .git is not written.
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT
GIT_DIR_ABS="$(cd "$DIR" && cd "$(git rev-parse --git-common-dir)" && pwd)"
mkdir -p "$SCRATCH/objects"
wt_tree="$(cd "$DIR" && GIT_INDEX_FILE="$SCRATCH/index" GIT_OBJECT_DIRECTORY="$SCRATCH/objects" \
    GIT_ALTERNATE_OBJECT_DIRECTORIES="$GIT_DIR_ABS/objects" \
    sh -c 'git add -A . && git write-tree')" \
    || die "could not compute the tree of the work tree in $DIR"
CANDS+=("$wt_tree")

# The template's history, into a scratch bare repo. Over the network only the
# commits are needed (tree:0). A local path is cloned whole instead: that is
# cheap (hardlinks), and a filtered clone of a SHALLOW local source makes git
# lazily fetch the missing objects back from it, which deadlocked here
# (git 2.43, measured 2026-10-20).
TMPL="$SCRATCH/template.git"
FILTER=(--filter=tree:0)
case "$URL" in *://*|*@*:*) ;; *) FILTER=() ;; esac
if ! git clone -q --bare --no-tags "${FILTER[@]}" "$URL" "$TMPL" 2>"$SCRATCH/clone.err"; then
    sed 's/^/    /' "$SCRATCH/clone.err" >&2
    die "cannot read the template's history from $URL
    Set RSR_TEMPLATE_URL to a reachable clone of ${SLUG} (a local path works)."
fi
DEFAULT="$(git -C "$TMPL" symbolic-ref -q --short HEAD 2>/dev/null || true)"
if [ -n "$DEFAULT" ] && ! git -C "$TMPL" show-ref --verify --quiet "refs/heads/$DEFAULT"; then
    DEFAULT=""
fi
# A detached source HEAD (a CI checkout of a pull request) is still history.
git -C "$TMPL" rev-parse -q --verify HEAD >/dev/null 2>&1 \
    && git -C "$TMPL" update-ref refs/source/HEAD HEAD

# branch_of SHA — the default branch when it holds SHA, else the first branch
# that does, else UNASSIGNED (reachable only from a detached source HEAD).
branch_of() {
    if [ -n "$DEFAULT" ] && git -C "$TMPL" merge-base --is-ancestor "$1" "refs/heads/$DEFAULT" 2>/dev/null; then
        echo "$DEFAULT"; return
    fi
    local b
    b="$(git -C "$TMPL" for-each-ref --count=1 --contains "$1" --format='%(refname:short)' refs/heads)"
    echo "${b:-UNASSIGNED}"
}

if [ -n "$COMMIT" ]; then
    git -C "$TMPL" cat-file -e "${COMMIT}^{commit}" 2>/dev/null \
        || die "asserted template commit $COMMIT is not in the history of $URL"
    COMMIT="$(git -C "$TMPL" rev-parse "${COMMIT}^{commit}")"
    tree="$(git -C "$TMPL" rev-parse "${COMMIT}^{tree}")"
    case " ${CANDS[*]} " in
        *" $tree "*) ;;
        *) echo "resolve-template-pin: WARN: the tree in hand differs from asserted $COMMIT's tree $tree" >&2 ;;
    esac
    printf 'branch=%s\ncommit=%s\ntree=%s\nsource=asserted\n' "$(branch_of "$COMMIT")" "$COMMIT" "$tree"
    exit 0
fi

# Newest first along the default branch, then anywhere in the history.
REFS=()
[ -n "$DEFAULT" ] && REFS+=("refs/heads/$DEFAULT")
n=0
for t in "${CANDS[@]}"; do
    n=$((n + 1))
    for scope in "${REFS[@]}" --all; do
        # awk reads to the end rather than exiting at the first hit: an early exit
        # SIGPIPEs git log on a long history, and pipefail made that fatal
        # (exit 141 against the real rsr-template-repo, 2026-10-20).
        hit="$(git -C "$TMPL" log --format='%H %T' "$scope" | awk -v t="$t" '!found && $2 == t { print $1; found = 1 }')"
        if [ -n "$hit" ]; then
            if [ "$t" = "$wt_tree" ] && [ "$n" -eq "${#CANDS[@]}" ]; then src="work-tree"; else src="root-commit"; fi
            printf 'branch=%s\ncommit=%s\ntree=%s\nsource=%s\n' "$(branch_of "$hit")" "$hit" "$t" "$src"
            exit 0
        fi
    done
done

{
    echo "resolve-template-pin: no commit of ${SLUG} (read from $URL) has the tree this repo was minted from."
    echo "    searched for: ${CANDS[*]}"
    echo "    (root-commit trees first, then the current work tree)"
    echo "    This is a child of the template (origin ${ORIGIN:-<none>}), so its own HEAD"
    echo "    is not a template commit and will not be recorded as one."
    echo "    Fix one of:"
    echo "      - mint before changing the tree the template gave you;"
    echo "      - RSR_TEMPLATE_URL=<clone> if the template commit is not on ${URL};"
    echo "      - RSR_TEMPLATE_COMMIT=<sha> to assert the template commit you took."
} >&2
exit 1
