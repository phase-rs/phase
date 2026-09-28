import { beforeEach, describe, expect, it, vi } from "vitest";

const wasm = vi.hoisted(() => ({
  init: vi.fn(),
  loadComboTable: vi.fn(),
  initializeGame: vi.fn(),
  initializeMultiplayerHostGame: vi.fn(),
  estimateBracketForDeck: vi.fn(),
  loadReplayForPlayback: vi.fn(),
}));

vi.mock("@wasm/engine", () => {
  const unusedExport = vi.fn();
  const unusedNames = [
    "ping",
    "take_last_panic_message",
    "submit_action",
    "submit_interaction_js",
    "get_game_state",
    "get_filtered_game_state",
    "get_ai_action_proposal",
    "get_ai_action_proposal_with_diagnostics",
    "get_ai_tactical_action_proposal",
    "get_ai_tactical_action_proposal_with_diagnostics",
    "get_ai_action_proposal_from_scores",
    "get_ai_action_proposal_from_scores_with_diagnostics",
    "get_ai_scored_candidates",
    "submit_ai_action_proposal",
    "get_legal_actions_js",
    "get_legal_actions_for_viewer_js",
    "get_viewer_snapshot_js",
    "restore_game_state",
    "resume_restored_game_state",
    "resume_multiplayer_host_state",
    "load_card_database",
    "build_ai_card_subset",
    "evaluate_deck_compatibility_js",
    "evaluateDeckFormatGate",
    "customFormatFromLobbyConfig",
    "formatConfigForCustomRules",
    "apply_seat_mutation",
    "project_seat_view",
    "export_game_state_json",
    "clear_game_state",
    "set_multiplayer_mode",
    "selectAiPod",
    "deck_signals_for_deck",
    "has_replay_recording",
    "export_replay_log",
    "replay_length_js",
    "replay_header_js",
    "replay_seek_js",
    "clear_replay_playback",
    "preview_mana_payment_js",
    "preview_interaction_js",
    "get_card_face_data",
    "get_card_parse_details",
    "get_card_rulings",
  ];
  return {
    ...Object.fromEntries(unusedNames.map((name) => [name, unusedExport])),
    default: wasm.init,
    load_combo_table: wasm.loadComboTable,
    initialize_game: wasm.initializeGame,
    initialize_multiplayer_host_game: wasm.initializeMultiplayerHostGame,
    estimate_bracket_for_deck: wasm.estimateBracketForDeck,
    load_replay_for_playback: wasm.loadReplayForPlayback,
  };
});

interface WorkerScopeStub {
  onmessage: ((event: MessageEvent<Record<string, unknown>>) => Promise<void>) | null;
  postMessage: ReturnType<typeof vi.fn>;
}

function installWorkerScope(): WorkerScopeStub {
  const scope: WorkerScopeStub = { onmessage: null, postMessage: vi.fn() };
  vi.stubGlobal("self", scope);
  return scope;
}

async function loadWorker(scope: WorkerScopeStub): Promise<void> {
  await import("../engine-worker");
  expect(scope.onmessage).not.toBeNull();
}

async function dispatch(
  scope: WorkerScopeStub,
  message: Record<string, unknown>,
): Promise<void> {
  if (!scope.onmessage) throw new Error("worker handler not installed");
  await scope.onmessage(new MessageEvent("message", { data: message }));
}

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
  vi.stubGlobal("__ENGINE_WASM_URL__", undefined);
  vi.stubGlobal("__CARD_DATA_URL__", "/card-data.json");
  vi.stubGlobal("__COMBO_TABLE_URL__", "/combo-table.json");
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(new Response("{\"entries\":[]}", { status: 200 })),
  );
  wasm.loadComboTable.mockResolvedValue(0);
  wasm.initializeGame.mockReturnValue({ events: [], log_entries: [] });
  wasm.initializeMultiplayerHostGame.mockReturnValue({ events: [], log_entries: [] });
  wasm.estimateBracketForDeck.mockReturnValue(null);
  wasm.loadReplayForPlayback.mockReturnValue(0);
});

describe("engine worker combo-table ordering", () => {
  it.each([
    [
      "single-player initialization",
      { type: "initializeGame", id: 1, deckData: null, seed: 1, formatConfig: null, matchConfig: null },
      wasm.initializeGame,
    ],
    [
      "multiplayer host initialization",
      {
        type: "initializeMultiplayerHostGame",
        id: 1,
        deckData: null,
        seed: 1,
        formatConfig: null,
        matchConfig: null,
      },
      wasm.initializeMultiplayerHostGame,
    ],
    [
      "bracket estimation",
      {
        type: "estimateBracketForDeck",
        id: 1,
        request: {
          deck: {
            commander: [],
            main_deck: [],
            sideboard: [],
            companion: [],
            signature_spell: [],
            combo_declaration: { kind: "undeclared" },
          },
          declared_tier: null,
        },
      },
      wasm.estimateBracketForDeck,
    ],
    [
      "replay loading",
      { type: "loadReplayForPlayback", id: 1, replayJson: "{}" },
      wasm.loadReplayForPlayback,
    ],
  ])("loads the combo table before %s", async (_name, message, operation) => {
    const scope = installWorkerScope();
    await loadWorker(scope);

    await dispatch(scope, message);

    expect(wasm.loadComboTable).toHaveBeenCalledOnce();
    expect(operation).toHaveBeenCalledOnce();
    expect(wasm.loadComboTable.mock.invocationCallOrder[0]).toBeLessThan(
      operation.mock.invocationCallOrder[0],
    );
  });

  it("retries a transient combo-table failure on the next resolving operation", async () => {
    const fetchMock = vi
      .fn()
      .mockRejectedValueOnce(new Error("network unavailable"))
      .mockResolvedValueOnce(new Response("{\"entries\":[]}", { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    const scope = installWorkerScope();
    await loadWorker(scope);

    const message = {
      type: "initializeGame",
      id: 1,
      deckData: null,
      seed: 1,
      formatConfig: null,
      matchConfig: null,
    };
    await dispatch(scope, message);
    await dispatch(scope, { ...message, id: 2 });

    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(wasm.loadComboTable).toHaveBeenCalledOnce();
    expect(wasm.initializeGame).toHaveBeenCalledTimes(2);
  });
});
