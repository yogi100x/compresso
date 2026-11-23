import assert from "node:assert/strict";
import { join } from "node:path";
import { test } from "node:test";

import { checkWorkspace } from "./check-crate-capabilities.ts";

const fixturesRoot = join(
  import.meta.dirname,
  "fixtures",
  "crate-capabilities",
);

function fixture(name: string) {
  return join(fixturesRoot, name);
}

test("test_reqwest_in_core_fails", async () => {
  const result = await checkWorkspace(fixture("denied-core-reqwest"));

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-core.*reqwest \(renamed http-client\).*network/u,
  );
});

test("test_reqwest_in_proxy_passes", async () => {
  const result = await checkWorkspace(fixture("allowed-proxy-reqwest"));

  assert.deepEqual(result, { ok: true, checkedCrates: 1, violations: [] });
});

test("test_direct_spawn_in_terminal_fails", async () => {
  const result = await checkWorkspace(fixture("denied-terminal-spawn"));

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-terminal.*process API.*std::process/u,
  );
});

test("test_bounded_local_integration_test_process_passes", async () => {
  const result = await checkWorkspace(
    fixture("allowed-bounded-local-test-process"),
  );

  assert.deepEqual(result, { ok: true, checkedCrates: 1, violations: [] });
});

test("test_bounded_local_production_process_still_fails", async () => {
  const result = await checkWorkspace(
    fixture("denied-bounded-local-production-process"),
  );
  const violations = result.violations.join("\n");

  assert.equal(result.ok, false);
  assert.match(violations, /compresso-cli.*process API.*src\/lib\.rs/u);
  assert.doesNotMatch(violations, /tests\/e2e\.rs/u);
});

test("test_cli_mcp_probe_module_process_passes", async () => {
  const result = await checkWorkspace(fixture("allowed-cli-mcp-probe-process"));

  assert.deepEqual(result, { ok: true, checkedCrates: 1, violations: [] });
});

test("test_mcp_probe_allowance_is_bound_to_the_cli_crate", async () => {
  const result = await checkWorkspace(
    fixture("denied-integrations-mcp-probe-process"),
  );

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-integrations.*process API.*src\/commands\/mcp_probe\.rs/u,
  );
});

test("test_mcp_probe_allowance_does_not_cover_other_cli_modules", async () => {
  const result = await checkWorkspace(
    fixture("denied-cli-mcp-probe-sibling-process"),
  );
  const violations = result.violations.join("\n");

  assert.equal(result.ok, false);
  assert.match(
    violations,
    /compresso-cli.*process API.*src\/commands\/doctor\.rs/u,
  );
  assert.doesNotMatch(violations, /mcp_probe\.rs/u);
});

test("test_bounded_local_integration_test_network_still_fails", async () => {
  const result = await checkWorkspace(
    fixture("denied-bounded-local-test-network"),
  );

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-cli.*network API std::net.*tests\/e2e\.rs/u,
  );
});

test("test_test_target_cannot_relabel_production_source", async () => {
  const result = await checkWorkspace(
    fixture("denied-bounded-local-dual-target-process"),
  );

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-cli.*process API.*tests\/shared\.rs/u,
  );
});

test("test_inert_std_process_apis_pass", async () => {
  const result = await checkWorkspace(fixture("allowed-core-inert-process"));

  assert.deepEqual(result, { ok: true, checkedCrates: 1, violations: [] });
});

test("test_process_control_apis_fail", async () => {
  const result = await checkWorkspace(fixture("denied-core-process-control"));
  const violations = result.violations.join("\n");

  assert.equal(result.ok, false);
  assert.match(violations, /compresso-core.*std::process::Command.*process/u);
  assert.match(violations, /compresso-core.*std::process::exit.*process/u);
  assert.match(violations, /compresso-core.*std::process::abort.*process/u);
});

test("test_std_process_wildcard_import_fails", async () => {
  const result = await checkWorkspace(fixture("denied-core-process-wildcard"));

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-core.*process API std::process::\*.*process/u,
  );
});

test("test_nested_process_imports_fail", async () => {
  const result = await checkWorkspace(fixture("denied-core-process-nested"));
  const violations = result.violations.join("\n");

  assert.equal(result.ok, false);
  assert.match(violations, /process API .*Command.*direct\.rs/u);
  assert.match(violations, /process API .*Command.*nested\.rs/u);
  assert.match(violations, /process API .*Command.*root_alias\.rs/u);
});

test("test_nested_inert_process_imports_pass", async () => {
  const result = await checkWorkspace(
    fixture("allowed-core-nested-inert-process"),
  );

  assert.deepEqual(result, { ok: true, checkedCrates: 1, violations: [] });
});

test("test_sanctioned_agent_network_and_process_pass", async () => {
  const result = await checkWorkspace(fixture("allowed-agent-process"));

  assert.deepEqual(result, { ok: true, checkedCrates: 1, violations: [] });
});

test("test_browser_process_dependency_fails", async () => {
  const result = await checkWorkspace(fixture("denied-browser-process"));

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-browser.*duct.*process/u,
  );
});

test("test_unknown_crate_class_fails_closed", async () => {
  const result = await checkWorkspace(fixture("denied-unknown-class"));

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-unknown.*unknown capability class/u,
  );
});

test("test_external_workspace_member_fails_closed", async () => {
  const result = await checkWorkspace(
    fixture("denied-external-workspace-member"),
  );

  assert.equal(result.checkedCrates, 1);
  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-unknown.*unknown capability class/u,
  );
});

test("test_table_form_tokio_process_feature_fails", async () => {
  const result = await checkWorkspace(
    fixture("denied-core-tokio-process-table"),
  );

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-core.*tokio.*process capability/u,
  );
});

