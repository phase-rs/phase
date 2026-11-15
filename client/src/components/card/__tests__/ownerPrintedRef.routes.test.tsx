import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CardPrinting, GameObject, GameState, WaitingFor } from "../../../adapter/types.ts";
import type * as Platform from "../../../services/platform.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { buildStackEntry, gameStateFactory } from "../../../test/factories/gameStateFactory.ts";

// Only metadata I/O and action transport are doubled; image resolution and every
// mounted surface use their production implementations.
vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn().mockResolvedValue(undefined), dispatchInteraction: vi.fn(),
}));
vi.mock("../../../hooks/useGameDispatch.ts", () => ({ useGameDispatch: () => vi.fn() }));
vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null, useLocalizedCardName: (name: string | null) => name,
  useCardParseDetails: () => null, useCardRulings: () => [],
}));
vi.mock("../../../hooks/useLocalizedCardContent.ts", () => ({ useLocalizedCardContent: () => null }));

const A = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const B = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const PA = "11111111-1111-4111-8111-111111111111";
const PB = "22222222-2222-4222-8222-222222222222";
const aPrinting: CardPrinting = { set_code: "aaa", collector_number: "1" };
const bPrinting: CardPrinting = { set_code: "bbb", collector_number: "2" };
const ownerA = "https://img.example/twin-a.jpg";
const ownerB = "https://img.example/twin-b.jpg";
const legacy = "/twin-legacy.jpg";
const ref = { oracle_id: B, face_name: "Twin" };

function json(value: unknown) {
  return new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });
}

async function setup() {
  const entry = { oracle_id: A, name: "Twin", face_names: ["twin"], layout: "normal",
    mana_cost: "{1}", cmc: 1, type_line: "Creature", colors: [], color_identity: [], keywords: [],
    faces: [{ normal: legacy }] };
  const printing = (id: string, pair: CardPrinting, url: string) => ({
    id, set: pair.set_code, collector_number: pair.collector_number, set_name: pair.set_code,
    released_at: "2026-01-01", border_color: "black", frame_effects: [], full_art: false,
    faces: [{ normal: url }],
  });
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
    const url = new URL(String(input), "http://localhost/");
    if (url.pathname === "/scryfall-data.json") return json({ twin: entry, [A]: entry, [B]: { ...entry, oracle_id: B } });
    if (url.pathname === "/scryfall-printings.json") return json({
      [A]: [printing(PA, aPrinting, ownerA)], [B]: [printing(PB, bPrinting, ownerB)],
    });
    throw new Error(`Unexpected metadata request ${String(input)}`);
  }));
  vi.doMock("../../../services/platform.ts", async (original) => ({
    ...await original<typeof Platform>(), loadVisualPackBackend: vi.fn(async () => null),
  }));
  // resetModules isolates the real caches and store graph between hostile routes;
  // static imports would retain the previous fixture's service instances.
  const preferences = (await import("../../../stores/preferencesStore.ts")).usePreferencesStore;
  preferences.setState({ ...preferences.getInitialState(), language: "en", showOwnerPrintings: true,
    artOverrides: {}, artChain: [] });
  const connectivity = (await import("../../../stores/connectivityStore.ts")).useConnectivityStore;
  connectivity.setState({ forcedOffline: false, browserOnline: true });
  await (await import("../../../services/scryfall.ts")).loadScryfallData();
  const store = (await import("../../../stores/gameStore.ts")).useGameStore;
  const ui = (await import("../../../stores/uiStore.ts")).useUiStore;
  const multiplayer = (await import("../../../stores/multiplayerStore.ts")).useMultiplayerStore;
  ui.setState(ui.getInitialState());
  multiplayer.setState({ activePlayerId: 0, isSpectator: false });
  const { setGameStoreForTest } = await import("../../../test/helpers/gameStoreHelpers.ts");
  function seed(object: GameObject, overrides: Partial<GameState> = {}) {
    setGameStoreForTest({ gameState: gameStateFactory.withPlayers(0, 1).withObjects(object).build(overrides),
      gameMode: "online", legalActions: [], legalActionsByObject: {}, spellCosts: {}, viewerInteraction: null });
  }
  return { preferences, store, ui, seed };
}

