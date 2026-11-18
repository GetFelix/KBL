// Which gateway the page may send a token to. A token goes only to the gateway its own
// link named (or the local default), never to one a later link names on its own.
import type { GatewaySession } from './session.ts';

export const DEFAULT_GATEWAY = 'ws://127.0.0.1:8787/ws';

const LOCAL = new Set(['localhost', '127.0.0.1', '[::1]']);

/** The URL as given if it is a usable gateway address: wss:, or ws: for this machine only. */
export function validGateway(raw: string | null | undefined): string | null {
  if (!raw) return null;
  let u: URL;
  try {
    u = new URL(raw);
  } catch {
    return null;
  }
  if (u.username || u.password || u.hash) return null;
  if (u.protocol === 'wss:') return raw;
  return u.protocol === 'ws:' && LOCAL.has(u.hostname) ? raw : null;
}

export type Pick =
  | { kind: 'session'; gateway: string | null; room: string; token: string; fresh: boolean }
  | { kind: 'refused'; reason: string }
  | { kind: 'none' };

/**
 * Decides the session from the address and the tab's saved one. A gateway is taken from
 * the address only together with a token (and room) in the same link; a link with a gateway
 * alone keeps the saved session's gateway.
 */
export function pickSession(q: URLSearchParams, saved: GatewaySession | null): Pick {
  const token = q.get('token');
  const room = q.get('room');
  if (token && room) {
    const asked = q.get('gateway');
    if (asked !== null && !validGateway(asked)) {
      return { kind: 'refused', reason: 'The link names a gateway address that is not allowed: use wss://, or ws:// for localhost only.' };
    }
    return { kind: 'session', gateway: asked, room, token, fresh: true };
  }
  if (!saved) return { kind: 'none' };
  const gateway = saved.gateway === null ? null : validGateway(saved.gateway);
  if (saved.gateway !== null && !gateway) return { kind: 'none' };
  // The room may come from the address; the gateway and token never do.
  return { kind: 'session', gateway, room: q.get('room') ?? saved.room, token: saved.token, fresh: false };
}

/** The host to show next to the connection status. */
export function gatewayHost(url: string | null): string {
  try {
    return new URL(url ?? DEFAULT_GATEWAY).host;
  } catch {
    return '';
  }
}
