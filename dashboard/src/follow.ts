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
  /** Called with the running count of records the sink refused (not a record this build reads). */
  onRefused?: (total: number) => void;
}

export class FollowError extends Error {}

/** Codes a retry cannot fix: the log no longer holds the offset, or the request or sign-in is refused. */
const FATAL_CODES = new Set(['trimmed', 'bad_request', 'past_end', 'forbidden', 'signed_out', 'bad_frame']);

export function decodeBase64(text: string): Uint8Array {
  const bin = atob(text);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/**
 * Follows the stream until the connection ends. Resolves with the cursor, the
 * offset to pass as `from` on a reconnect, also when a transient gateway error
 * ended it; rejects on an error a retry cannot fix (`trimmed`, `bad_request`, ...).
 */
export function follow(transport: Transport, sink: Sink, opts: FollowOptions): Promise<number> {
  return new Promise((resolve, reject) => {
    let cursor = opts.from ?? 0; // the next offset wanted
    let nextId = 1;
    let phase: 'reading' | 'subscribing' | 'live' = 'reading';
    let outstanding = 0;
    let lastEvent: number | null = null; // last event offset seen on this subscription
    let refused = 0;
    let done = false;

    const fail = (error: string) => {
      if (done) return;
      done = true;
      reject(new FollowError(error)); // before close, whose handler would resolve
      transport.close();
    };
    const apply = (offset: number, payload: string) => {
      if (offset < cursor) return; // already applied
      let bytes: Uint8Array;
      try {
        bytes = decodeBase64(payload);
      } catch {
        fail(`record ${offset} is not base64`);
        return;
      }
      // A record the fold does not read is counted and skipped; the cursor moves past it.
      if (!sink.apply(offset, bytes)) opts.onRefused?.(++refused);
      cursor = offset + 1;
      opts.onApplied?.(offset);
    };
    const readPage = () => {
      outstanding = nextId++;
      transport.send({ type: 'read', stream: opts.stream, from: cursor, id: outstanding });
    };
    // The gateway has no unsubscribe: a second subscribe on the same stream replaces the first.
    // Events that arrive before the new `subscribed` belong to the replaced one and are ignored
    // (phase is not 'live'), so they cannot trigger another resubscribe.
    const subscribe = () => {
      phase = 'subscribing';
      lastEvent = null;
      transport.send({ type: 'subscribe', stream: opts.stream, from: cursor });
    };

    transport.onClose = () => {
      if (!done) resolve(cursor);
    };
    transport.onMessage = (m) => {
      if (done) return;
      if (m.type === 'error') {
        const code = m.code ?? 'error';
        if (code === 'subscription_ended' && phase === 'live') {
          // A broker failover or close: the protocol says to subscribe again from the last offset + 1.
          subscribe();
        } else if (FATAL_CODES.has(code)) {
          fail(`${code}: ${m.message ?? ''}`);
        } else if (!done) {
          // read_failed, subscribe_failed and the rest may pass: hand the cursor back so the caller
          // reconnects with a backoff.
          done = true;
          transport.close();
          resolve(cursor);
        }
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
