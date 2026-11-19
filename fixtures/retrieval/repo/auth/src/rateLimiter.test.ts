import { TooManyAttemptsError } from "./errors";
import { LoginRateLimiter } from "./rateLimiter";

declare function test(name: string, body: () => void): void;
declare function expect(value: unknown): { toThrow(kind: unknown): void };

export function failRepeatedly(limiter: LoginRateLimiter, account: string, times: number): void {
  for (let attempt = 0; attempt < times; attempt += 1) {
    limiter.recordFailedLogin(account, 100 + attempt);
  }
}

test("blocks login after the failed attempt limit", () => {
  const limiter = new LoginRateLimiter();
  failRepeatedly(limiter, "ada", 5);
  expect(() => limiter.assertLoginAllowed("ada", 110)).toThrow(TooManyAttemptsError);
});
