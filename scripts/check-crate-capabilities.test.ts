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
// The fixtures' driver is a path crate at 0.0.0; the real pin (crates.io,
// the locked version) is checked by the run over the real workspace and by
// the two tests after this one.
const fixtureDriver = {
  tokioProcessDriverPin: { source: null, version: "0.0.0" },
};

test("test_tokio_process_only_from_the_driver_passes", async () => {
  const result = await checkWorkspace(
    fixture("allowed-sql-driver-tokio-process"),
    fixtureDriver,
  );

  assert.deepEqual(result, { ok: true, checkedCrates: 3, violations: [] });
});

test("test_a_path_crate_named_like_the_driver_keeps_the_process_capability", async () => {
  const result = await checkWorkspace(
    fixture("allowed-sql-driver-tokio-process"),
  );

  assert.equal(result.ok, false);
  assert.match(
    result.violations.join("\n"),
    /compresso-sql \(database-network\): resolved dependency path compresso-sql -> mongodb -> tokio grants process capability/u,
  );
});

test("test_another_driver_version_keeps_the_process_capability", async () => {
  const result = await checkWorkspace(
    fixture("allowed-sql-driver-tokio-process"),
    { tokioProcessDriverPin: { source: null, version: "0.0.1" } },
  );

  assert.equal(result.ok, false);
});

test("test_tokio_process_beside_the_driver_still_fails", async () => {
  const result = await checkWorkspace(
    fixture("denied-mcp-tokio-process-beside-driver"),
    fixtureDriver,
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
    fixtureDriver,
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
    fixtureDriver,
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
    fixtureDriver,
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

// D117 review: evasions of the lexical source scan, each with the driver
// exemption in force, so only the scan can catch them. The std cases apply
// to every crate without process capability (on main too).
for (const [name, pattern] of [
  ["denied-sql-glob-tokio-process", /tokio::\* \(glob import\)/u],
  [
    "denied-sql-macro-tokio-process",
    /tokio::\$\.\.\. \(path built by a macro\)/u,
  ],
  ["denied-sql-include-outside-src", /include! of \.\.\/\.\.\/\.\.\/x\.rs/u],
  ["denied-sql-glob-std-process", /std::\* \(glob import\)/u],
  ["denied-sql-macro-std-process", /std::\$\.\.\. \(path built by a macro\)/u],
  [
    "denied-sql-process-reexported-by-driver",
    /mongodb::process::Command \(a process module of a dependency\)/u,
  ],
  [
    "denied-sql-std-process-reexported-by-dependency",
    /mongodb::process::Command \(a process module of a dependency\)/u,
  ],
] as const) {
  test(`test_${name.replaceAll("-", "_")}`, async () => {
    const result = await checkWorkspace(fixture(name), fixtureDriver);

    assert.equal(result.ok, false);
    assert.match(result.violations.join("\n"), pattern);
  });
}

// D117 confirmation round: the evasions a lexical scan missed (each now
// caught), the layout and lint-level checks that keep Clippy's
// disallowed-path lints in force, and ordinary code that must still pass.
for (const [name, pattern] of [
  ["denied-sql-charlit-hides-process", /tokio::process/u],
  [
    "denied-sql-nested-group-process",
    /tokio::\{\.\.process\.\.\} \(group of tokio\)/u,
  ],
  [
    "denied-sql-leading-colon-group",
    /tokio::\{\.\.process\.\.\} \(group of tokio\)/u,
  ],
  [
    "denied-sql-extern-alias-group",
    /tk07::\{\.\.process\.\.\} \(group of tokio\)/u,
  ],
  ["denied-sql-colon-alias", /t08::process \(alias of tokio\)/u],
  ["denied-sql-std-byte-charlit", /std::process/u],
  ["denied-sql-std-nested-group", /std::\{\.\.process\.\.\} \(group of std\)/u],
  ["denied-sql-macro-pair", /a macro given tokio and process/u],
  ["denied-sql-path-attribute", /a #\[path\] module is forbidden/u],
  [
    "denied-sql-bin-outside",
    /target spawn06 at .* is outside the scanned directories/u,
  ],
  ["denied-sql-build-script", /a build script .* is forbidden/u],
  ["denied-sql-clippy-config-missing", /clippy\.toml is missing/u],
  [
    "denied-sql-clippy-entry-missing",
    /clippy\.toml disallowed-methods must list tokio::process::Command::new/u,
  ],
  [
    "denied-sql-lint-allowed",
    /a lint attribute that can switch off clippy::disallowed_\* is forbidden/u,
  ],
  [
    "denied-sql-manifest-lint",
    /may not set the level of the disallowed lints/u,
  ],
] as const) {
  test(`test_${name.replaceAll("-", "_")}`, async () => {
    const result = await checkWorkspace(fixture(name), fixtureDriver);

    assert.equal(result.ok, false);
    assert.match(result.violations.join("\n"), pattern);
  });
}

test("test_local_process_modules_lifetimes_and_inert_members_pass", async () => {
  const result = await checkWorkspace(
    fixture("allowed-sql-local-process-module"),
    fixtureDriver,
  );

  assert.deepEqual(result, { ok: true, checkedCrates: 3, violations: [] });
});