beforeEach(() => {
  vi.resetModules();
  vi.doUnmock("../../../services/platform.ts");
  localStorage.clear();
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  localStorage.clear();
});

async function image(url: string) {
  await waitFor(() => expect(document.body.querySelector("img")).toHaveAttribute("src", url));
}

const routes = ["peek", "attackers", "Exile", "Graveyard", "commander", "reveal-until",
  "alternative", "type-slot", "choice", "debug", "preview", "emblem"] as const;

describe("object-owned Oracle on real secondary surfaces", () => {
  it.each(routes)("T2 admits B, rejects A and preserves toggle-off art through %s", async (route) => {
    const env = await setup();
    const object = gameObjectFactory.creature(2, 2).onBattlefield().named("Twin").build({
      printing: bPrinting, printed_ref: ref, display_visible_to_viewer: true,
      owner: 0, controller: route === "peek" ? 1 : 0,
      zone: route === "Exile" || route === "Graveyard" ? route : "Battlefield",
      ...(route === "emblem" ? { is_emblem: true, printed_ref: null,
        emblem_source: { name: "Twin", printed_ref: ref, printing: bPrinting } } : {}),
    });
    let waitingFor: WaitingFor = { type: "Priority", data: { player: 0 } };
    if (route === "commander") waitingFor = { type: "CommanderZoneChoice", data: {
      player: 0, commander_id: object.id, current_zone: "Graveyard",
    } };
    if (route === "reveal-until") waitingFor = { type: "RevealUntilKeptChoice", data: {
      player: 0, hit_card: object.id, source_id: object.id, accept_zone: "Battlefield", decline_zone: "Graveyard",
      enter_tapped: false, enters_attacking: false, revealed_misses: [], rest_destination: "Graveyard",
    } };
    if (route === "alternative") waitingFor = { type: "AlternativeCastChoice", data: {
      player: 0, object_id: object.id, card_id: object.card_id, keyword: { type: "Cleave" },
      normal_cost: { type: "NoCost" }, alternative_cost: { type: "NoCost" },
      alternative_additional_cost: null, alternative_additional_cost_description: null,
    } };
    if (route === "type-slot") waitingFor = { type: "ChoosePermanentTypeSlot", data: {
      player: 0, object_id: object.id, card_id: object.card_id, source: object.id, available_slots: ["Creature"],
    } };
    const state: Partial<GameState> = { waiting_for: waitingFor,
      players: gameStateFactory.withPlayers({ id: 0, graveyard: route === "Graveyard" ? [object.id] : [] }, 1).build().players,
      exile: route === "Exile" ? [object.id] : [],
      derived: { debug_library_cards: route === "debug"
        ? [{ object_id: object.id, name: "Twin", printed_ref: ref, printing: bPrinting }] : [] },
    };
    env.seed(object, state);
    if (route === "Exile" || route === "Graveyard") env.store.setState({ legalActionsByObject: {
      [object.id]: [{ type: "CastSpell", data: { object_id: object.id, card_id: object.card_id, targets: [] } }],
    } });
    if (route === "debug") env.ui.setState({ debugLibraryViewer: { playerId: 0 } });
    if (route === "preview" || route === "emblem") env.ui.setState({ inspectedObjectId: object.id, previewSticky: true });
    // Real components must bind the same reset store graph as setup().
    switch (route) {
      case "peek": {
        const { BattlefieldPeekPopover } = await import("../../hud/BattlefieldPeekPopover.tsx");
        render(<BattlefieldPeekPopover playerId={1} opponentName="Opponent" seatColor="#fff" isTargeting={false} legalTargetIds={[]} />);
        break;
      }
      case "attackers": {
        const { IncomingAttackersPopover } = await import("../../hud/IncomingAttackersPopover.tsx");
        render(<IncomingAttackersPopover attackerIds={[object.id]} opponentName="Opponent" />);
        break;
      }
      case "Exile": case "Graveyard": {
        const { PlayerHand } = await import("../../hand/PlayerHand.tsx");
        render(<PlayerHand />);
        break;
      }
      case "commander": case "reveal-until": {
        const { CardChoiceModal } = await import("../../modal/CardChoiceModal.tsx");
        render(<CardChoiceModal />);
        break;
      }
      case "alternative": {
        const { AlternativeCostModal } = await import("../../modal/AlternativeCostModal.tsx");
        render(<AlternativeCostModal />);
        break;
      }
      case "type-slot": {
        const { PermanentTypeSlotModal } = await import("../../modal/PermanentTypeSlotModal.tsx");
        render(<PermanentTypeSlotModal />);
        break;
      }
      case "choice": {
        const { ChoiceModal } = await import("../../modal/ChoiceModal.tsx");
        render(<ChoiceModal title="Choose" previewCardName="Twin" previewObjectId={object.id}
          options={[{ id: "yes", label: "Yes" }]} onChoose={vi.fn()} />);
        break;
      }
      case "debug": {
        const { DebugLibraryViewer } = await import("../../chrome/DebugLibraryViewer.tsx");
        render(<DebugLibraryViewer />);
        break;
      }
      case "preview": case "emblem": {
        const { GameCardPreview } = await import("../GameCardPreview.tsx");
        render(<GameCardPreview />);
        break;
      }
    }
    await image(ownerB);
    act(() => env.preferences.getState().setShowOwnerPrintings(false));
    await image(legacy);
    act(() => env.preferences.getState().setShowOwnerPrintings(true));
    await image(ownerB);
    const current = env.store.getState().gameState!;
    act(() => env.store.setState({ gameState: { ...current,
      objects: { ...current.objects, [object.id]: { ...object, printing: aPrinting,
        ...(route === "emblem" ? { emblem_source: { ...object.emblem_source!, printing: aPrinting } } : {}),
      } },
      derived: { ...current.derived, debug_library_cards: route === "debug"
        ? [{ object_id: object.id, name: "Twin", printed_ref: ref, printing: aPrinting }] : [] },
    } }));
    await image(legacy);
    expect(document.body.querySelector(`img[src="${ownerA}"]`)).toBeNull();
  });

  it.each(["departed", "live", "stale-incarnation", "stale-zone"] as const)(
    "stack rows never borrow name or same-ID live authority for %s sources", async (route) => {
      const env = await setup();
      const source = gameObjectFactory.creature(2, 2).onBattlefield().named("Twin").build({
        printing: aPrinting, printed_ref: { oracle_id: A, face_name: "Twin" },
        display_visible_to_viewer: true,
        ...(route === "stale-incarnation" ? { incarnation: 2 } : {}),
        ...(route === "stale-zone" ? { zone: "Exile" as const } : {}),
      });
      const entry = buildStackEntry({ source_id: source.id, kind: { type: "TriggeredAbility",
        data: { source_id: source.id, source_name: "Twin", ability: { targets: [] } } } });
      env.seed(source, { stack: [entry] });
      if (route === "departed") {
        env.store.setState({ gameState: { ...env.store.getState().gameState!, objects: {} } });
        expect(env.store.getState().gameState!.objects[source.id]).toBeUndefined();
      } else {
        expect(env.store.getState().gameState!.objects[source.id].printed_ref?.oracle_id).toBe(A);
      }
      // setup resets modules; static imports would bind the previous resolver/store graph.
      const { StackEntry } = await import("../../stack/StackEntry.tsx");
      const props = { entry, index: 0, isTop: true, cardSize: { width: 120, height: 168 } };
      const details = { source_name: "Twin", kind_label: "Triggered Ability", printing: bPrinting, printed_ref: ref };
      const surface = render(<StackEntry {...props} details={details} />);
      await image(ownerB);
      act(() => env.preferences.getState().setShowOwnerPrintings(false));
      await image(legacy);
      act(() => env.preferences.getState().setShowOwnerPrintings(true));
      await image(ownerB);
      // A's pair is admitted by both the display name and the stale live identity.
      surface.rerender(<StackEntry {...props} details={{ ...details, printing: aPrinting }} />);
      await image(legacy);
      surface.rerender(<StackEntry {...props} details={{ ...details, printing: aPrinting, printed_ref: undefined }} />);
      await act(async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); });
      await image(legacy);
      expect(document.body.querySelector(`img[src="${ownerA}"]`)).toBeNull();
      surface.rerender(<StackEntry {...props} details={details} />);
      await image(ownerB);
    },
  );
});
