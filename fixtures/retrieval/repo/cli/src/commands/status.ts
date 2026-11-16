import type { CliConfig } from "../config";
import { formatTable, printLine } from "../output";
import { postJson } from "../transport";

function paint(text: string, environment: Record<string, string | undefined>): string {
  // Coloured output is turned off whenever NO_COLOR is set, whatever its value.
  if (environment.NO_COLOR !== undefined) {
    return text;
  }
  return `\u001b[32m${text}\u001b[0m`;
}

export function statusCommand(config: CliConfig, environment: Record<string, string | undefined>): number {
  const reply = postJson(config, "/status", "{}");
  if (reply.status !== 200) {
    return 1;
  }
  const depth = String((JSON.parse(reply.body) as { depth: number }).depth);
  printLine(formatTable([["queue depth", paint(depth, environment)]]));
  return 0;
}
