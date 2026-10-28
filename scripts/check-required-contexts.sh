#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
#
# Verify that:
#   1. Every required status check context mapped in
#      .machine_readable/required-contexts-allow.txt (and any non-empty
#      required_status_checks context in .github/rulesets/*.json) resolves to
#      an existing workflow under .github/workflows/ that runs on pull_request
#      without path-filtering that could strand a PR (#219 defect class).
#   2. No workflow in .github/workflows/ invokes an admin bypass (`gh pr merge
#      --admin`) — enforcing cicd-squabbler's core `squabble != bypass`
#      invariant statically alongside the runtime binary.
#
# Exit contract (proven-tests-and-benches 3-outcome convention):
#   0 = all mapped required contexts are producible on pull_request and zero
#       bypass commands exist in workflows
#   1 = phantom/path-stranded required context or admin-bypass workflow found
#   2 = setup error (missing repo directory, workflows directory, or allowfile)
set -euo pipefail

REPO_ROOT="${1:-.}"
if [ ! -d "$REPO_ROOT" ]; then
  echo "check-required-contexts: repo directory '$REPO_ROOT' does not exist" >&2
  exit 2
fi
WORKFLOWS_DIR="$REPO_ROOT/.github/workflows"
ALLOW_FILE="$REPO_ROOT/.machine_readable/required-contexts-allow.txt"

if [ ! -d "$WORKFLOWS_DIR" ]; then
  echo "check-required-contexts: workflows directory '$WORKFLOWS_DIR' does not exist" >&2
  exit 2
fi
if [ ! -f "$ALLOW_FILE" ]; then
  echo "check-required-contexts: mapping file '$ALLOW_FILE' does not exist" >&2
  exit 2
fi

command -v bun >/dev/null 2>&1 || {
  echo "check-required-contexts: bun is required to parse workflow YAML" >&2
  exit 2
}

bun run - "$REPO_ROOT" <<'JS'
import { readdirSync, readFileSync, existsSync } from "node:fs";
import { join, resolve } from "node:path";

const root = resolve(process.argv[2]);
const wfDir = join(root, ".github/workflows");
const allowPath = join(root, ".machine_readable/required-contexts-allow.txt");
const rulesetsDir = join(root, ".github/rulesets");

let errors = 0;
/** Report a violation and increment the shared error count without exiting. */
function fail(msg) {
  console.error(`FAIL: ${msg}`);
  errors++;
}

// 1. Parse mapping file: "<context> => <.github/workflows/foo.yml>"
const mapped = new Map();
const lines = readFileSync(allowPath, "utf8").split(/\r?\n/);
for (let idx = 0; idx < lines.length; idx++) {
  const trimmed = lines[idx].trim();
  if (!trimmed || trimmed.startsWith("#")) continue;
  const parts = trimmed.split("=>").map((s) => s.trim());
  if (parts.length !== 2 || !parts[0] || !parts[1]) {
    console.error(`check-required-contexts: malformed mapping at line ${idx + 1}: ${lines[idx]}`);
    process.exit(2);
  }
  mapped.set(parts[0], parts[1]);
}
if (mapped.size === 0) {
  console.error("check-required-contexts: 0 contexts mapped in required-contexts-allow.txt");
  process.exit(2);
}

// 2. Also collect any required_status_checks declared in .github/rulesets/*.json
if (existsSync(rulesetsDir)) {
  for (const name of readdirSync(rulesetsDir).sort()) {
    if (!name.endsWith(".json")) continue;
    const rs = JSON.parse(readFileSync(join(rulesetsDir, name), "utf8"));
    for (const rule of rs.rules ?? []) {
      if (rule.type === "required_status_checks") {
        for (const check of rule.parameters?.required_status_checks ?? []) {
          if (check.context && !mapped.has(check.context)) {
            fail(`ruleset ${name} requires context '${check.context}', which has no producer mapping in .machine_readable/required-contexts-allow.txt`);
          }
        }
      }
    }
  }
}

// 3. Verify each mapped producer workflow exists, triggers on pull_request, and
//    has no pull_request.paths / paths-ignore filter that can strand a PR.
for (const [ctx, relWf] of mapped.entries()) {
  const fullWf = join(root, relWf);
  if (!existsSync(fullWf)) {
    fail(`required context '${ctx}' maps to missing workflow '${relWf}'`);
    continue;
  }
  const doc = Bun.YAML.parse(readFileSync(fullWf, "utf8"));
  const onBlock = doc?.on ?? doc?.true;
  const triggers =
    typeof onBlock === "string"
      ? [onBlock]
      : Array.isArray(onBlock)
        ? onBlock.map(String)
        : onBlock && typeof onBlock === "object"
          ? Object.keys(onBlock)
          : [];
  if (!triggers.includes("pull_request")) {
    fail(`required context '${ctx}' producer '${relWf}' does not trigger on pull_request`);
    continue;
  }
  const prCfg =
    onBlock && typeof onBlock === "object" && !Array.isArray(onBlock)
      ? onBlock.pull_request
      : null;
  if (prCfg && typeof prCfg === "object") {
    if (Object.hasOwn(prCfg, "paths") || Object.hasOwn(prCfg, "paths-ignore")) {
      fail(`required context '${ctx}' producer '${relWf}' filters pull_request by paths — strands PRs that do not touch those paths`);
    }
  }
}

// 4. Enforce squabble != bypass across all workflows: no workflow may call
//    `gh pr merge ... --admin` (joining backslash line continuations first).
for (const name of readdirSync(wfDir).sort()) {
  if (!name.endsWith(".yml") && !name.endsWith(".yaml")) continue;
  const raw = readFileSync(join(wfDir, name), "utf8").replace(/\\\r?\n\s*/g, " ");
  if (/gh\s+pr\s+merge\b[^\n]*--admin\b/.test(raw)) {
    fail(`.github/workflows/${name} invokes 'gh pr merge --admin' (violates squabble != bypass)`);
  }
}

if (errors > 0) {
  console.error(`check-required-contexts: ${errors} violation(s) found`);
  process.exit(1);
}
console.log(`OK: ${mapped.size} required status context(s) verified producible on pull_request; 0 admin-bypass workflows`);
JS
