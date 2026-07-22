#!/usr/bin/env ruby
# frozen_string_literal: true

require "yaml"

workflow_path = File.expand_path("../../.github/workflows/ci.yml", __dir__)
expected = %w[macos-14 macos-15-intel ubuntu-24.04 windows-2022].sort.freeze
expected_eval = [
  { "os" => "ubuntu-24.04", "enforce_latency" => false },
  { "os" => "macos-14", "enforce_latency" => true }
].freeze
shared_cache_key = 'ci-${{ matrix.os }}'

workflow = YAML.safe_load(
  File.read(workflow_path),
  permitted_classes: [],
  permitted_symbols: [],
  aliases: true
)
jobs = workflow.fetch("jobs")

forbidden_warning_suppression = ["--no-deprecation", "--no-warnings", "NODE_NO_WARNINGS"].freeze
forbidden_warning_suppression.each do |value|
  next unless File.read(workflow_path).include?(value)

  warn "CI must not broadly suppress Node.js warnings with #{value}"
  exit 1
end

targeted_warning_suppression = {
  "Swatinem/rust-cache@v2" => "--disable-warning=DEP0040",
  "actions/download-artifact@v8" => "--disable-warning=DEP0005"
}.freeze
workflow_steps = jobs.values.flat_map { |job| job.fetch("steps", []) }
targeted_warning_suppression.each do |action, expected_node_options|
  matching_steps = workflow_steps.select { |step| step["uses"] == action }
  invalid_steps = matching_steps.reject do |step|
    step.dig("env", "NODE_OPTIONS") == expected_node_options
  end
  next unless matching_steps.empty? || invalid_steps.any?

  invalid_names = invalid_steps.map { |step| step.fetch("name", action) }
  warn "CI #{action} steps must use only #{expected_node_options}: #{invalid_names.join(", ")}"
  exit 1
end

puts "CI applies only code-specific Node.js warning suppression to affected actions"

rust_job = jobs.fetch("rust")
actual = rust_job.fetch("strategy").fetch("matrix").fetch("os")

unless actual.is_a?(Array) && actual.all?(String)
  warn "CI rust matrix must define an os array"
  exit 1
end

if actual.sort != expected
  warn "CI rust matrix mismatch: expected #{expected.join(", ")}; got #{actual.sort.join(", ")}"
  exit 1
end
puts "CI rust matrix covers exactly: #{expected.join(", ")}"

eval_job = jobs.fetch("eval")
unless Array(eval_job.fetch("needs")) == ["rust"]
  warn "CI eval job must depend only on the rust build job"
  exit 1
end

actual_eval = eval_job.fetch("strategy").fetch("matrix").fetch("include")
unless actual_eval == expected_eval
  warn "CI eval matrix must use ubuntu-24.04 without latency and macos-14 with latency"
  exit 1
end

rust_cache = rust_job.fetch("steps").find { |step| step["uses"] == "Swatinem/rust-cache@v2" }
eval_steps = eval_job.fetch("steps")
eval_cache = eval_steps.find { |step| step["uses"] == "Swatinem/rust-cache@v2" }
unless rust_cache&.dig("with", "shared-key") == shared_cache_key &&
       eval_cache&.dig("with", "shared-key") == shared_cache_key
  warn "CI rust and eval jobs must reuse the same OS-specific Rust cache"
  exit 1
end

eval_run = eval_steps.find { |step| step["name"] == "Run release evaluation gates" }
required_invocation = "cargo run -p compresso-eval --release -- run fixtures/ --gates docs/plan/gates.toml --json"
run_script = eval_run&.fetch("run", "")
unless run_script.include?(required_invocation)
  warn "CI eval job must run the documented release gate invocation"
  exit 1
end
unless run_script.include?('ENFORCE_LATENCY') &&
       run_script.include?('.name != "p95_latency_ms"') &&
       run_script.include?('.gates.failed_fixtures')
  warn "CI eval job must enforce latency only on the dedicated macOS leg"
  exit 1
end

large_payload_gate = eval_steps.find { |step| step["name"] == "Run large-payload latency gates" }
required_large_payload_invocation =
  "cargo bench -p compresso-eval --bench large_payload_latency -- --noplot"
unless large_payload_gate &&
       large_payload_gate["if"] == "${{ matrix.enforce_latency }}" &&
       large_payload_gate.fetch("run", "").include?(required_large_payload_invocation)
  warn "CI eval job must enforce the optimized large-payload benchmark on macos-14"
  exit 1
end

upload = eval_steps.find { |step| step["uses"] == "actions/upload-artifact@v7" }
unless upload && upload["if"] == "always()" &&
       upload.dig("with", "path") == 'eval-report-${{ matrix.os }}.json' &&
       upload.dig("with", "if-no-files-found") == "error"
  warn "CI eval job must always upload each OS-specific JSON report"
  exit 1
end

puts "CI eval job has the required runners, cache reuse, gate split, and report artifact"
