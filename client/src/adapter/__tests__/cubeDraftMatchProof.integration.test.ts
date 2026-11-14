import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

import { beforeAll, describe, expect, it, vi } from "vitest";
import initDraft from "@wasm/draft";
import initEngine, {
  export_game_state_json, get_legal_actions_js, initialize_game, load_card_database as loadEngineCards,
} from "@wasm/engine";

const { hostRoom, saveDraftHostSession } = vi.hoisted(() => ({
  hostRoom: vi.fn(),
  saveDraftHostSession: vi.fn(async () => {}),
}));

vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  hostRoom,
}));

vi.mock("../../services/draftPersistence", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/draftPersistence")>()),
  clearDraftHostSession: vi.fn(async () => {}),
  loadDraftHostSession: vi.fn(async () => ({ type: "absent" })),
  saveActiveDraftPod: vi.fn(),
  saveDraftHostSession,
}));

import fixture from "../../test/fixtures/draftMatchProof.json";
import { DraftAdapter, type CubeDraftSettings, type DraftPlayerView } from "../draft-adapter";
import { P2PDraftHost } from "../p2p-draft-host";
import { formatMetadata } from "../../data/formatRegistry";
import type { PersistedDraftHostSession } from "../../services/draftPersistence";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";

const wasmDir = resolve(__dirname, "../../wasm");
const cubeList = fixture.spells.map((name) => `15 ${name}`).join("\n");

function cardData(): string {
  return JSON.stringify(Object.fromEntries([...fixture.spells, ...fixture.basics].map((name) => [
    name.toLowerCase(),
    {
      name,
      mana_cost: { type: "NoCost" },
      card_type: {
        supertypes: fixture.basics.includes(name) ? ["Basic"] : [],
        core_types: [fixture.basics.includes(name) ? "Land" : "Creature"],
        subtypes: [],
      },
      power: fixture.basics.includes(name) ? null : "1",
      toughness: fixture.basics.includes(name) ? null : "1",
      loyalty: null,
      defense: null,
      oracle_text: null,
      abilities: [],
      triggers: [],
      static_abilities: [],
      replacements: [],
      keywords: [],
      bracket_signals: {
        game_changer: false, mass_land_denial: false, extra_turn: false, efficient_tutor: false,
      },
    },
  ])));
}

function settings(minDeckSize: number): CubeDraftSettings {
  return {
    pod_size: 2,
    pack_count: 2,
    cards_per_pack: 10,
    min_deck_size: minDeckSize,
    addable_cards: { policy: "StandardBasics", custom: [] },
  };
}

async function draftTwenty(adapter: DraftAdapter, minimum: number): Promise<DraftPlayerView> {
  let view = await adapter.initializeCube(cubeList, "Twenty-card proof", settings(minimum), 0, 291);
  expect(view.status).toBe("Drafting");
  for (let step = 0; step < 25 && view.status === "Drafting"; step += 1) {
    const card = view.current_pack?.[0];
    if (!card) throw new Error(`Cube draft stalled at pick ${step}: ${view.status}`);
    view = await adapter.submitPick(card.instance_id);
  }
  expect(view.status).toBe("Deckbuilding");
  expect(view.pool).toHaveLength(20);
  return view;
}

function installedDecks(): string[][] {
  const envelope = JSON.parse(export_game_state_json()) as { state: Record<string, unknown> };
  const state = envelope.state;
  const players = state.players as Array<{ library: unknown[]; hand: unknown[] }>;
  const objects = state.objects as Record<string, { name: string }> | Map<unknown, { name: string }>;
  return players.map(({ library, hand }) => [...library, ...hand].map((id) => {
    const card = objects instanceof Map ? objects.get(id) : objects[String(id)];
    if (!card) throw new Error(`Missing installed card object ${String(id)}`);
    return card.name;
  }));
}

