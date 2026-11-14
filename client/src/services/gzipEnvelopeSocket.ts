import { decodeJsonEnvelope, encodeJsonEnvelope } from "../network/wireEnvelope";
import type { PhaseSocketTransport } from "./openPhaseSocket";

type MessageListener = (event: MessageEvent<string>) => void;

/** Serializes async envelope work without reordering socket events. */
export class GzipEnvelopeSocket implements PhaseSocketTransport {
  onopen: ((event: Event) => void) | null = null;
  onmessage: MessageListener | null = null;
  onerror: ((event: Event) => void) | null = null;
  onclose: ((event: CloseEvent) => void) | null = null;

  private readonly messageListeners = new Map<MessageListener, boolean>();
  private readonly closeListeners = new Map<(event: CloseEvent) => void, boolean>();
  private sendQueue = Promise.resolve();
  // Pending payloads belong to this owner, not Promise callbacks behind an
  // active inflater. Retirement can release them even if that inflater stalls.
  private readonly pendingReceives: unknown[] = [];
  private nextReceive = 0;
  private receiving = false;
  private rawCloseEvent: CloseEvent | null = null;
  private retired = false;
  private closeNotified = false;

  constructor(private readonly socket: PhaseSocketTransport, private readonly onFrameSent?: () => void) {
    if ("binaryType" in socket) {
      (socket as PhaseSocketTransport & { binaryType: BinaryType }).binaryType = "arraybuffer";
    }
    socket.onopen = (event) => {
      if (!this.retired && this.rawCloseEvent === null) this.onopen?.(event);
    };
    socket.onerror = (event) => {
      if (!this.retired && this.rawCloseEvent === null) this.onerror?.(event);
    };
    socket.onclose = (event) => {
      if (this.rawCloseEvent !== null) return;
      this.rawCloseEvent = event;
      // Raw death stops admissions, but healthy pre-death work still drains.
      // Explicit retirement instead notifies without waiting for active work.
      if (this.retired) queueMicrotask(() => this.notifyClose());
      else this.startReceiveDrain();
    };
    socket.onmessage = (event) => {
      if (this.retired || this.rawCloseEvent !== null) return;
      this.pendingReceives.push(event.data);
      this.startReceiveDrain();
    };
  }

  get readyState(): number {
    return this.socket.readyState;
  }

  send(data: string): void {
    if (this.retired || this.rawCloseEvent !== null) return;
    this.enqueueWrite(data).catch(() => undefined);
  }

  /**
   * Ordered write receipt for setup: resolves after this frame's encode and
   * raw write settle, or rejects when either fails. Failure still travels the
   * transport's own error/close path exactly as `send` does, so callers see
   * both the receipt and the transport closing.
   */
  sendReceipt(data: string, signal?: AbortSignal): Promise<void> {
    if (this.retired || this.rawCloseEvent !== null) {
      return Promise.reject(new Error("gzip envelope transport is retired"));
    }
    return this.enqueueWrite(data, signal);
  }

  /** Appends one write to the shared FIFO and reports its settled outcome. */
  private enqueueWrite(data: string, signal?: AbortSignal): Promise<void> {
    const isAborted = () => signal?.aborted === true;
    const write = this.sendQueue.then(async () => {
      if (this.retired || this.rawCloseEvent !== null || isAborted()) {
        throw new Error("gzip envelope transport is retired");
      }
      const encoded = await encodeJsonEnvelope(data);
      if (this.retired || this.rawCloseEvent !== null || isAborted()) {
        throw new Error("gzip envelope transport is retired");
      }
      if (this.socket.readyState !== WebSocket.OPEN) {
        // A socket that is closing may silently drop a frame; a receipt must
        // never report a write that the browser declined to transmit.
        throw new Error("gzip envelope transport is not open");
      }
      (this.socket as unknown as { send(data: Uint8Array): void }).send(encoded);
      this.onFrameSent?.();
    });
    this.sendQueue = write
      .catch(() => {
        if (this.retired || this.rawCloseEvent !== null) return;
        // The close is what `withReconnect` recovers from, so it must survive a
        // throwing error handler: `ws-adapter`'s `emit` does not catch.
        try {
          this.onerror?.(new Event("error"));
        } finally {
          this.close();
        }
      })
      .catch(() => undefined);
    return write;
  }

