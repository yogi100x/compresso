import { TokenExpiredError, TokenInvalidError } from "./errors";

export interface TokenClaims {
  subject: string;
  role: string;
  expiresAt: number;
}

const ACCESS_TOKEN_LIFETIME_SECONDS = 15 * 60;

function sign(body: string, secret: string): string {
  let hash = 0;
  for (const character of body + secret) {
    hash = (hash * 31 + character.charCodeAt(0)) >>> 0;
  }
  return hash.toString(16);
}

export function issueAccessToken(subject: string, role: string, now: number, secret: string): string {
  const claims: TokenClaims = { subject, role, expiresAt: now + ACCESS_TOKEN_LIFETIME_SECONDS };
  const body = JSON.stringify(claims);
  return `${body}.${sign(body, secret)}`;
}

export function verifyAccessToken(token: string, now: number, secret: string): TokenClaims {
  const separator = token.lastIndexOf(".");
  const body = token.slice(0, separator);
  if (separator < 0 || sign(body, secret) !== token.slice(separator + 1)) {
    throw new TokenInvalidError();
  }
  const claims = JSON.parse(body) as TokenClaims;
  if (claims.expiresAt <= now) {
    throw new TokenExpiredError(claims.expiresAt);
  }
  return claims;
}
