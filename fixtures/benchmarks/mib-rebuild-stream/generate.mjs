import { createHash } from "node:crypto";
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const outputDirectory = dirname(fileURLToPath(import.meta.url));
const rebuildLine =
  "2026-07-22 09:15:12 File change detected: src/components/Dashboard.tsx. Starting incremental compilation... status=rebuilding..\n";
if (Buffer.byteLength(rebuildLine, "ascii") !== 128) {
  throw new Error("the canonical rebuild line must remain exactly 128 bytes");
}
const variants = [
  ["input-1mib.log", 1 * 1024 * 1024],
  ["input-4mib.log", 4 * 1024 * 1024],
  ["input-8mib.log", 8 * 1024 * 1024],
];

const manifest = {
  generator: "node fixtures/benchmarks/mib-rebuild-stream/generate.mjs",
  variants: [],
};

for (const [name, bytes] of variants) {
  const content = rebuildLine
    .repeat(Math.ceil(bytes / rebuildLine.length))
    .slice(0, bytes);
  const path = join(outputDirectory, name);
  writeFileSync(path, content, "ascii");
  manifest.variants.push({
    name,
    bytes,
    sha256: createHash("sha256").update(content, "ascii").digest("hex"),
  });
}

writeFileSync(
  join(outputDirectory, "manifest.json"),
  `${JSON.stringify(manifest, null, 2)}\n`,
  "utf8",
);
