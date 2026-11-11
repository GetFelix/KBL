import type { GatewayMessage, Transport } from '../src/follow.ts';

/**
 * Speaks the part of the gateway protocol the page uses: `read` pages of the
 * committed log, and `subscribe` that delivers history from an offset and then
 * live publishes. Tests hook the moments the page's order depends on.
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
  private live = false;

  send(m: Record<string, unknown>): void {
    queueMicrotask(() => {
      if (m.type === 'read') this.read(m.from as number, m.id as number);
      else if (m.type === 'subscribe') this.subscribe(m.from as number);
    });
  }

  close(): void {
    this.onClose('closed');
  }

  publish(payload: string): number {
    const offset = this.log.push(payload) - 1;
    if (this.live) this.deliver(offset);
    return offset;
  }

  private read(from: number, id: number): void {
    const records = this.log.slice(from, from + this.pageSize).map((payload, i) => ({ offset: from + i, payload }));
    if (records.length === 0) this.afterEmptyRead();
    this.onMessage({ type: 'records', id, records, next_offset: from + records.length });
  }

  private subscribe(from: number): void {
    this.subscribedFrom.push(from);
    this.live = false;
    this.onMessage({ type: 'subscribed', start_offset: from, live_offset: this.log.length });
    for (let o = from; o < this.log.length; o++) this.deliver(o);
    this.live = true;
  }

  private deliver(offset: number): void {
    if (this.filterEvent(offset)) this.onMessage({ type: 'event', offset, payload: this.log[offset] });
  }
}

export const record = (n: number): string => Buffer.from(JSON.stringify({ n })).toString('base64');
