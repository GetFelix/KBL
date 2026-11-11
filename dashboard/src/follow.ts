// Reads work.events from the start, then follows it, through felix-gateway's
// browser protocol (docs/protocol.md in GetFelix/felix-gateway).
//
// The order matters. Read until a page comes back empty, then subscribe from the
// offset after the last record read. A record published between the two is in
// the log by the time the subscribe registers, so the subscribe delivers it as
// history: nothing is missed and nothing is read twice. The cursor below also
// drops any offset already applied, so a replayed event cannot reach the fold.

export interface Transport {
  send(message: Record<string, unknown>): void;
  /** Set by `follow`. Called once per message from the gateway. */
  onMessage: (message: GatewayMessage) => void;
  /** Set by `follow`. Called when the connection ends. */
  onClose: (reason: string) => void;
  close(): void;
}

export interface GatewayMessage {
  type: string;
  id?: number;
  code?: string;
  message?: string;
  records?: { offset: number; payload: string }[];
  next_offset?: number;
  offset?: number;
  payload?: string;
  start_offset?: number | null;
  live_offset?: number | null;
  skipped_before?: number;
}

export interface Sink {
  apply(offset: number, payload: Uint8Array): boolean;
}

export interface FollowOptions {
  /** The gateway's alias for work.events. */
  stream: string;
  /** Where to start; 0 for a fresh page, the cursor after a reconnect. */
  from?: number;
  /** Called once the history is folded and the subscription is live. */
  onLive?: () => void;
  /** Called after each record reaches the sink. */
  onApplied?: (offset: number) => void;
}

export class FollowError extends Error {}

export function decodeBase64(text: string): Uint8Array {
  const bin = atob(text);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/**
 * Follows the stream until the connection ends. Resolves with the cursor, the
 * offset to pass as `from` on a reconnect; rejects if the gateway refuses a
 * request.
 */
export function follow(transport: Transport, sink: Sink, opts: FollowOptions): Promise<number> {
  return new Promise((resolve, reject) => {
    let cursor = opts.from ?? 0; // the next offset wanted
    let nextId = 1;
    let phase: 'reading' | 'subscribing' | 'live' = 'reading';
    let outstanding = 0;
    let lastEvent: number | null = null; // last event offset seen on this subscription

    const fail = (error: string) => {
      reject(new FollowError(error)); // before close, whose handler would resolve
      transport.close();
    };
    const apply = (offset: number, payload: string) => {
      if (offset < cursor) return; // already applied
      // An undecodable record is skipped, not fatal: the cursor still moves past it.
      sink.apply(offset, decodeBase64(payload));
      cursor = offset + 1;
      opts.onApplied?.(offset);
    };
    const readPage = () => {
      outstanding = nextId++;
      transport.send({ type: 'read', stream: opts.stream, from: cursor, id: outstanding });
    };
    const subscribe = () => {
      phase = 'subscribing';
      lastEvent = null;
      transport.send({ type: 'subscribe', stream: opts.stream, from: cursor });
    };

    transport.onClose = () => resolve(cursor);
    transport.onMessage = (m) => {
      if (m.type === 'error') {
        // A read or subscribe that fails ends the follow; the caller decides whether to retry.
        fail(`${m.code ?? 'error'}: ${m.message ?? ''}`);
      } else if (m.type === 'records' && phase === 'reading' && m.id === outstanding) {
        const records = m.records ?? [];
        for (const r of records) apply(r.offset, r.payload);
        cursor = Math.max(cursor, m.next_offset ?? cursor);
        // A page never waits, so an empty one means we have reached what is committed.
        if (records.length === 0) subscribe();
        else readPage();
      } else if (m.type === 'subscribed' && phase === 'subscribing') {
        if (m.start_offset !== null && m.start_offset !== cursor) {
          fail(`subscribed from ${m.start_offset}, asked for ${cursor}`);
          return;
        }
        phase = 'live';
        opts.onLive?.();
      } else if (m.type === 'event' && phase === 'live') {
        const offset = m.offset;
        if (offset === undefined || offset === null || m.payload === undefined) return;
        if (offset < cursor) return;
        // Records the subscription skipped are counted by skipped_before; any more is a drop.
        const expected = lastEvent === null ? cursor : lastEvent + 1;
        if (offset - expected - (m.skipped_before ?? 0) > 0) {
          subscribe(); // resume from the cursor; the replaced subscription's events are ignored
          return;
        }
        lastEvent = offset;
        apply(offset, m.payload);
      }
    };
    readPage();
  });
}
