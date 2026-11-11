// Connect to felix-gateway, fold work.events with the core crate as WebAssembly,
// and list the items with their stage. Settings come from the query string:
// ?gateway=ws://127.0.0.1:8787/ws&room=kibble&token=<ID token>&stream=events
import init, { Folder } from './wasm/agents_wasm.js';
import { items, type Fold } from './fold.ts';
import { follow, FollowError } from './follow.ts';
import { connect } from './gateway.ts';

const status = document.getElementById('status')!;
const list = document.getElementById('items')!;

function render(fold: Fold) {
  list.replaceChildren(
    ...items(fold).map((item) => {
      const li = document.createElement('li');
      for (const [cls, text] of [['id', item.id], ['stage', item.stage.replace('_', ' ')], ['title', item.title]]) {
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
  let cursor = 0;
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
  for (;;) {
    try {
      status.textContent = cursor === 0 ? 'Reading history' : 'Reconnecting';
      const transport = await connect(url, room, token);
      cursor = await follow(transport, sink, {
        stream: q.get('stream') ?? 'events',
        from: cursor,
        onApplied: redraw,
        onLive: () => (status.textContent = `Live, ${items(sink).length} items`),
      });
    } catch (e) {
      status.textContent = e instanceof FollowError ? `Stopped: ${e.message}` : `Disconnected: ${(e as Error).message}`;
      if (e instanceof FollowError) return;
    }
    await new Promise((r) => setTimeout(r, 2000));
  }
}

void main();
