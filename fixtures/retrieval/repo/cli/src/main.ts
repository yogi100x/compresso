import { enqueueCommand } from "./commands/enqueue";
import { statusCommand } from "./commands/status";
import { loadCliConfig } from "./config";
import { printError } from "./output";

export interface ParsedArguments {
  command: string;
  positional: string[];
  flags: Map<string, string>;
}

export function parseArguments(argv: string[]): ParsedArguments {
  const flags = new Map<string, string>();
  const positional: string[] = [];
  for (const argument of argv.slice(1)) {
    if (argument.startsWith("--")) {
      const [name, value = "true"] = argument.slice(2).split("=");
      flags.set(name, value);
    } else {
      positional.push(argument);
    }
  }
  return { command: argv[0] ?? "help", positional, flags };
}

export function runCli(argv: string[], environment: Record<string, string | undefined>): number {
  const parsed = parseArguments(argv);
  const config = loadCliConfig(parsed.flags, environment);
  switch (parsed.command) {
    case "enqueue":
      return enqueueCommand(parsed, config);
    case "status":
      return statusCommand(config, environment);
    default:
      printError(`unknown command ${parsed.command}`);
      return 2;
  }
}
