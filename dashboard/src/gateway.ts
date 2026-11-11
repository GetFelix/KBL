import type { GatewayMessage, Transport } from './follow.ts';

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
      const m = JSON.parse(e.data) as GatewayMessage & { features?: string[] };
      if (!joined) {
        if (m.type === 'hello' && m.features?.includes('read')) {
          joined = true;
          resolve(transport);
        } else {
          reject(new Error(m.type === 'error' ? `${m.code}: ${m.message}` : 'the gateway did not grant the read feature'));
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
