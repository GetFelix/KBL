import type { GatewayMessage, Transport } from './follow.ts';

/** The gateway refused the join. `signed_out` and `forbidden` will not pass on a retry. */
export class GatewayError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(`${code}: ${message}`);
    this.code = code;
  }
  get fatal(): boolean {
    return this.code === 'signed_out' || this.code === 'forbidden' || this.code === 'no_read';
  }
}

/**
 * Opens a WebSocket to felix-gateway, joins `room` with an ID token and resolves
 * with a transport once the gateway answers `hello`. The `read` feature is what
 * lets the page read the log without subscribing.
 */
export function connect(url: string, room: string, token: string): Promise<Transport> {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    const transport: Transport = {
      send: (m) => ws.send(JSON.stringify(m)),
      onMessage: () => {},
      onClose: () => {},
      close: () => ws.close(),
    };
    let joined = false;
    ws.onopen = () => ws.send(JSON.stringify({ type: 'join', protocol: 1, features: ['read'], room, token }));
    ws.onmessage = (e) => {
      if (typeof e.data !== 'string') return;
      let m: GatewayMessage & { features?: string[] };
      try {
        m = JSON.parse(e.data) as GatewayMessage & { features?: string[] };
      } catch {
        m = { type: 'error', code: 'bad_frame', message: 'the gateway sent text that is not JSON' };
      }
      if (!joined) {
        if (m.type === 'hello' && m.features?.includes('read')) {
          joined = true;
          resolve(transport);
        } else {
          reject(
            m.type === 'error'
              ? new GatewayError(m.code ?? 'error', m.message ?? '')
              : new GatewayError('no_read', 'the gateway did not grant the read feature'),
          );
          ws.close();
        }
        return;
      }
      transport.onMessage(m);
    };
    ws.onclose = () => (joined ? transport.onClose('closed') : reject(new Error('the gateway closed the connection')));
    ws.onerror = () => {};
  });
}
