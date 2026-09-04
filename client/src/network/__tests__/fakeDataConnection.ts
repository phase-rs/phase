/**
 * Minimal fake matching the subset of PeerJS's `DataConnection` API surface
 * that `createPeerSession` and the P2P adapter integration tests use.
 *
 * Shared between protocol unit tests (`peer.test.ts`) and adapter integration
 * tests (`p2p-adapter-multiplayer.test.ts`).
 *
 * ## Wire-format note
 *
 * Production `peer.ts` now encodes messages to gzipped `Uint8Array` before
 * `conn.send`. This fake decodes on send / re-encodes on receive so tests can
 * continue to assert on plain-object `sent` entries and pass plain-object
 * messages to `simulateData`. Tests that want to verify the actual wire bytes
 * can read `sentRaw` instead.
 */

import type { P2PMessage } from "../protocol";
import { decodeWireMessage, encodeWireMessage } from "../protocol";

type DataHandler = (data: unknown) => void;
type VoidHandler = () => void;
type ErrorHandler = (err: Error) => void;

export class FakeDataConnection {
  open = true;
  /**
   * Decoded messages for ergonomic assertions in existing tests. Typed as
   * `unknown[]` because tests frequently narrow with custom type guards to
   * check wire-format-specific fields like `legalActionsByObject` that live
   * only on specific P2PMessage variants.
   */
  sent: unknown[] = [];
  /** Raw wire bytes, populated in parallel with `sent`. Use to assert on encoding. */
  sentRaw: Uint8Array[] = [];

  private dataHandlers = new Set<DataHandler>();
  private closeHandlers = new Set<VoidHandler>();
  private errorHandlers = new Set<ErrorHandler>();

  send(data: unknown) {
    if (!this.open) throw new Error("Connection is closed");
    if (data instanceof Uint8Array) {
      this.sentRaw.push(data);
      // Synchronously push a placeholder so `.sent.length` stays accurate,
      // then backfill on decode. Tests asserting on message content should
      // `await fake.getSentMessages()` to wait for all backfills.
      const idx = this.sent.length;
      this.sent.push({ type: "__pending__" });
      const decodePromise = decodeWireMessage(data).then(
        (msg) => {
          this.sent[idx] = msg;
          // Fire AFTER the backfill so a subclass reaction (e.g. an
          // auto-ack) can never appear in `sent` before the frame that
          // provoked it.
          this.onDecodedSend(msg);
        },
        (err) => { console.warn("[FakeDataConnection] decode failed:", err); },
      );
      this.pendingDecodes.push(decodePromise);
    } else {
      // Legacy-compatibility path for tests that still pass raw objects
      // through (e.g., tests written before the binary wire format).
      this.sent.push(data);
    }
  }

  /**
   * Wait for all in-flight decodes to settle, then return the decoded `sent`
   * array. Returns `unknown[]` so callers can apply custom `is T` type
   * guards that narrow on fields specific to a single P2PMessage variant.
   *
   * Fixed-point loop: handlers running on settled decodes may push more
   * sends, which in turn enqueue more decodes. Drain until the queue is
   * empty AND no new entries were added by the most recent drain.
   *
   * Tests must `await` the originating adapter operation first so that
   * `peer.ts`'s send queue has flushed bytes through to `conn.send` —
   * `getSentMessages` only awaits decode-side work, not the encode chain.
   */
  async getSentMessages(): Promise<unknown[]> {
    while (this.pendingDecodes.length > 0) {
      const drain = this.pendingDecodes.splice(0);
       
      await Promise.allSettled(drain);
    }
    return this.sent;
  }

  /**
   * Decode promises in flight from the most recent `send(Uint8Array)` calls.
   * `protected` so subclasses that react to a decoded frame can enqueue their
   * own follow-on work and have `getSentMessages()` drain it.
   */
  protected pendingDecodes: Promise<void>[] = [];

  /**
   * Hook fired once a sent frame has been decoded and backfilled into `sent`.
   * Subclasses override it to model peer behavior (e.g. feeding a `state_ack`
   * back to the host); an override MUST call `super.onDecodedSend(msg)` so the
   * base keep-alive reply below survives — unless it is deliberately modelling
   * a peer that has stopped responding, as `SilentPeerConnection` in
   * `peer.test.ts` does. Nothing enforces this. Overrides that start async work
   * should push the resulting promise onto `pendingDecodes` so
   * `getSentMessages()` awaits it.
   *
   * Default behavior: answer `ping` with `pong`. A live channel always does,
   * and `peer.ts` now disconnects a session that sees no pong for 10s — so a
   * fake that never ponged would model a DEAD peer, which is not what any
   * caller of this class means by "a connection". The reply goes out through
   * `simulateData` into the session's receive path, so it never appears in
   * `sent`; tests asserting on `sent` are unaffected.
   */
  protected onDecodedSend(msg: P2PMessage): void {
    if (msg.type !== "ping" || !this.open) return;
    this.pendingDecodes.push(
      this.simulateData({ type: "pong", timestamp: msg.timestamp }).catch((e) => {
        console.warn("[FakeDataConnection] pong dispatch failed:", e);
      }),
    );
  }

  close() {
    if (this.open) this.simulateClose();
  }

  on(event: string, handler: (...args: unknown[]) => void): this {
    if (event === "data") this.dataHandlers.add(handler as DataHandler);
    else if (event === "close") this.closeHandlers.add(handler as VoidHandler);
    else if (event === "error") this.errorHandlers.add(handler as ErrorHandler);
    return this;
  }

  off(event: string, handler: (...args: unknown[]) => void): this {
    if (event === "data") this.dataHandlers.delete(handler as DataHandler);
    else if (event === "close") this.closeHandlers.delete(handler as VoidHandler);
    else if (event === "error") this.errorHandlers.delete(handler as ErrorHandler);
    return this;
  }

  // ── Test helpers ──
  /**
   * Dispatch data to registered handlers. The returned Promise resolves
   * after `peer.ts`'s recvQueue has fully decoded the message AND any
   * handler-triggered async work has settled (handlers are awaited inside
   * the recvQueue chain). Raw `Uint8Array` / `ArrayBuffer` payloads are
   * forwarded as-is to simulate pre-encoded messages from the peer; plain
   * objects are encoded first so the real decode path is exercised.
   */
  async simulateData(data: unknown): Promise<void> {
    let bytes: Uint8Array;
    if (data instanceof Uint8Array) bytes = data;
    else if (data instanceof ArrayBuffer) bytes = new Uint8Array(data);
    else bytes = await encodeWireMessage(data as P2PMessage);
    // peer.ts's onData returns its recvQueue chain promise; collect and
    // await all handler completions (which include async message handlers
    // awaited inside the recvQueue) before resolving.
    const chains: Promise<unknown>[] = [];
    for (const h of this.dataHandlers) {
      const r = h(bytes);
      if (r !== undefined) chains.push(r as Promise<unknown>);
    }
    if (chains.length > 0) await Promise.allSettled(chains);
  }

  simulateClose() {
    this.open = false;
    for (const h of this.closeHandlers) h();
  }

  simulateError(err: Error) {
    for (const h of this.errorHandlers) h(err);
  }
}
