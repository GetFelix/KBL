import type { GatewayMessage, Transport } from '../src/follow.ts';

/**
 * Speaks the part of the gateway protocol the page uses: `read` pages of the
 * committed log, and `subscribe` that delivers history from an offset and then
 * live publishes. Like the real gateway, a second subscribe on the stream replaces the
 * first: nothing from the old one is delivered after the new `subscribed`, but events
 * already in flight can still arrive before it (`beforeSubscribed`). Tests hook the
 * moments the page's order depends on.
 */
export class FakeGateway implements Transport {
  onMessage: (message: GatewayMessage) => void = () => {};
  onClose: (reason: string) => void = () => {};
  log: string[] = []; // base64 payloads; the index is the offset
  pageSize = 2;
  subscribedFrom: number[] = [];
  /** Runs when a read is answered with an empty page, before the page reaches the client. */
  afterEmptyRead: () => void = () => {};
  /** Runs for each event about to be delivered; return false to drop it. */
  filterEvent: (offset: number) => boolean = () => true;
  /** Runs when a subscribe arrives, before `subscribed` is sent: in-flight events of the replaced subscription. */
  beforeSubscribed: () => void = () => {};
  /** Offsets that hold no event (a generation-start record): never delivered, counted by `skipped_before`. */
  nonEvents = new Set<number>();
  /** Makes an empty read page report a `next_offset` this far ahead of `from`. */
  emptyPageAhead = 0;
  /** Answers a read with this instead of records, when it returns a message. */
  readError: (from: number) => GatewayMessage | undefined = () => undefined;
  /** A read page starts this many offsets before `from`, as a repeating gateway might. */
  overlap = 0;
  closed = false;
  private live = false;
  private skipped = 0;

  send(m: Record<string, unknown>): void {
    queueMicrotask(() => {
      if (m.type === 'read') this.read(m.from as number, m.id as number);
      else if (m.type === 'subscribe') this.subscribe(m.from as number);
    });
  }

  close(): void {
    this.closed = true;
    this.onClose('closed');
  }

  publish(payload: string): number {
    const offset = this.log.push(payload) - 1;
    if (this.live) this.deliver(offset);
    return offset;
  }

  private read(from: number, id: number): void {
    const err = this.readError(from);
    if (err) {
      this.onMessage({ ...err, id });
      return;
    }
    const start = from >= this.log.length ? from : Math.max(0, from - this.overlap);
    const records = this.log.slice(start, from + this.pageSize).map((payload, i) => ({ offset: start + i, payload }));
    if (records.length === 0) this.afterEmptyRead();
    this.onMessage({ type: 'records', id, records, next_offset: Math.max(from, start + records.length) + (records.length === 0 ? this.emptyPageAhead : 0) });
  }

  private subscribe(from: number): void {
    this.subscribedFrom.push(from);
    this.live = false;
    this.skipped = 0;
    this.beforeSubscribed();
    this.onMessage({ type: 'subscribed', start_offset: from, live_offset: this.log.length });
    for (let o = from; o < this.log.length; o++) this.deliver(o);
    this.live = true;
  }

  private deliver(offset: number): void {
    if (this.nonEvents.has(offset)) {
      this.skipped++;
      return;
    }
    if (this.filterEvent(offset)) {
      const skipped_before = this.skipped || undefined;
      this.skipped = 0;
      this.onMessage({ type: 'event', offset, payload: this.log[offset], skipped_before });
    }
  }
}

export const record = (n: number): string => Buffer.from(JSON.stringify({ n })).toString('base64');
