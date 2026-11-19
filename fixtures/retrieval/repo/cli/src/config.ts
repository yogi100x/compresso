export interface CliConfig {
  serverUrl: string;
  timeoutSeconds: number;
}

const DEFAULT_SERVER_URL = "http://127.0.0.1:7400";

export function resolveServerUrl(
  flags: Map<string, string>,
  environment: Record<string, string | undefined>,
): string {
  return flags.get("server") ?? environment.JOBS_SERVER_URL ?? DEFAULT_SERVER_URL;
}

export function loadCliConfig(
  flags: Map<string, string>,
  environment: Record<string, string | undefined>,
): CliConfig {
  return {
    serverUrl: resolveServerUrl(flags, environment),
    timeoutSeconds: Number(flags.get("timeout") ?? "10"),
  };
}
