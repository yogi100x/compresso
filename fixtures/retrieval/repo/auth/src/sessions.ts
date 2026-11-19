export interface Session {
  id: string;
  subject: string;
  createdAt: number;
  revoked: boolean;
}

export class SessionStore {
  private readonly sessions = new Map<string, Session>();
  private counter = 0;

  createSession(subject: string, now: number): Session {
    this.counter += 1;
    const session: Session = { id: `s${this.counter}`, subject, createdAt: now, revoked: false };
    this.sessions.set(session.id, session);
    return session;
  }

  revokeSession(id: string): boolean {
    const session = this.sessions.get(id);
    if (!session || session.revoked) {
      return false;
    }
    session.revoked = true;
    return true;
  }

  isActive(id: string): boolean {
    const session = this.sessions.get(id);
    return session !== undefined && !session.revoked;
  }
}
