import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { StateCodecDecodeResult, StateCodecSession } from "../../adapter/state-codec-runtime";
import { AdapterErrorCode } from "../../adapter/types";
import { PROTOCOL_VERSION, WebSocketAdapter } from "../../adapter/ws-adapter";
import i18n from "../../i18n";
import { useAppNotificationStore } from "../../stores/appToastStore";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { buildGameState } from "../../test/factories/gameStateFactory";
import { abandonPendingDispatches, dispatchAction, isDispatchIdle } from "../dispatch";

/** Raw network boundary only: negotiation, recovery and notifications remain real. */
class RecoverySocket extends EventTarget {
  static OPEN = 1;
  static last: RecoverySocket | null = null;
  readyState = RecoverySocket.OPEN;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: ((event?: CloseEvent) => void) | null = null;
  send = vi.fn<(data: string) => void>();
  close = vi.fn();

  constructor(public url: string) {
    super();
    RecoverySocket.last = this;
  }

  receive(data: string): void {
    // Deliver EventTarget and IDL observers once each, as in the adapter tests.
    const handler = this.onmessage;
    this.onmessage = null;
    this.dispatchEvent(new MessageEvent("message", { data }));
    this.onmessage = handler;
    handler?.({ data });
  }
}

const FULL_KEY = { game_code: "GAME01", generation: 7 };
const HELLO = JSON.stringify({
  type: "ServerHello",
  data: {
    server_version: "0.0.0-test",
    build_commit: "testhash",
    protocol_version: PROTOCOL_VERSION,
    mode: "Full",
    command_correlation: "CommandIdV1",
    state_codecs: ["StateCodecV1"],
  },
});
const CONFIRMATION = JSON.stringify({
  type: "ServerHello",
  data: { server_version: "0.0.0-test", selected_state_codec: "StateCodecV1" },
});

function frame(type: "Full" | "Delta", state: unknown, revision: number): string {
  return JSON.stringify({ type, data: { state, revision } });
}

function actionsSent(raw: RecoverySocket): unknown[] {
  return raw.send.mock.calls
    .map(([text]) => JSON.parse(text) as { type: string })
    .filter((message) => message.type === "Action");
}

describe("state codec recovery notifications", () => {
  let adapter: WebSocketAdapter | null = null;
  let restoreState: () => void;

  beforeEach(() => {
    const previousGame = useGameStore.getState();
    const previousNotification = useAppNotificationStore.getState();
    const previousLanguage = usePreferencesStore.getState().language;
    restoreState = () => {
      useGameStore.setState(previousGame);
      useAppNotificationStore.setState(previousNotification);
      usePreferencesStore.getState().setLanguage(previousLanguage);
    };
    abandonPendingDispatches();
    useAppNotificationStore.getState().clearNotification();
    usePreferencesStore.getState().setLanguage("en");
    vi.useFakeTimers();
    RecoverySocket.last = null;
    vi.stubGlobal("WebSocket", RecoverySocket);
  });

  afterEach(() => {
    // Release pending adapter work before restoring the previous session.
    adapter?.dispose();
    adapter = null;
    abandonPendingDispatches();
    restoreState();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it("localizes interrupted and never-sent actions through real dispatch without sending during recovery", async () => {
    let rejectNext = false;
    const decode = vi.fn(async (bytes: Uint8Array): Promise<StateCodecDecodeResult> => {
      if (rejectNext) {
        rejectNext = false;
        return { kind: "resyncRequired" };
      }
      const envelope = JSON.parse(new TextDecoder().decode(bytes)) as { data: { state: unknown } };
      return { kind: "decoded", json: JSON.stringify(envelope.data.state) };
    });
    const session: StateCodecSession = { decode, close: vi.fn() };
    adapter = new WebSocketAdapter(
      "wss://localhost:9374/ws", "join", { main_deck: [], sideboard: [] }, "GAME01",
      undefined, undefined, undefined,
      { stateCodecSessionProvider: async () => session },
    );
    const initialized = adapter.initialize();
    await vi.waitFor(() => expect(RecoverySocket.last).not.toBeNull());
    const raw = RecoverySocket.last!;
    raw.receive(HELLO);
    await vi.waitFor(() => expect(raw.send).toHaveBeenCalledWith(
      expect.stringContaining('"type":"JoinGameWithPassword"'),
    ));
    expect(raw.send).toHaveBeenCalledWith(expect.stringContaining('"StateCodecV1"'));
    raw.receive(CONFIRMATION);
    const state = buildGameState({ stack: [], players: [] });
    raw.receive(frame("Full", {
      type: "GameStarted",
      data: {
        state, state_revision: 1, your_player: 0,
        player_token: "codec-token", full_key: FULL_KEY,
      },
    }, 1));
    await initialized;
    useGameStore.setState({
      adapter, gameState: state, gameMode: "online", waitingFor: state.waiting_for,
    });

    const pending = dispatchAction({ type: "PassPriority" }, 0);
    // Attach rejection observation before driving the asynchronous decoder.
    const interrupted = expect(pending).rejects.toMatchObject({
      code: "WS_CLOSED",
      recoverable: true,
      message: "La transmission de l’état a été interrompue pendant l’action",
    });
    await vi.waitFor(() => expect(actionsSent(raw)).toHaveLength(1));

    // The action was sent in English, but the recovery error must use the
    // preference active when it is produced, not a module-load translation.
    usePreferencesStore.getState().setLanguage("fr");
    expect(i18n.language).toBe("fr");
    rejectNext = true;
    raw.receive(frame("Delta", { type: "StateUpdate", data: { state } }, 2));
    await interrupted;
    expect(raw.send).toHaveBeenCalledWith('{"type":"ResyncState"}');
    expect(useAppNotificationStore.getState().notification).toMatchObject({
      description: "La transmission de l’état a été interrompue pendant l’action",
    });
    expect(isDispatchIdle()).toBe(true);

    useAppNotificationStore.getState().clearNotification();
    const sendsBeforeBlocked = raw.send.mock.calls.length;
    await expect(dispatchAction({ type: "PassPriority" }, 0)).rejects.toMatchObject({
      code: AdapterErrorCode.ACTION_NOT_SENT,
      recoverable: true,
      message: "La transmission de l’état est en cours de rétablissement ; cette action n’a pas été envoyée.",
    });
    expect(useAppNotificationStore.getState().notification).toMatchObject({
      description: "La transmission de l’état est en cours de rétablissement ; cette action n’a pas été envoyée.",
    });
    expect(raw.send).toHaveBeenCalledTimes(sendsBeforeBlocked);
    expect(actionsSent(raw)).toHaveLength(1);
    expect(raw.close).not.toHaveBeenCalled();
    expect(isDispatchIdle()).toBe(true);
  });
});
