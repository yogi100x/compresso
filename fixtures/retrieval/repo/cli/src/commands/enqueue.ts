import type { CliConfig } from "../config";
import type { ParsedArguments } from "../main";
import { printError, printLine } from "../output";
import { postJson } from "../transport";

export type ReadText = (location: string) => string;

let readText: ReadText = () => "";

export function installReader(next: ReadText): void {
  readText = next;
}

export function enqueueCommand(parsed: ParsedArguments, config: CliConfig): number {
  const [kind, payloadFile] = parsed.positional;
  if (!kind || !payloadFile) {
    printError("usage: enqueue <kind> <payload-file>");
    return 2;
  }
  const payload = readText(payloadFile);
  const reply = postJson(config, "/jobs", JSON.stringify({ kind, payload }));
  if (reply.status !== 202) {
    printError(`server answered ${reply.status}: ${reply.body}`);
    return 1;
  }
  printLine(reply.body);
  return 0;
}
