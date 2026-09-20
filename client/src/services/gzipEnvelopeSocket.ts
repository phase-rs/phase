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
  private receiveQueue = Promise.resolve();
  private closeQueued = false;

  constructor(private readonly socket: PhaseSocketTransport) {
    if ("binaryType" in socket) {
      (socket as PhaseSocketTransport & { binaryType: BinaryType }).binaryType = "arraybuffer";
    }
    socket.onopen = (event) => this.onopen?.(event);
    socket.onerror = (event) => this.onerror?.(event);
    socket.onclose = (event) => {
      if (this.closeQueued) return;
      this.closeQueued = true;
      this.receiveQueue = this.receiveQueue
        .then(() => {
          // The loop is cleanup and must not be skipped by a throwing
          // `onclose`: callers may assign one and the emitter does not catch.
          try {
            this.onclose?.(event);
          } finally {
            for (const [listener, once] of this.closeListeners) {
              listener(event);
              if (once) this.closeListeners.delete(listener);
            }
          }
        })
        .catch(() => undefined);
    };
    socket.onmessage = (event) => {
      this.receiveQueue = this.receiveQueue
        .then(async () => {
          let json: string;
          try {
            json = await this.decodeIncoming(event.data as unknown);
          } catch {
            // A frame we cannot decode is a broken transport, not a droppable
            // message: mirror `send()` so the drop is observable and
            // `withReconnect` gets its close event. Deliberately scoped to the
            // decode call — the terminal catch below still swallows listener
            // exceptions, because the unwrapped plain-text transport does not
            // close on those either.
            try {
              this.onerror?.(new Event("error"));
            } finally {
              this.socket.close();
            }
            return;
          }
          const decoded = new MessageEvent<string>("message", { data: json });
          this.onmessage?.(decoded);
          for (const [listener, once] of this.messageListeners) {
            listener(decoded);
            if (once) this.messageListeners.delete(listener);
          }
        })
        .catch(() => undefined);
    };
  }

  get readyState(): number {
    return this.socket.readyState;
  }

  send(data: string): void {
    this.sendQueue = this.sendQueue
      .then(async () => {
        const encoded = await encodeJsonEnvelope(data);
        (this.socket as unknown as { send(data: Uint8Array): void }).send(encoded);
      })
      .catch(() => {
        // The close is what `withReconnect` recovers from, so it must survive a
        // throwing error handler: `ws-adapter`'s `emit` does not catch.
        try {
          this.onerror?.(new Event("error"));
        } finally {
          this.socket.close();
        }
      })
      .catch(() => undefined);
  }

  close(): void {
    this.socket.close();
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
