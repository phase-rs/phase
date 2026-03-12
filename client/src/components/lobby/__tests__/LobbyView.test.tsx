import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";

import { LobbyView } from "../LobbyView";

class MockWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  static instances: MockWebSocket[] = [];

  readyState = MockWebSocket.CONNECTING;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  send = vi.fn();
  close = vi.fn(() => {
    this.readyState = MockWebSocket.CLOSED;
    this.onclose?.();
  });

  constructor(_url: string) {
    MockWebSocket.instances.push(this);
  }
}

vi.stubGlobal("WebSocket", MockWebSocket as unknown as typeof WebSocket);

describe("LobbyView", () => {
  afterEach(() => {
    cleanup();
    MockWebSocket.instances = [];
    vi.clearAllMocks();
  });

  it("calls onServerOffline when lobby websocket errors", () => {
    const onServerOffline = vi.fn();
    render(
      <LobbyView
        onHostGame={vi.fn()}
        onHostP2P={vi.fn()}
        onJoinGame={vi.fn()}
        activeDeckName="Deck A"
        onChangeDeck={vi.fn()}
        onServerOffline={onServerOffline}
      />,
    );

    const ws = MockWebSocket.instances[0];
    ws.onerror?.();

    expect(onServerOffline).toHaveBeenCalledTimes(1);
  });

  it("does not call onServerOffline when component unmounts before connection opens", () => {
    const onServerOffline = vi.fn();
    const { unmount } = render(
      <LobbyView
        onHostGame={vi.fn()}
        onHostP2P={vi.fn()}
        onJoinGame={vi.fn()}
        activeDeckName="Deck A"
        onChangeDeck={vi.fn()}
        onServerOffline={onServerOffline}
      />,
    );

    unmount();

    expect(onServerOffline).not.toHaveBeenCalled();
  });

  it("does not create a websocket in p2p mode", () => {
    render(
      <LobbyView
        onHostGame={vi.fn()}
        onHostP2P={vi.fn()}
        onJoinGame={vi.fn()}
        activeDeckName="Deck A"
        onChangeDeck={vi.fn()}
        connectionMode="p2p"
        onServerOffline={vi.fn()}
      />,
    );

    expect(MockWebSocket.instances).toHaveLength(0);
  });
});
