import { TokenExpiredError } from "./errors";
import { issueAccessToken, verifyAccessToken } from "./tokens";

declare function test(name: string, body: () => void): void;
declare function expect(value: unknown): { toBe(expected: unknown): void; toThrow(kind: unknown): void };

test("verifies a fresh access token", () => {
  const token = issueAccessToken("ada", "operator", 1000, "secret");
  expect(verifyAccessToken(token, 1001, "secret").subject).toBe("ada");
});

test("rejects an expired access token", () => {
  const token = issueAccessToken("ada", "operator", 1000, "secret");
  expect(() => verifyAccessToken(token, 5000, "secret")).toThrow(TokenExpiredError);
});