  close(): void {
    if (this.retired) return;
    this.retired = true;
    this.clearPendingReceives();
    if (this.rawCloseEvent !== null) {
      queueMicrotask(() => this.notifyClose());
    } else if (this.socket.readyState !== WebSocket.CLOSED) {
      // A raw capture listener can retire us before our onclose receives the
      // original event. Do not close that already-dead transport a second time.
      this.socket.close();
    }
  }

  addEventListener(
    type: "close",
    listener: (event: CloseEvent) => void,
    options?: AddEventListenerOptions | boolean,
  ): void;
  addEventListener(
    type: "message",
    listener: MessageListener,
    options?: AddEventListenerOptions | boolean,
  ): void;
  addEventListener(
    type: "close" | "message",
    listener: ((event: CloseEvent) => void) | MessageListener,
    options?: AddEventListenerOptions | boolean,
  ): void {
    if (type === "message") {
      const once = typeof options === "object" && options.once === true;
      this.messageListeners.set(listener as MessageListener, once);
    } else {
      const once = typeof options === "object" && options.once === true;
      this.closeListeners.set(listener as (event: CloseEvent) => void, once);
    }
  }

  removeEventListener(
    type: "close",
    listener: (event: CloseEvent) => void,
  ): void;
  removeEventListener(type: "message", listener: MessageListener): void;
  removeEventListener(
    type: "close" | "message",
    listener: ((event: CloseEvent) => void) | MessageListener,
  ): void {
    if (type === "message") {
      this.messageListeners.delete(listener as MessageListener);
    } else {
      this.closeListeners.delete(listener as (event: CloseEvent) => void);
    }
  }

  private startReceiveDrain(): void {
    if (this.receiving) return;
    this.receiving = true;
    queueMicrotask(() => { void this.drainReceives().catch(() => undefined); });
  }

  private async drainReceives(): Promise<void> {
    try {
      while (!this.retired && this.nextReceive < this.pendingReceives.length) {
        const data = this.pendingReceives[this.nextReceive];
        this.pendingReceives[this.nextReceive++] = undefined;
        if (this.nextReceive === this.pendingReceives.length) this.clearPendingReceives();
        else if (this.nextReceive * 2 >= this.pendingReceives.length) {
          // Keep storage proportional to the live tail, not cumulative traffic.
          // Moving only after half is consumed makes the total copying linear.
          this.pendingReceives.copyWithin(0, this.nextReceive);
          this.pendingReceives.length -= this.nextReceive;
          this.nextReceive = 0;
        }
        let json: string;
        try {
          json = await this.decodeIncoming(data);
        } catch {
          if (this.retired) return;
          // A corrupt frame retires its queued successors. After raw death,
          // preserve the original close without another error or raw close.
          try {
            if (this.rawCloseEvent === null) this.onerror?.(new Event("error"));
          } finally {
            this.close();
          }
          return;
        }
        if (this.retired) return;
        const decoded = new MessageEvent<string>("message", { data: json });
        try {
          this.onmessage?.(decoded);
        } catch {
          // Consumer exceptions do not corrupt the envelope transport.
        }
        for (const [listener, once] of this.messageListeners) {
          if (this.retired) break;
          if (once) this.messageListeners.delete(listener);
          try {
            listener(decoded);
          } catch {
            // Independent observers must not strand later messages or close.
          }
        }
      }
    } finally {
      this.receiving = false;
      if (this.rawCloseEvent !== null) this.notifyClose();
    }
  }

  private clearPendingReceives(): void {
    this.pendingReceives.length = 0;
    this.nextReceive = 0;
  }

  private notifyClose(): void {
    const event = this.rawCloseEvent;
    if (event === null || this.closeNotified) return;
    this.closeNotified = true;
    this.retired = true;
    this.clearPendingReceives();
    try {
      this.onclose?.(event);
    } catch {
      // Close consumers run independently and cannot prevent owner cleanup.
    }
    for (const [listener, once] of this.closeListeners) {
      if (once) this.closeListeners.delete(listener);
      try {
        listener(event);
      } catch {
        // One throwing close observer must not skip the others.
      }
    }
  }

  private async decodeIncoming(data: unknown): Promise<string> {
    if (typeof data === "string") return data;
    if (data instanceof Blob) {
      return decodeJsonEnvelope(new Uint8Array(await data.arrayBuffer()));
    }
    if (ArrayBuffer.isView(data)) {
      return decodeJsonEnvelope(new Uint8Array(data.buffer, data.byteOffset, data.byteLength));
    }
    if (data instanceof ArrayBuffer) return decodeJsonEnvelope(new Uint8Array(data));
    throw new Error("unsupported WebSocket frame type");
  }
}
