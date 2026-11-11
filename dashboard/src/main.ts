// Connect to felix-gateway, fold work.events with the core crate as WebAssembly,
// and list the items with their stage. Settings come from the query string:
// ?gateway=ws://127.0.0.1:8787/ws&room=kibble&token=<ID token>&stream=events
import init, { Folder } from './wasm/agents_wasm.js';
import { items, type Fold } from './fold.ts';
import { follow, FollowError } from './follow.ts';
import { connect, GatewayError } from './gateway.ts';

const status = document.getElementById('status')!;
const list = document.getElementById('items')!;

function render(fold: Fold) {
  list.replaceChildren(
    ...items(fold).map((item) => {
      const li = document.createElement('li');
      for (const [cls, text] of [['id', item.id], ['stage', item.stage.replaceAll('_', ' ')], ['title', item.title]]) {
        const span = document.createElement('span');
        span.className = cls;
        span.textContent = text;
        li.append(span);
      }
      return li;
    }),
  );
}

async function main() {
  const q = new URLSearchParams(location.search);
  const url = q.get('gateway') ?? 'ws://127.0.0.1:8787/ws';
  const room = q.get('room');
  const token = q.get('token');
  // Keep the token in memory only: out of the address bar, history and any shared link.
  if (token) {
    q.delete('token');
    history.replaceState(null, '', `${location.pathname}${q.size ? `?${q}` : ''}${location.hash}`);
  }
  if (!room || !token) {
    status.textContent = 'Add ?room=...&token=... to the address to connect.';
    return;
  }
  await init();
  const fold = new Folder();
  const sink: Fold = {
    apply: (o, p) => fold.apply(o, p),
    at: () => fold.at(),
    stateJson: () => fold.state_json(),
    itemsJson: () => fold.items_json(),
  };
  // Redraw once per frame however many records arrive.
  let queued = false;
  const redraw = () => {
    if (queued) return;
    queued = true;
    requestAnimationFrame(() => {
      queued = false;
      render(sink);
    });
  };
  let cursor = 0;
  let refused = 0;
  let refusedBefore = 0; // refusals from earlier connections; follow counts per connection
  const live = () =>
    `Live, ${items(sink).length} items${refused ? `, ${refused} records this build does not read` : ''}`;
  let delay = 1000; // grows on failures, resets once a connection goes live
  for (;;) {
    try {
      status.textContent = cursor === 0 ? 'Reading history' : 'Reconnecting';
      const transport = await connect(url, room, token);
      refusedBefore = refused;
      cursor = await follow(transport, sink, {
        stream: q.get('stream') ?? 'events',
        from: cursor, // past refused records too, which the fold does not count
        onApplied: redraw,
        onRefused: (n) => {
          refused = refusedBefore + n;
          if (status.textContent?.startsWith('Live')) status.textContent = live();
        },
        onLive: () => {
          delay = 1000;
          status.textContent = live();
        },
      });
    } catch (e) {
      const msg = (e as Error).message;
      // A rejected or expired token, or a refused request, will not pass on a retry.
      if (e instanceof FollowError || (e instanceof GatewayError && e.fatal)) {
        status.textContent = `Stopped: ${msg}`;
        return;
      }
      status.textContent = `Disconnected: ${msg}. Retrying in ${Math.round(delay / 1000)} s`;
    }
    await new Promise((r) => setTimeout(r, delay));
    delay = Math.min(delay * 2, 30000);
  }
}

void main();
