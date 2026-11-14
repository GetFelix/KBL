// The tracker's REST routes, called with the viewer's own person token. The page only
// talks to the tracker at its own origin (`/v1/...`): it never sends the token to an
// address taken from the link, and needs no CORS on the tracker.
import { outcome, route, type Outcome, type Request } from './writes.ts';

export type Fetch = (url: string, init: { method: string; headers: Record<string, string>; body?: string }) => Promise<{ status: number; json(): Promise<unknown> }>;

export interface Me {
  person: string;
  teams: string[];
}

export class Tracker {
  private readonly fetch: Fetch;
  private readonly token: string;
  private readonly team: string | null;

  constructor(fetch: Fetch, token: string, team: string | null) {
    this.fetch = fetch;
    this.token = token;
    this.team = team;
  }

  private async send(method: string, path: string, body?: unknown): Promise<{ status: number | null; body: unknown; network: string }> {
    try {
      const res = await this.fetch(path, {
        method,
        headers: { authorization: `Bearer ${this.token}`, ...(body === undefined ? {} : { 'content-type': 'application/json' }) },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      let parsed: unknown = null;
      try {
        parsed = await res.json();
      } catch {
        // A proxy's HTML error page, say; the status still says what happened.
      }
      return { status: res.status, body: parsed, network: '' };
    } catch (e) {
      return { status: null, body: null, network: (e as Error).message };
    }
  }

  /** Who the token is, or why the tracker would not say. */
  async me(): Promise<Me | Outcome> {
    const r = await this.send('GET', '/v1/me');
    const o = outcome(r.status, r.body, r.network);
    if (o.status !== 'ok') return o;
    const b = r.body as { person?: unknown; teams?: { team?: unknown }[] } | null;
    if (typeof b?.person !== 'string') return { status: 'failed', reason: 'The tracker answered /v1/me without a person.' };
    const teams = Array.isArray(b.teams) ? b.teams.map((t) => t.team).filter((t): t is string => typeof t === 'string') : [];
    return { person: b.person, teams };
  }

  async write(req: Request): Promise<Outcome> {
    const r = await this.send('POST', route(req.path, this.team), req.body);
    return outcome(r.status, r.body, r.network);
  }
}
