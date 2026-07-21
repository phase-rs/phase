import { Channel, invoke } from "@tauri-apps/api/core";

type BridgeEvent =
  | { type: "message"; text: string }
  | { type: "closed"; code: number; reason: string }
  | { type: "error"; detail: string };

type CloseListener = (event: CloseEvent) => void;

/**
 * WebSocket-shaped client for the shell-owned native-engine bridge.
 *
 * The bridge accepts and forwards JSON text frames only; it intentionally has
 * no URL, binary-frame, or feature-detection surface for remote content.
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
  private readonly pendingEvents: BridgeEvent[] = [];
  private readonly channel = new Channel<BridgeEvent>((event) => {
    this.handleBridgeEvent(event);
  });
  private bridgeId: number | null = null;
  private _readyState = NativeEngineSocket.CONNECTING;

  constructor() {
    void this.connect();
  }

  get readyState(): number {
    return this._readyState;
  }

  addEventListener(
    type: "close",
    listener: CloseListener,
    options?: AddEventListenerOptions | boolean,
  ): void {
    if (type !== "close") return;
    const once = typeof options === "object" && options.once === true;
    this.closeListeners.set(listener, once);
  }

  removeEventListener(type: "close", listener: CloseListener): void {
    if (type !== "close") return;
    this.closeListeners.delete(listener);
  }

  send(text: string): void {
    if (this.readyState !== NativeEngineSocket.OPEN || this.bridgeId === null) {
      throw new DOMException("WebSocket is not open.", "InvalidStateError");
    }
    void invoke("native_engine_bridge_send", { id: this.bridgeId, text }).catch((error) => {
      this.handleBridgeFailure(error);
    });
  }

  close(): void {
    if (
      this.readyState === NativeEngineSocket.CLOSING ||
      this.readyState === NativeEngineSocket.CLOSED
    ) {
      return;
    }
    this._readyState = NativeEngineSocket.CLOSING;
    if (this.bridgeId !== null) {
      this.closeBridge(this.bridgeId);
    }
  }

  private async connect(): Promise<void> {
    try {
      const bridgeId = await invoke<number>("connect_native_engine", {
        onEvent: this.channel,
      });
      this.bridgeId = bridgeId;
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

  private closeBridge(bridgeId: number): void {
    void invoke("native_engine_bridge_close", { id: bridgeId }).catch((error) => {
      this.handleBridgeFailure(error);
    });
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
      case "message":
        if (this.readyState === NativeEngineSocket.OPEN) {
          this.onmessage?.(new MessageEvent("message", { data: event.text }));
        }
        break;
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
    this.onerror?.(new Event("error"));
    this.finishClose(1006, "Native engine bridge failed");
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
    this.onclose?.(event);
    for (const [listener, once] of this.closeListeners) {
      listener(event);
      if (once) {
        this.closeListeners.delete(listener);
      }
    }
  }
}