test("test_aliased_std_process_fails", async () => {
  const result = await checkWorkspace(fixture("denied-terminal-aliased-std"));

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-terminal.*process API platform::process::Command \(alias of std::process\)/u,
  );
});

test("test_grouped_std_process_import_fails", async () => {
  const result = await checkWorkspace(fixture("denied-terminal-grouped-std"));

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-terminal.*process API proc::Command \(alias of std::process\)/u,
  );
});

test("test_transitive_reqwest_in_core_fails", async () => {
  const result = await checkWorkspace(
    fixture("denied-core-transitive-reqwest"),
  );

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-core.*compresso-core -> capability-helper -> reqwest.*network/u,
  );
});

test("test_transitive_process_dependency_in_core_fails", async () => {
  const result = await checkWorkspace(
    fixture("denied-core-transitive-process"),
  );

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-core.*compresso-core -> capability-helper -> duct.*process/u,
  );
});

// D115 amendment (b): compresso-mcp and compresso-cli may reach network
// packages only through the compresso-sql member, judged on each member's own
// resolved tree.
test("test_mcp_and_cli_network_only_through_sql_passes", async () => {
  const result = await checkWorkspace(
    fixture("allowed-mcp-network-through-sql"),
  );

  assert.deepEqual(result, { ok: true, checkedCrates: 3, violations: [] });
});

test("test_mcp_direct_network_beside_sql_still_fails", async () => {
  const result = await checkWorkspace(
    fixture("denied-mcp-direct-network-beside-sql"),
  );

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-mcp \(pure-local\): resolved dependency path compresso-mcp -> tokio -> socket2 without compresso-sql grants network capability/u,
  );
});

test("test_network_through_sql_fails_for_every_other_crate", async () => {
  const result = await checkWorkspace(
    fixture("denied-store-network-through-sql"),
  );
  const violations = result.violations.join("\n");

  assert.equal(result.ok, false);
  assert.match(
    violations,
    /compresso-store \(pure-local\): resolved dependency path compresso-store -> compresso-sql -> tokio -> socket2 grants network capability/u,
  );
  assert.match(
    violations,
    /compresso-integrations \(bounded-local\): resolved dependency path compresso-integrations -> compresso-sql -> tokio -> socket2 grants network capability/u,
  );
});

// D117: Tokio's process feature turned on only by the MongoDB driver (without
// its encryption feature) is not counted for compresso-sql and its two
// consumers; every other route to it still is.
test("test_tokio_process_only_from_the_driver_passes", async () => {
  const result = await checkWorkspace(
    fixture("allowed-sql-driver-tokio-process"),
  );

  assert.deepEqual(result, { ok: true, checkedCrates: 3, violations: [] });
});

test("test_tokio_process_beside_the_driver_still_fails", async () => {
  const result = await checkWorkspace(
    fixture("denied-mcp-tokio-process-beside-driver"),
  );

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-mcp \(pure-local\): resolved dependency path compresso-mcp -> compresso-sql -> mongodb -> tokio grants process capability/u,
  );
});

test("test_a_tokio_feature_implying_process_beside_the_driver_fails", async () => {
  const result = await checkWorkspace(
    fixture("denied-mcp-tokio-full-beside-driver"),
  );

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-mcp \(pure-local\): resolved dependency path compresso-mcp -> compresso-sql -> mongodb -> tokio grants process capability/u,
  );
});

test("test_the_driver_encryption_feature_keeps_the_process_capability", async () => {
  const result = await checkWorkspace(
    fixture("denied-sql-driver-encryption-process"),
  );
  const violations = result.violations.join("\n");

  assert.equal(result.ok, false);
  assert.match(
    violations,
    /compresso-sql \(database-network\): resolved dependency path compresso-sql -> mongodb -> tokio grants process capability/u,
  );
  assert.match(violations, /compresso-cli \(bounded-local\)/u);
});

test("test_the_driver_outside_the_database_crate_fails", async () => {
  const result = await checkWorkspace(
    fixture("denied-store-process-through-driver"),
  );
  const violations = result.violations.join("\n");

  assert.equal(result.ok, false);
  assert.match(
    violations,
    /compresso-store \(pure-local\): resolved dependency path compresso-store -> mongodb -> tokio grants process capability/u,
  );
  assert.match(
    violations,
    /compresso-store \(pure-local\): resolved dependency path compresso-store -> mongodb -> tokio -> socket2 grants network capability/u,
  );
});

test("test_network_through_non_member_sql_crate_fails_closed", async () => {
  await assert.rejects(
    checkWorkspace(fixture("denied-mcp-network-through-impostor-sql")),
    /compresso-sql must be exactly one workspace member/u,
  );
});

test("test_eval_calibration_contract_passes", async () => {
  const result = await checkWorkspace(fixture("allowed-eval-calibration"));

  assert.deepEqual(result, { ok: true, checkedCrates: 1, violations: [] });
});

test("test_eval_calibration_contract_fails_closed", async () => {
  const result = await checkWorkspace(
    fixture("denied-eval-calibration-contract"),
  );

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-eval.*reqwest.*network capability/u,
  );
});

test("test_network_through_a_pure_local_crate_enabled_by_sql_fails", async () => {
  // compresso-sql turns on a feature of compresso-core that pulls a network
  // package: the real tree reaches it through compresso-core, not
  // compresso-sql, even though the shadow resolve without compresso-sql
  // finds nothing (E17-50 review).
  const result = await checkWorkspace(
    fixture("denied-pure-local-feature-through-sql"),
  );

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-mcp \(pure-local\): resolved dependency path compresso-mcp -> compresso-core -> foo -> socket2 passes through workspace crate compresso-core/u,
  );
});
