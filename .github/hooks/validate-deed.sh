#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
#
# validate-deed.sh — pre-push deed check, through the canonical linter.
#
# One validator per family (hyperpolymath/standards,
# docs/CONTRACTILE-DESCRIPTILE-VALIDATION.adoc): the grep checks that used to
# live here were a second validator. This runs 1-formats/deed/tools/deed_lint.js
# (Bun) from a standards checkout over the tracked .deed files, as the
# deed-conformance lane does in CI (.github/workflows/dogfood-gate.yml). The
# lane is the gate; this is the early warning. A tracked .a2ml file is an
# error on sight: the format is retired estate-wide (D99, D312) and its facts
# belong in the repo deed.
#
# Environment variables:
#   INPUT_PATH     — Repository to check (default: .)
#   STANDARDS_DIR  — A hyperpolymath/standards checkout
#                    (default: ../standards beside the repository)
#
# Exit codes:
#   0 — Every tracked deed lints, or no linter is available (skipped with a
#       notice: CI still runs the lane)
#   1 — A deed failed, or a .a2ml file is tracked

set -euo pipefail

REPO="${INPUT_PATH:-.}"
STANDARDS_DIR="${STANDARDS_DIR:-$(cd "$REPO/.." && pwd)/standards}"
LINTER="$STANDARDS_DIR/1-formats/deed/tools/deed_lint.js"

cd "$REPO"
errors=0

mapfile -t a2ml < <(git ls-files -- '*.a2ml')
for f in "${a2ml[@]}"; do
    echo "::error file=$f::.a2ml is retired; move its facts into the repo deed and delete it"
    errors=$((errors + 1))
done

if [ ! -f "$LINTER" ] || ! command -v bun >/dev/null 2>&1; then
    echo "::notice::DEED: no deed linter here (needs Bun and a standards checkout at $STANDARDS_DIR; set STANDARDS_DIR); the deed-conformance lane lints in CI"
    [ "$errors" -eq 0 ] || exit 1
    exit 0
fi

mapfile -t deeds < <(git ls-files -- '*.deed' | sort)
for f in "${deeds[@]}"; do
    echo "  Linting: $f"
    if ! bun "$LINTER" "$f"; then
        errors=$((errors + 1))
    fi
done

echo "DEED: ${#deeds[@]} file(s), ${errors} failing (linter: $LINTER)"
[ "$errors" -eq 0 ] || { echo "::error::DEED validation failed for ${errors} file(s)"; exit 1; }
echo "DEED validation passed."
