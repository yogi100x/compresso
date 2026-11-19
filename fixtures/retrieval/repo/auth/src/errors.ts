export class AuthError extends Error {
  constructor(message: string, readonly status: number) {
    super(message);
  }
}

export class TokenInvalidError extends AuthError {
  constructor() {
    super("token signature does not match", 401);
  }
}

export class TokenExpiredError extends AuthError {
  constructor(readonly expiredAt: number) {
    super("token is no longer valid", 401);
  }
}

export class TooManyAttemptsError extends AuthError {
  constructor(readonly retryAfterSeconds: number) {
    super("too many attempts", 429);
  }
}

export class ForbiddenError extends AuthError {
  constructor(action: string) {
    super(`not allowed to ${action}`, 403);
  }
}
