#!/usr/bin/env ruby
# frozen_string_literal: true

require "yaml"

workflow_path = File.expand_path("../../.github/workflows/ci.yml", __dir__)
expected = %w[macos-14 macos-15-intel ubuntu-24.04 windows-2022].sort.freeze

workflow = YAML.safe_load(File.read(workflow_path), [], [], true)
actual = workflow.fetch("jobs").fetch("rust").fetch("strategy").fetch("matrix").fetch("os")

unless actual.is_a?(Array) && actual.all?(String)
  warn "CI rust matrix must define an os array"
  exit 1
end

if actual.sort != expected
  warn "CI rust matrix mismatch: expected #{expected.join(", ")}; got #{actual.sort.join(", ")}"
  exit 1
end
puts "CI rust matrix covers exactly: #{expected.join(", ")}"
