#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Exercise the artifact producer and missing-badge guard without GitHub writes.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
bun run - "$REPO_DIR" <<'JS'
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const repo = process.argv[2];
const readYaml = (file) => Bun.YAML.parse(readFileSync(join(repo, ".github/workflows", file), "utf8"));
const workflow = readYaml("dogfood-gate.yml");
const lock = readYaml("actions.lock");
const steps = workflow.jobs["k9-validate"].steps;
const validation = steps.find((s) => s.id === "validate");
const generate = steps.find((s) => s.name === "Generate K9 badge");
const upload = steps.find((s) => s.uses?.startsWith("actions/upload-artifact@"));
assert.ok(validation?.run.includes("validate-k9.sh"));
assert.equal(generate.if, "always()");
assert.equal(generate.env.K9_RESULT, "${{ steps.validate.outcome }}");
assert.equal(upload.if, "always()");
assert.equal(upload.with.name, "k9-grid");
assert.equal(upload.with.path, "${{ runner.temp }}/k9-grid/badge.json");
assert.equal(upload.with["if-no-files-found"], "error");
assert.ok(lock.workflows[".github/workflows/dogfood-gate.yml"].includes(upload.uses));
assert.ok(lock.dependencies[upload.uses]?.commit);
const badges = workflow.jobs.badges;
assert.equal(badges.needs, "k9-validate");
assert.match(badges.if, /github.event_name == 'push'/);
assert.match(badges.if, /github.ref == 'refs\/heads\/main'/);
const download = badges.steps.find((s) => s.uses?.startsWith("actions/download-artifact@"));
assert.notEqual(download["continue-on-error"], true);
assert.equal(download.with.name, upload.with.name);
assert.equal(download.with.path, "${{ runner.temp }}/k9-grid");
const publish = badges.steps.find((s) => s.env?.BADGE);
assert.equal(publish.env.BADGE, "${{ runner.temp }}/k9-grid/badge.json");

const scratch = mkdtempSync(join(tmpdir(), "dogfood-badge-"));
try {
  for (const result of ["success", "failure", "skipped", "cancelled"]) {
    const run = Bun.spawnSync(["bash", "-c", generate.run], {
      env: { ...process.env, RUNNER_TEMP: scratch, K9_RESULT: result },
    });
    assert.equal(run.exitCode, 0, run.stderr.toString());
    const badge = JSON.parse(readFileSync(join(scratch, "k9-grid/badge.json"), "utf8"));
    assert.equal(badge.schemaVersion, 1);
    assert.equal(badge.label, "K9");
    assert.equal(badge.message, result);
    assert.equal(badge.color, result === "success" ? "brightgreen" : "red");
  }
  const run = Bun.spawnSync(["bash", "-c", publish.run], {
    env: { ...process.env, BADGE: join(scratch, "missing.json") },
  });
  assert.equal(run.exitCode, 1);
  assert.ok(run.stdout.toString().includes("::error::"));
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
const security = readYaml("security-policy.yml");
const checkout = security.jobs.check.steps.find((s) => s.uses?.startsWith("actions/checkout@"));
assert.equal(checkout.with["persist-credentials"], false);
console.log("PASS: badge generation, artifact wiring, missing-badge failure, and checkout credential policy");
JS
