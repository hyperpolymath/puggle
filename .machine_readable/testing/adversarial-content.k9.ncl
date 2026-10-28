K9!
# SPDX-License-Identifier: MPL-2.0
# Adversarial and negative-control test fixtures (proven-tests-and-benches
# Silence + Firing doctrine). Documents why negative test suites name banned
# patterns or feed malformed inputs into gates so human reviewers and scanners
# do not mistake Firing controls for production defects.

{
  pedigree = {
    schema_version = "1.0.0",
    component_type = "testing:adversarial-controls",
    metadata = {
      name = "rsr-adversarial-controls",
      version = "0.1.0",
    },
    security = {
      leash = 'Kennel,
    },
  },

  adversarial_suites = [
    {
      path = "tests/proven_suite_test.sh",
      purpose = "Generates ephemeral Idris2 escape-hatch mutants and benchmark regressions inside mktemp to prove check-escape-hatches.sh, check-bench-regression.sh, compute-bench-baseline.sh, and emit-aggregate-inputs.sh fire with exact 0/1/2 exit codes.",
    },
    {
      path = "tests/workflows/check_ijson_jcs_test.sh",
      purpose = "Feeds duplicate keys, integers outside +/-(2^53-1), lone UTF-16 surrogates, Unicode noncharacters, and non-JCS formatting into check-ijson-jcs.sh.",
    },
    {
      path = "tests/workflows/squabble_gate_test.sh",
      purpose = "Plants admin-override strings, continue-on-error masks, and phantom ruleset contexts into ephemeral workflow fixtures to prove cicd-squabbler and check-required-contexts.sh fire.",
    },
    {
      path = "www/tests/controls",
      purpose = "Planted open-recursion BIND9, over-broad Caddyfile, and invalid AIBDP status controls.",
    },
  ],
}
