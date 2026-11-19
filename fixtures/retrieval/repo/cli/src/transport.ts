import type { CliConfig } from "./config";

export interface Reply {
  status: number;
  body: string;
}

export type Send = (url: string, body: string, timeoutSeconds: number) => Reply;

let send: Send = () => ({ status: 503, body: "no transport installed" });

export function installTransport(next: Send): void {
  send = next;
}

export function postJson(config: CliConfig, route: string, body: string): Reply {
  return send(`${config.serverUrl}${route}`, body, config.timeoutSeconds);
}
