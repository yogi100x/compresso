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
    /compresso-terminal.*process API platform::process \(alias of std\)/u,
  );
});

test("test_grouped_std_process_import_fails", async () => {
  const result = await checkWorkspace(fixture("denied-terminal-grouped-std"));

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-terminal.*process API std::\{process\}/u,
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
