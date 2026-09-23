import Peer from "peerjs";

/**
 * The small event surface the game uses from its peer transport.
 *
 * PeerJS remains the default implementation. Keeping this contract here
 * means the game/session layers do not need to import a signaling library when
 * another browser transport is added later.
 */
export interface TransportConnection {
  readonly open: boolean;
  readonly peer: string;
  readonly peerConnection?: RTCPeerConnection | null;
  readonly dataChannel?: RTCDataChannel | null;
  send(data: unknown): void;
  close(): void;
  on(event: "open" | "close", handler: () => void): this;
  on(event: "error", handler: (error: Error & { type?: string }) => void): this;
  on(event: "data", handler: (data: unknown) => void): this;
  once(event: "open" | "close", handler: () => void): this;
  once(event: "error", handler: (error: Error & { type?: string }) => void): this;
  once(event: "data", handler: (data: unknown) => void): this;
  off(event: "open" | "close", handler: () => void): this;
  off(event: "error", handler: (error: Error & { type?: string }) => void): this;
  off(event: "data", handler: (data: unknown) => void): this;
}

/** The connection options shared by every outgoing game dial. */
export interface TransportConnectOptions {
  serialization: "binary";
  reliable: boolean;
}

export interface TransportPeerOptions {
  config: RTCConfiguration;
}

export interface TransportPeer {
  readonly id: string;
  readonly destroyed: boolean;
  readonly disconnected: boolean;
  connect(peerId: string, options: TransportConnectOptions): TransportConnection;
  destroy(): void;
  reconnect(): void;
  on(event: "open" | "disconnected" | "close", handler: () => void): this;
  on(event: "error", handler: (error: Error & { type?: string }) => void): this;
  on(event: "connection", handler: (connection: TransportConnection) => void): this;
  once(event: "open" | "error", handler: (() => void) | ((error: Error & { type?: string }) => void)): this;
  off(event: "open" | "disconnected" | "close", handler: () => void): this;
  off(event: "error", handler: (error: Error & { type?: string }) => void): this;
  off(event: "connection", handler: (connection: TransportConnection) => void): this;
}

/** A future signaling backend implements this one construction seam. */
export interface PeerTransportFactory {
  create(id?: string, options?: TransportPeerOptions): TransportPeer;
}

const peerJsFactory: PeerTransportFactory = {
  create(id, options) {
    const peer = id === undefined
      ? (options === undefined ? new Peer() : new Peer(options))
      : new Peer(id, options);
    return peer as unknown as TransportPeer;
  },
};

/** Current default; future backends can be selected behind this seam. */
export const peerTransportFactory: PeerTransportFactory = peerJsFactory;

export function createPeer(id?: string, options?: TransportPeerOptions): TransportPeer {
  return peerTransportFactory.create(id, options);
}
