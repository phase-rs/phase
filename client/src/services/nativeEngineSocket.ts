import { Channel, invoke } from "@tauri-apps/api/core";

import { isDesktopTauri } from "./platform";
import { normalizeLanEndpoint } from "./lan";

type BridgeEvent =
  | { type: "message"; text: string }
  | { type: "fragment"; text: string }
  | { type: "closed"; code: number; reason: string }
  | { type: "error"; detail: string };

// Declares that this client reassembles `fragment` events; a shell splits texts only when told.
type BridgeTextDelivery = "fragments";
const TEXT_DELIVERY: BridgeTextDelivery = "fragments";

type CloseListener = (event: CloseEvent) => void;
type MessageListener = (event: MessageEvent<string>) => void;

/**
 * WebSocket-shaped client for the shell-owned native-engine bridge.
 *
 * The bridge accepts and forwards JSON text frames only; it intentionally has
 * no binary-frame negotiation surface. A typed LAN destination selects the
 * independent desktop LAN registry; no destination preserves the solo bridge.
 */
export class NativeEngineSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

  readonly CONNECTING = NativeEngineSocket.CONNECTING;
  readonly OPEN = NativeEngineSocket.OPEN;
  readonly CLOSING = NativeEngineSocket.CLOSING;
  readonly CLOSED = NativeEngineSocket.CLOSED;

  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent<string>) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  onclose: ((event: CloseEvent) => void) | null = null;

  private readonly closeListeners = new Map<CloseListener, boolean>();
  private readonly messageListeners = new Map<MessageListener, boolean>();
  private readonly pendingEvents: BridgeEvent[] = [];
  // The shell splits one received message into fragments, so `pendingText` never exceeds the bridge's `MAX_BRIDGE_MESSAGE_BYTES`.
  private pendingText = "";
  private bridgeId: number | null = null;
  private _readyState = NativeEngineSocket.CONNECTING;
  /**
   * Serialized admission: every write waits for the previous shell command to
   * settle, so ClientHello, setup, ordinary frames and close are admitted to
   * the Rust bridge FIFO in program order rather than task-scheduling order.
   */
  /** What the installed shell supports; probed before the socket opens. */
  private capabilities: { sinkReceipt: boolean; abandon: boolean } = {
    sinkReceipt: false,
    abandon: false,
  };
  private receiptWriter: ((data: string, signal?: AbortSignal) => Promise<void>) | null = null;
  /** Monotonic write sequence for the shell's ordered admission contract. */
  private nextSequence = 0;

  constructor(private readonly destination?: { type: "lan"; url: string; origin: string }) {
    // Match the browser WebSocket lifecycle: construction returns while the
    // socket is CONNECTING, giving callers a chance to install terminal-event
    // handlers before even a platform-boundary failure can be dispatched.
    queueMicrotask(() => void this.connect());
  }

  get readyState(): number {
    return this._readyState;
  }

  addEventListener(
    type: "close",
    listener: CloseListener,
    options?: AddEventListenerOptions | boolean,
  ): void;
  addEventListener(
    type: "message",
    listener: MessageListener,
    options?: AddEventListenerOptions | boolean,
  ): void;
  addEventListener(
    type: "close" | "message",
    listener: CloseListener | MessageListener,
    options?: AddEventListenerOptions | boolean,
  ): void {
    const once = typeof options === "object" && options.once === true;
    if (type === "close") this.closeListeners.set(listener as CloseListener, once);
    else this.messageListeners.set(listener as MessageListener, once);
  }

  removeEventListener(type: "close", listener: CloseListener): void;
  removeEventListener(type: "message", listener: MessageListener): void;
  removeEventListener(type: "close" | "message", listener: CloseListener | MessageListener): void {
    if (type === "close") this.closeListeners.delete(listener as CloseListener);
    else this.messageListeners.delete(listener as MessageListener);
  }

  send(text: string): void {
    if (this.readyState !== NativeEngineSocket.OPEN || this.bridgeId === null) {
      throw new DOMException("WebSocket is not open.", "InvalidStateError");
    }
    const sequence = this.nextSequence++;
    void this.admitWrite(sequence, text).catch((error) => {
      this.handleBridgeFailure(error);
    });
  }

  /**
   * Ordered write receipt: resolves once the shell acknowledged this frame's
   * IPC write, rejects when it failed (after routing the failure through the
   * usual bridge error/close path).
   */
  /**
   * Present only when the installed shell reports sink-write receipts. An
   * older shell acknowledges queue admission instead, so adapters must fall
   * back to the synchronous send path rather than retire an incumbent on a
   * promise that does not mean the frame was written.
   */
  get sendReceipt(): (data: string, signal?: AbortSignal) => Promise<void> {
    this.receiptWriter ??= (data: string, signal?: AbortSignal) => this.writeWithReceipt(data, signal);
    return this.receiptWriter;
  }

  private writeWithReceipt(text: string, signal?: AbortSignal): Promise<void> {
    if (this.readyState !== NativeEngineSocket.OPEN || this.bridgeId === null || signal?.aborted === true) {
      return Promise.reject(new DOMException("WebSocket is not open.", "InvalidStateError"));
    }
    const sequence = this.nextSequence++;
    const abandonOnAbort = () => this.abandonQueuedWrites();
    signal?.addEventListener("abort", abandonOnAbort, { once: true });
    return this.admitWrite(sequence, text).then(
      () => {
        if (signal?.aborted === true) throw new Error("native bridge write was abandoned");
      },
      (error: unknown) => {
        this.handleBridgeFailure(error);
        throw error;
      },
    ).finally(() => {
      signal?.removeEventListener("abort", abandonOnAbort);
    });
  }

  /**
   * Admits one frame. The current shell receives a sequence and enqueues it in
   * that order before acknowledging; an older shell admits synchronously and
   * ignores ordering metadata it was never given.
   */
  private admitWrite(sequence: number, text: string): Promise<void> {
    if (this.capabilities.sinkReceipt) {
      return this.invokeDesktop(
        this.destination ? "lan_bridge_write" : "native_engine_bridge_write",
        { id: this.bridgeId, text, sequence },
      ).then(() => undefined);
    }
    return this.invokeDesktop(
      this.destination ? "lan_bridge_send" : "native_engine_bridge_send",
      { id: this.bridgeId, text },
    ).then(() => undefined);
  }

  /**
   * Retires the bridge's still-queued frames at the writer. Aborting a setup
   * write therefore cannot leave a candidate's frame in the writer after the
   * client declined to adopt it.
   */
  private abandonQueuedWrites(): void {
    const bridgeId = this.bridgeId;
    if (bridgeId === null || !this.capabilities.abandon) return;
    void this.invokeDesktop(
      this.destination ? "lan_bridge_abandon" : "native_engine_bridge_abandon",
      { id: bridgeId },
    ).catch(() => undefined);
  }


  close(): void {
    if (
      this.readyState === NativeEngineSocket.CLOSING ||
      this.readyState === NativeEngineSocket.CLOSED
    ) {
      return;
    }
    this._readyState = NativeEngineSocket.CLOSING;
    const bridgeId = this.bridgeId;
    if (bridgeId !== null) {
      if (this.capabilities.sinkReceipt) {
        // The shell admits the close only after every earlier sequence, so
        // already-accepted writes are neither skipped nor blocked by a slow
        // sink. Its watchdog bounds the drain.
        const sequence = this.nextSequence++;
        void this.invokeDesktop(
          this.destination ? "lan_bridge_finish" : "native_engine_bridge_finish",
          { id: bridgeId, sequence },
        ).catch((error) => this.handleBridgeFailure(error));
      } else {
        this.closeBridge(bridgeId);
      }
    }
  }

  private async connect(): Promise<void> {
    try {
      // Fail before Channel registers a Tauri callback that mobile cannot consume.
      if (!isDesktopTauri()) {
        throw new Error("Native engine bridge is available only in the desktop shell.");
      }
      const lanUrl = this.destination ? normalizeLanEndpoint(this.destination.url) : null;
      if (this.destination && !lanUrl) throw new Error("Invalid LAN endpoint");
      const channel = new Channel<BridgeEvent>((event) => {
        this.handleBridgeEvent(event);
      });
      const bridgeId = await this.invokeDesktop<number>(
        this.destination ? "connect_lan_server" : "connect_native_engine",
        this.destination
          ? { url: lanUrl, origin: this.destination.origin, onEvent: channel, textDelivery: TEXT_DELIVERY }
          : { onEvent: channel, textDelivery: TEXT_DELIVERY },
      );
      this.bridgeId = bridgeId;
      await this.probeCapabilities();
      if (this.readyState === NativeEngineSocket.CLOSING) {
        this.closeBridge(bridgeId);
        return;
      }
      if (this.readyState === NativeEngineSocket.CLOSED) {
        return;
      }
      this._readyState = NativeEngineSocket.OPEN;
      this.onopen?.(new Event("open"));
      for (const event of this.pendingEvents.splice(0)) {
        this.dispatchBridgeEvent(event);
      }
    } catch (error) {
      this.handleBridgeFailure(error);
    }
  }

  private async probeCapabilities(): Promise<void> {
    try {
      const caps = await this.invokeDesktop<{ sinkReceipt?: unknown; abandon?: unknown }>(
        "bridge_capabilities",
        {},
      );
      this.capabilities = {
        sinkReceipt: caps.sinkReceipt === true,
        abandon: caps.abandon === true,
      };
    } catch {
      // An older shell has no capability command: keep the legacy contract.
      this.capabilities = { sinkReceipt: false, abandon: false };
    }
  }

  private closeBridge(bridgeId: number): void {
    void this.invokeDesktop(this.destination ? "lan_bridge_close" : "native_engine_bridge_close", { id: bridgeId }).catch((error) => {
      this.handleBridgeFailure(error);
    });
  }

  private async invokeDesktop<T>(
    command: string,
    args: Record<string, unknown>,
  ): Promise<T> {
    if (!isDesktopTauri()) {
      throw new Error("Native engine bridge is available only in the desktop shell.");
    }
    return invoke<T>(command, args);
  }

  private handleBridgeEvent(event: BridgeEvent): void {
    if (this.readyState === NativeEngineSocket.CONNECTING) {
      this.pendingEvents.push(event);
      return;
    }
    this.dispatchBridgeEvent(event);
  }

  private dispatchBridgeEvent(event: BridgeEvent): void {
    switch (event.type) {
      case "fragment":
        this.pendingText += event.text;
        break;
      case "message": {
        const text = this.pendingText + event.text;
        this.pendingText = "";
        if (this.readyState === NativeEngineSocket.OPEN) {
          const message = new MessageEvent<string>("message", { data: text });
          this.onmessage?.(message);
          for (const [listener, once] of this.messageListeners) {
            listener(message);
            if (once) this.messageListeners.delete(listener);
          }
        }
        break;
      }
      case "error":
        if (this.readyState !== NativeEngineSocket.CLOSED) {
          this.onerror?.(new Event("error"));
        }
        break;
      case "closed":
        this.finishClose(event.code, event.reason);
        break;
    }
  }

  private handleBridgeFailure(_error: unknown): void {
    if (this.readyState === NativeEngineSocket.CLOSED) {
      return;
    }
    // The close is what `withReconnect` recovers from, so it must survive a
    // throwing error handler: `ws-adapter`'s `emit()` does not catch. Same
    // guard `gzipEnvelopeSocket` applies on this seam.
    try {
      this.onerror?.(new Event("error"));
    } finally {
      this.finishClose(1006, "Native engine bridge failed");
    }
  }

  private finishClose(code: number, reason: string): void {
    if (this.readyState === NativeEngineSocket.CLOSED) {
      return;
    }
    this._readyState = NativeEngineSocket.CLOSED;
    const event = new CloseEvent("close", {
      code,
      reason,
      wasClean: code === 1000,
    });
    // The loop is cleanup and must not be skipped by a throwing `onclose`:
    // `withReconnect` recovers through a close LISTENER, not through `onclose`.
    try {
      this.onclose?.(event);
    } finally {
      for (const [listener, once] of this.closeListeners) {
        listener(event);
        if (once) {
          this.closeListeners.delete(listener);
        }
      }
    }
  }
}
