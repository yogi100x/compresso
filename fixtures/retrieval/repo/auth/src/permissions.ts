import { ForbiddenError } from "./errors";

export type Action = "enqueue" | "cancel" | "inspect" | "purge";

const ROLE_GRANTS: Record<string, Action[]> = {
  viewer: ["inspect"],
  operator: ["inspect", "enqueue", "cancel"],
  admin: ["inspect", "enqueue", "cancel", "purge"],
};

export function hasPermission(role: string, action: Action): boolean {
  return (ROLE_GRANTS[role] ?? []).includes(action);
}

export function requirePermission(role: string, action: Action): void {
  if (!hasPermission(role, action)) {
    throw new ForbiddenError(action);
  }
}
