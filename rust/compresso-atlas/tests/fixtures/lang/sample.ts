export interface RetryPolicy {
  maxAttempts: number;
  delayMs: number;
}

export class HttpClient {
  async get(url: string): Promise<Response> {
    return fetch(url);
  }
}

export const parseUrl = (s: string) => new URL(s);
