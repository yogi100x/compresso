import { TooManyAttemptsError } from "./errors";

const WINDOW_SECONDS = 300;
const MAX_FAILURES = 5;

export class LoginRateLimiter {
  private readonly failures = new Map<string, number[]>();

  recordFailedLogin(account: string, now: number): void {
    const recent = this.recentFailures(account, now);
    recent.push(now);
    this.failures.set(account, recent);
  }

  assertLoginAllowed(account: string, now: number): void {
    const recent = this.recentFailures(account, now);
    if (recent.length >= MAX_FAILURES) {
      throw new TooManyAttemptsError(recent[0] + WINDOW_SECONDS - now);
    }
  }

  private recentFailures(account: string, now: number): number[] {
    return (this.failures.get(account) ?? []).filter((at) => now - at < WINDOW_SECONDS);
  }
}
