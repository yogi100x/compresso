import { verifyPassword } from "./passwordHash";
import { LoginRateLimiter } from "./rateLimiter";
import { SessionStore } from "./sessions";
import { issueAccessToken } from "./tokens";

export interface AuthServerConfig {
  secret: string;
  listenPort: number;
}

export interface Account {
  name: string;
  role: string;
  storedPassword: string;
}

export class AuthServer {
  private readonly limiter = new LoginRateLimiter();
  private readonly sessions = new SessionStore();

  constructor(private readonly config: AuthServerConfig, private readonly accounts: Map<string, Account>) {}

  handleLogin(name: string, password: string, now: number): string | undefined {
    this.limiter.assertLoginAllowed(name, now);
    const account = this.accounts.get(name);
    if (!account || !verifyPassword(password, account.storedPassword)) {
      this.limiter.recordFailedLogin(name, now);
      return undefined;
    }
    this.sessions.createSession(name, now);
    return issueAccessToken(name, account.role, now, this.config.secret);
  }
}