describe("custom Cube submission to real Limited game", () => {
  beforeAll(async () => {
    const [draftBytes, engineBytes] = await Promise.all([
      readFile(resolve(wasmDir, "draft_wasm_bg.wasm")),
      readFile(resolve(wasmDir, "engine_wasm_bg.wasm")),
    ]);
    await initDraft({ module_or_path: await WebAssembly.compile(draftBytes) });
    await initEngine({ module_or_path: await WebAssembly.compile(engineBytes) });
    const data = cardData();
    const adapter = new DraftAdapter();
    expect(await adapter.loadCardDatabase(data)).toBeGreaterThan(0);
    loadEngineCards(data);
  }, 300_000);

  it("installs the exact exported 20-card submission as an engine game", async () => {
    const adapter = new DraftAdapter();
    const view = await draftTwenty(adapter, 20);
    expect(view.min_deck_size).toBe(20);
    const submitted = view.pool.map((card) => card.name);
    expect(submitted).toHaveLength(20);
    const accepted = await adapter.submitDeck(submitted, []);
    expect(accepted.seats[0].has_submitted_deck).toBe(true);
    const session = JSON.parse(await adapter.exportSession()) as {
      submitted_decks: Record<string, { seat: number; main_deck: string[] }>;
    };
    const exported = Object.values(session.submitted_decks).find((deck) => deck.seat === 0);
    expect(exported?.main_deck).toEqual(submitted);
    if (!exported) throw new Error("Accepted Cube deck was not exported");

    const payload = {
      player: { main_deck: exported.main_deck, sideboard: [], commander: [] },
      opponent: { main_deck: Array<string>(20).fill("Forest"), sideboard: [], commander: [] },
      ai_decks: [],
    };
    expect(payload.player.main_deck).toBe(exported.main_deck);
    const result = initialize_game(
      payload, 291, formatMetadata("Limited")!.default_config, { match_type: "Bo1" }, 2, 0,
    ) as { error?: boolean; reasons?: string[] };
    expect(result.error, result.reasons?.join("; ")).not.toBe(true);
    const decks = installedDecks();
    expect(decks).toHaveLength(2);
    expect([...decks[0]].sort()).toEqual([...exported.main_deck].sort());
    expect(decks[1]).toHaveLength(20);
    expect((get_legal_actions_js() as { actions: unknown[] }).actions.length).toBeGreaterThan(0);
    console.info(`Cube proof: exported submission ${exported.main_deck.length}, game decks ${decks.map((deck) => deck.length).join("/")}`);

    const invalid = initialize_game(
      { ...payload, player: { ...payload.player, main_deck: [...exported.main_deck.slice(1), "Unknown proof card"] } },
      291, formatMetadata("Limited")!.default_config, { match_type: "Bo1" }, 2, 0,
    ) as { error?: boolean; reasons?: string[] };
    expect(invalid.error).toBe(true);
    expect(invalid.reasons?.join("; ")).toContain("Unknown proof card");
  }, 300_000);

  it("rejects the adjacent 21-card Cube minimum after reaching Deckbuilding", async () => {
    const adapter = new DraftAdapter();
    const view = await draftTwenty(adapter, 21);
    expect(view.min_deck_size).toBe(21);
    await expect(adapter.submitDeck(view.pool.map((card) => card.name), []))
      .rejects.toThrow("deck has 20 cards, minimum is 21");
  }, 300_000);

  it("checks an exported session with the legacy admission without installing it", async () => {
    const adapter = new DraftAdapter();
    const view = await draftTwenty(adapter, 20);
    await adapter.submitDeck(view.pool.map((card) => card.name), []);
    const exported = await adapter.exportSession();
    const session = JSON.parse(exported) as { config: Record<string, unknown> };

    // Reach: the session the engine exported passes the check.
    expect(await adapter.checkRestorableSession(exported)).toEqual({ type: "Admitted" });
    expect(await adapter.checkRestorableSession(JSON.stringify({
      ...session,
      config: { ...session.config, flow: "FifoAsync" },
    }))).toEqual({ type: "Incompatible" });
    expect(await adapter.checkRestorableSession(JSON.stringify({
      ...session,
      config: { ...session.config, selection_policy: { burns_per_pick: 1, discard_threshold: 0 } },
    }))).toEqual({ type: "Incompatible" });
    expect(await adapter.checkRestorableSession("{")).toEqual({ type: "Unreadable" });
    expect(await adapter.exportSession()).toBe(exported);
  }, 300_000);

  it("keeps a recovering host's saved session out of an installed host's draft engine session", async () => {
    const engine = new DraftAdapter();
    await engine.initializeCube(cubeList, "Recovered pod", settings(20), 0, 401);
    const recoveredSession = await engine.exportSession();
    await engine.initializeCube(cubeList, "Installed pod", settings(20), 0, 402);
    const installedSession = await engine.exportSession();
    const ownSession: Record<string, string> = {
      "installed-owner": installedSession,
      "recovered-host": recoveredSession,
    };
    // Reach: the engine exported two different sessions and holds the second.
    expect(recoveredSession).not.toBe(installedSession);
    expect(await engine.exportSession()).toBe(installedSession);

    const savedUnder = (persistenceId: string) => saveDraftHostSession.mock.calls
      .filter((call) => (call as unknown[])[0] === persistenceId)
      .map((call) => ((call as unknown[])[1] as PersistedDraftHostSession).draftSessionJson);
    hostRoom.mockImplementation(async (_signal: AbortSignal | undefined, options: { preferredRoomCode?: string } = {}) => {
      const roomCode = options.preferredRoomCode ?? "ABCDE";
      return {
        roomCode,
        peerId: `host-${roomCode}`,
        peer: { destroy: vi.fn() } as never,
        onGuestConnected: vi.fn(() => vi.fn()),
        destroy: vi.fn(),
      };
    });
    const poolInput = { type: "Set" as const, data: { pools: [{ code: "TST" }], sequence: ["TST"] } };
    const started = vi.spyOn(P2PDraftHost.prototype, "initialize");
    const realImport = DraftAdapter.prototype.importSession;
    let releaseImport!: () => void;
    const importReleased = new Promise<void>((resolveRelease) => {
      releaseImport = resolveRelease;
    });
    let reachImport!: () => void;
    const importReached = new Promise<void>((resolveReach) => {
      reachImport = resolveReach;
    });
    // Holds the recovering host after its real import has installed its session.
    const imports = vi.spyOn(DraftAdapter.prototype, "importSession").mockImplementation(
      async function (this: DraftAdapter, json: string, difficulty: number) {
        const view = await realImport.call(this, json, difficulty);
        reachImport();
        await importReleased;
        return view;
      },
    );
    try {
      await expect(useMultiplayerDraftStore.getState().hostDraft({
        poolInput,
        kind: "Premier",
        podSize: 2,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Casual",
        persistenceId: "installed-owner",
      })).resolves.toEqual({ status: "opened" });
      const installed = started.mock.contexts[0] as P2PDraftHost;
      Reflect.set(installed, "draftStarted", true);
      const saveInstalled = () =>
        (Reflect.get(installed, "persistSessionStrict") as () => Promise<void>).call(installed);
      await saveInstalled();
      // Reach: the installed host's own save path saves its own session.
      expect(savedUnder("installed-owner")).toContain(installedSession);

      const recovered: PersistedDraftHostSession = {
        persistenceId: "recovered-host",
        roomCode: "BBBBB",
        kind: "Premier",
        podSize: 2,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Casual",
        seatTokens: { 0: "host" },
        seatNames: { 0: "Host" },
        kickedTokens: [],
        draftStarted: true,
        draftCode: "recovered-host",
        draftSessionJson: recoveredSession,
        poolInput,
      };
      let current = true;
      const outcome = useMultiplayerDraftStore.getState().resumeHostedDraft({
        config: {
          poolInput,
          kind: recovered.kind,
          podSize: recovered.podSize,
          hostDisplayName: recovered.hostDisplayName,
          tournamentFormat: recovered.tournamentFormat,
          podPolicy: recovered.podPolicy,
          persistenceId: recovered.persistenceId,
          preferredRoomCode: recovered.roomCode,
          recovery: { session: recovered, raw: recovered },
        },
        isCurrent: () => current,
        routeSignal: new AbortController().signal,
      });
      await importReached;
      // Reach: the recovering host's restore imported its exact saved session.
      expect(imports.mock.calls.map(([json]) => json)).toEqual([recoveredSession]);

      await saveInstalled();
      current = false;
      releaseImport();
      await outcome;

      expect(savedUnder("installed-owner")).not.toContain(recoveredSession);
      const { hostPersistenceId } = useMultiplayerDraftStore.getState();
      expect(hostPersistenceId).not.toBeNull();
      expect(JSON.parse(await engine.exportSession())).toEqual(JSON.parse(ownSession[hostPersistenceId!]!));
    } finally {
      releaseImport();
      await useMultiplayerDraftStore.getState().leave(true);
      vi.restoreAllMocks();
    }
  }, 300_000);

  it.each([
    ["in Review", (session: Record<string, unknown>) => ({ ...session, status: "Review" })],
    [
      "with a FIFO flow",
      (session: Record<string, unknown>) => ({ ...session, config: { ...(session.config as Record<string, unknown>), flow: "FifoAsync" } }),
    ],
  ] as const)("refuses a recovering host before any import when the draft engine refuses its exported session %s", async (_label, edit) => {
    const engine = new DraftAdapter();
    await engine.initializeCube(cubeList, "Installed pod", settings(20), 0, 403);
    const installedSession = await engine.exportSession();
    const refusedSession = JSON.stringify(edit(JSON.parse(installedSession) as Record<string, unknown>));
    // Reach: the edit changed the session the engine exported, and the engine holds the export.
    expect(refusedSession).not.toBe(installedSession);
    expect(await engine.exportSession()).toBe(installedSession);
    hostRoom.mockImplementation(async (_signal: AbortSignal | undefined, options: { preferredRoomCode?: string } = {}) => {
      const roomCode = options.preferredRoomCode ?? "ABCDE";
      return {
        roomCode,
        peerId: `host-${roomCode}`,
        peer: { destroy: vi.fn() } as never,
        onGuestConnected: vi.fn(() => vi.fn()),
        destroy: vi.fn(),
      };
    });
    const poolInput = { type: "Set" as const, data: { pools: [{ code: "TST" }], sequence: ["TST"] } };
    const check = vi.spyOn(DraftAdapter.prototype, "checkRestorableSession");
    const imports = vi.spyOn(DraftAdapter.prototype, "importSession");
    try {
      await expect(useMultiplayerDraftStore.getState().hostDraft({
        poolInput,
        kind: "Premier",
        podSize: 2,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Casual",
        persistenceId: "installed-owner",
      })).resolves.toEqual({ status: "opened" });
      const refused: PersistedDraftHostSession = {
        persistenceId: "refused-host",
        roomCode: "CCCCC",
        kind: "Premier",
        podSize: 2,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Casual",
        seatTokens: { 0: "host" },
        seatNames: { 0: "Host" },
        kickedTokens: [],
        draftStarted: true,
        draftCode: "refused-host",
        draftSessionJson: refusedSession,
        poolInput,
      };

      const outcome = await useMultiplayerDraftStore.getState().resumeHostedDraft({
        config: {
          poolInput,
          kind: refused.kind,
          podSize: refused.podSize,
          hostDisplayName: refused.hostDisplayName,
          tournamentFormat: refused.tournamentFormat,
          podPolicy: refused.podPolicy,
          persistenceId: refused.persistenceId,
          preferredRoomCode: refused.roomCode,
          recovery: { session: refused, raw: refused },
        },
        isCurrent: () => true,
        routeSignal: new AbortController().signal,
      });

      // Reach: the real draft engine was asked once, with the exact saved session, and refused it.
      expect(check.mock.calls).toEqual([[refusedSession]]);
      await expect(check.mock.results[0]?.value).resolves.toEqual({ type: "Incompatible" });
      expect(outcome).toBe("incompatible");
      expect(imports).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState().hostPersistenceId).toBe("installed-owner");
      expect(JSON.parse(await engine.exportSession())).toEqual(JSON.parse(installedSession));
    } finally {
      await useMultiplayerDraftStore.getState().leave(true);
      vi.restoreAllMocks();
    }
  }, 300_000);
});
