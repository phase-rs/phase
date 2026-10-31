import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createRef, useEffect } from "react";
import type { ReactNode } from "react";
import { MemoryRouter, Route, Routes } from "react-router";
import type * as Three from "three";
import type * as Motion from "framer-motion";
import { setTimeout as delay } from "node:timers/promises";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

import type { CardPrinting, GameObject, GameState } from "../../../adapter/types.ts";
import type * as Platform from "../../../services/platform.ts";
import type { PrintingEntry } from "../../../services/scryfall.ts";
import type { ResolutionKey, ResolutionResponse, ResolvedAsset } from "../../../services/visualPacks/types.ts";
import { assetKey, catalogRoot, installedRevision, packId } from "../../../services/visualPacks/types.ts";
import { encodeCandidateKey } from "../../../services/visualPacks/candidateKeys.ts";
import type { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import type { useGameStore } from "../../../stores/gameStore.ts";
import type { useUiStore } from "../../../stores/uiStore.ts";
import type { useAnimationStore } from "../../../stores/animationStore.ts";
import type { WaitingFor } from "../../../adapter/types.ts";
import type { CardVfxLayerHandle } from "../../animation/cardVfx/CardVfxLayer.tsx";
import type { CardFlightSpec } from "../../animation/cardVfx/cardFlightSpecs.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory, buildStackEntry } from "../../../test/factories/gameStateFactory.ts";

// Only unrelated engine-content and dispatch boundaries are doubled. Image
// selection, repositories, preference selection, and Settings remain real.
const dispatch = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));
const observation = vi.hoisted(() => ({
  mobile: false,
  snapshot: new Map<number, DOMRect>(),
  textures: [] as Array<{ image: unknown }>,
  deaths: [] as HTMLImageElement[],
}));
vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: dispatch, dispatchInteraction: vi.fn(), dispatchResolveAll: vi.fn(),
  processRemoteUpdate: vi.fn(), restoreGameState: vi.fn(), currentSnapshot: observation.snapshot,
}));
vi.mock("../../../hooks/useGameDispatch.ts", () => ({ useGameDispatch: () => dispatch, currentSnapshot: observation.snapshot }));
vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useLocalizedCardName: (name: string | null) => name,
  useCardParseDetails: () => null,
  useCardRulings: () => [],
}));
vi.mock("../../../hooks/useLocalizedCardContent.ts", () => ({ useLocalizedCardContent: () => null }));
vi.mock("../../../services/backup.ts", () => ({ downloadBackup: vi.fn(), importBackupFromFile: vi.fn() }));
// GamePage session/board dependencies are unrelated to its real prompt routing.
vi.mock("../../../providers/GameProvider.tsx", () => ({ GameProvider: ({ children }: { children: ReactNode }) => <>{children}</> }));
vi.mock("../../../game/sessionCleanup.ts", () => ({ clearPromptOverlayState: vi.fn() }));
vi.mock("../../../hooks/useIsMobile.ts", () => ({ useIsMobile: () => observation.mobile, useIsCompactHeight: () => false }));
vi.mock("../../../audio/useAudioContext.ts", () => ({ useAudioContext: () => undefined }));
vi.mock("../../../hooks/useGameplayPreferencesSync.ts", () => ({ useGameplayPreferencesSync: () => undefined }));
vi.mock("../../../hooks/useCardDataMeta.ts", () => ({ useCardDataMeta: () => null, formatRelativeDate: () => "" }));
vi.mock("../../board/BattlefieldBackground.tsx", () => ({ BattlefieldBackground: () => null }));
vi.mock("../../board/GameBoard.tsx", () => ({ GameBoard: () => null }));
vi.mock("../../stack/StackDisplay.tsx", () => ({ StackDisplay: () => null }));
vi.mock("../../hud/HUD.tsx", () => ({ HUD: () => null }));
vi.mock("../../debug/DebugPanel.tsx", () => ({ DebugPanel: () => null }));
vi.mock("../../chrome/GameMenu.tsx", () => ({ GameMenu: () => null }));
vi.mock("../../modal/EngineLostModal.tsx", () => ({ EngineLostModal: () => null }));
vi.mock("../../modal/CardDataMissingModal.tsx", () => ({ CardDataMissingModal: () => null }));
vi.mock("../../multiplayer/ConcedeDialog.tsx", () => ({ ConcedeDialog: () => null }));
vi.mock("../../animation/ParticleCanvas.tsx", () => ({ ParticleCanvas: () => null }));
vi.mock("framer-motion", async (original) => ({
  ...await original<typeof Motion>(), useReducedMotion: () => false,
}));
// Only the upload/graphics boundary is doubled. The GL controller, real scene,
// faceReady/decode/takeFace, and resolver retirement all execute unchanged.
vi.mock("three", async (original) => {
  const three = await original<typeof Three>();
  class RecordingRenderer {
    setClearColor() {}
    setSize() {}
    setPixelRatio() {}
    initTexture(texture: Three.Texture) { observation.textures.push(texture); }
    compileAsync() { return Promise.resolve(); }
    render() {}
    dispose() {}
    forceContextLoss() {}
  }
  return { ...three, WebGLRenderer: RecordingRenderer };
});
// Observation double at the allowed DeathShatter graphics boundary, retaining
// its 600ms ordinary effect lifetime. Readiness and pending-loader retirement
// remain the real AnimationOverlay implementation.
vi.mock("../../animation/DeathShatter.tsx", () => ({
  DeathShatter: ({ image, onComplete }: { image: HTMLImageElement; onComplete: () => void }) => {
    if (!observation.deaths.includes(image)) observation.deaths.push(image);
    useEffect(() => { const timer = setTimeout(onComplete, 600); return () => clearTimeout(timer); }, [onComplete]);
    return <div data-testid="consumed-death-image" />;
  },
}));

const ORACLE = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OTHER_ORACLE = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const OWNER_A = "11111111-1111-4111-8111-111111111111";
const OWNER_B = "22222222-2222-4222-8222-222222222222";
const VIEWER = "33333333-3333-4333-8333-333333333333";
const FOREIGN = "44444444-4444-4444-8444-444444444444";
const printingA: CardPrinting = { set_code: "own", collector_number: "1" };
const printingB: CardPrinting = { set_code: "own", collector_number: "2" };
const foreignPrinting: CardPrinting = { set_code: "foreign", collector_number: "9" };
const URL_A = "/owner-a.jpg";
const URL_B = "/owner-b.jpg";
const URL_V = "/viewer.jpg";
const CROP_A = "/owner-a-crop.jpg";
const CROP_V = "/viewer-crop.jpg";

function json(value: unknown) {
  return new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });
}

function printing(id: string, set: string, collector_number: string): PrintingEntry {
  return {
    id, set, collector_number, set_name: set, released_at: "2026-01-01", border_color: "black",
    frame_effects: [], full_art: false,
    faces: [
      { normal: `https://img.example/${id}.jpg`, art_crop: `https://img.example/${id}-crop.jpg` },
      { normal: `https://img.example/${id}-back.jpg`, art_crop: `https://img.example/${id}-back-crop.jpg` },
    ],
  };
}

function card(overrides: Partial<GameObject> = {}) {
  return gameObjectFactory.creature(2, 2).onBattlefield().named("Alpha").build({
    printed_ref: { oracle_id: ORACLE, face_name: "Alpha" },
    printing: printingA, display_visible_to_viewer: true, ...overrides,
  });
}
// Dynamic imports intentionally reload the real resolver/store graph after
// resetModules: static imports would retain repositories across fixture runs.
interface Fixture {
  preferences: typeof usePreferencesStore;
  store: typeof useGameStore;
  ui: typeof useUiStore;
  animation: typeof useAnimationStore;
  seed: (objects: GameObject[], overrides?: Partial<GameState>) => GameState;
  admit: () => void;
  reset: () => void;
}
let activeFixture: Fixture | undefined;

async function setup({ holdCatalog = false, dfc = false, homonyms = false } = {}): Promise<Fixture> {
  const entry = {
    oracle_id: ORACLE, name: dfc ? "Alpha // Reverse" : "Alpha", face_names: dfc ? ["alpha", "reverse"] : ["alpha"], layout: dfc ? "transform" : "normal",
    mana_cost: "{1}", cmc: 1, type_line: "Creature", colors: [], color_identity: [], keywords: [],
    faces: [{ normal: "/canonical.jpg", art_crop: "/canonical-crop.jpg" }, { normal: "/canonical-back.jpg" }],
  };
  const catalog = {
    [ORACLE]: [printing(OWNER_A, "own", "1"), printing(OWNER_B, "own", "2"), printing(VIEWER, "pin", "3")],
    [OTHER_ORACLE]: [printing(FOREIGN, "foreign", "9")],
  };
  const homonym = { ...entry, name: "Twin", face_names: ["twin"] };
  const main = homonyms
    ? { [ORACLE]: homonym, twin: homonym, [OTHER_ORACLE]: { ...homonym, oracle_id: OTHER_ORACLE } }
    : { [ORACLE]: entry, alpha: entry, reverse: entry };
  if (homonyms) {
    catalog[ORACLE] = [printing(OWNER_A, "own", "1"), printing(VIEWER, "pin", "3")];
    catalog[OTHER_ORACLE] = [printing(OWNER_B, "own", "2")];
  }
  let admit!: (response: Response) => void;
  const gate = new Promise<Response>((resolve) => { admit = resolve; });
  const installed = new Map<string, ResolvedAsset[]>();
  const backend = {
    resolve: vi.fn(async (keys: ResolutionKey[]): Promise<ResolutionResponse> => ({
      revision: installedRevision("0"),
      entries: keys.map((key, ordinal) => ({ ordinal, key, matches: installed.get(key.key) ?? [] })),
    })),
    subscribeRevision: vi.fn(async () => () => {}),
  };
  vi.doMock("../../../services/platform.ts", async (original) => ({
    ...await original<typeof Platform>(), loadVisualPackBackend: vi.fn(async () => backend),
  }));
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
    const url = new URL(String(input), "http://localhost/");
    if (url.pathname === "/scryfall-data.json") return json(main);
    if (url.pathname === "/scryfall-printings.json") return holdCatalog ? gate : json(catalog);
    throw new Error(`Unexpected fixture request ${String(input)}`);
  }));
  const preferences = (await import("../../../stores/preferencesStore.ts")).usePreferencesStore;
  preferences.setState({ ...preferences.getInitialState(), language: "en", showOwnerPrintings: true,
    artOverrides: { [ORACLE]: { scryfallId: VIEWER, setCode: "pin", collectorNumber: "3" } }, artChain: [] });
  const connectivity = (await import("../../../stores/connectivityStore.ts")).useConnectivityStore;
  connectivity.setState({ forcedOffline: false, browserOnline: true });
  for (const [id, full, crop] of [[OWNER_A, URL_A, CROP_A], [OWNER_B, URL_B, "/owner-b-crop.jpg"], [VIEWER, URL_V, CROP_V]]) {
    for (const face of [0, 1]) {
      for (const rung of ["small", "normal", "art_crop"] as const) {
        const variant = rung === "art_crop" ? "art_crop" : "full_card";
        const key = encodeCandidateKey("english_printing", [id, face, variant, rung]);
        const url = rung === "art_crop" ? crop : full;
        installed.set(key, [{ packId: packId("complete"), assetKey: assetKey("asset:v1:exact_printing:QQ"),
          catalogRoot: catalogRoot("a".repeat(64)), url: face === 0 ? url : url.replace(".jpg", "-back.jpg"), media: "image/jpeg" }]);
      }
    }
  }
  const service = await import("../../../services/scryfall.ts");
  await service.loadScryfallData();
  const store = (await import("../../../stores/gameStore.ts")).useGameStore;
  const ui = (await import("../../../stores/uiStore.ts")).useUiStore;
  const animation = (await import("../../../stores/animationStore.ts")).useAnimationStore;
  ui.setState(ui.getInitialState());
  const multiplayer = (await import("../../../stores/multiplayerStore.ts")).useMultiplayerStore;
  multiplayer.setState({ activePlayerId: 0, isSpectator: false });
  const { setGameStoreForTest } = await import("../../../test/helpers/gameStoreHelpers.ts");
  function seed(objects: GameObject[], overrides: Partial<GameState> = {}) {
    const state = gameStateFactory.withPlayers(0, 1).withObjects(...objects).build(overrides);
    setGameStoreForTest({ gameState: state, gameMode: "online",
      legalActions: [], legalActionsByObject: {}, spellCosts: {}, viewerInteraction: null });
    return state;
  }
  activeFixture = { preferences, store, ui, animation, seed, admit: () => admit(json(catalog)), reset() {
    store.setState(store.getInitialState());
    ui.setState(ui.getInitialState());
    preferences.setState(preferences.getInitialState());
    connectivity.setState(connectivity.getInitialState());
    multiplayer.setState(multiplayer.getInitialState());
    animation.getState().clearQueue();
    observation.snapshot.clear();
  } };
  return activeFixture;
}

async function expectImage(container: HTMLElement, url: string) {
  await waitFor(() => expect(container.querySelector(`img[src="${url}"]`)).not.toBeNull());
}

async function toggleCycle(env: Fixture, container: HTMLElement, ownerUrl = URL_A, viewerUrl = URL_V) {
  await expectImage(container, ownerUrl);
  act(() => env.preferences.getState().setShowOwnerPrintings(false));
  await expectImage(container, viewerUrl);
  act(() => env.preferences.getState().setShowOwnerPrintings(true));
  await expectImage(container, ownerUrl);
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.resetModules();
  vi.doUnmock("../../../services/platform.ts");
  dispatch.mockClear();
  observation.mobile = false;
  observation.textures.length = 0;
  observation.deaths.length = 0;
  localStorage.clear();
});
afterEach(() => {
  cleanup();
  document.querySelectorAll("[data-owner-probe]").forEach((node) => node.remove());
  act(() => activeFixture?.reset());
  activeFixture = undefined;
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.resetModules();
  localStorage.clear();
});

describe("owner printing carrier surfaces (group 1)", () => {
  it("V1 admits Scry art without gating normal art and follows the real Settings checkbox", async () => {
    const env = await setup({ holdCatalog: true });
    const object = card({ zone: "Library", owner: 1, controller: 0 });
    env.seed([object], { waiting_for: { type: "ScryChoice", data: { player: 0, cards: [object.id] } },
      derived: { unique_authorized_submitter: 0 } });
    const { CardChoiceModal } = await import("../../modal/CardChoiceModal.tsx");
    const { PreferencesModal } = await import("../../settings/PreferencesModal.tsx");
    const surface = render(<CardChoiceModal />);
    await expectImage(surface.container, URL_V);
    await act(async () => env.admit());
    await expectImage(surface.container, URL_A);
    const settings = render(<PreferencesModal onClose={vi.fn()} initialTab="visual" />);
    const checkbox = screen.getByRole("checkbox", { name: "Show cards as their owners chose" });
    fireEvent.click(checkbox);
    await expectImage(surface.container, URL_V);
    fireEvent.click(checkbox);
    await expectImage(surface.container, URL_A);
    settings.unmount();
    for (const pair of [printingB, undefined, null, foreignPrinting, printingA]) {
      const current = env.store.getState().gameState!;
      act(() => env.store.setState({ gameState: { ...current,
        objects: { ...current.objects, [object.id]: { ...object, printing: pair } },
      } }));
      await expectImage(surface.container, pair === printingB ? URL_B : pair === printingA ? URL_A : URL_V);
    }
    fireEvent.click(within(surface.container).getByRole("button", { name: "Top" }));
    fireEvent.click(within(surface.container).getByRole("button", { name: /Confirm/i }));
    expect(dispatch).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [] } });
    env.seed([object], { waiting_for: { type: "ScryChoice", data: { player: 1, cards: [object.id] } },
      derived: { unique_authorized_submitter: 1 } });
    expect(within(surface.container).queryByRole("button", { name: /Confirm/i })).toBeNull();
  });

  it.each(["full", "ghost", "crop", "attachment", "scheme", "plane"] as const)(
    "V2 forwards the selected board carrier through %s", async (kind) => {
      const env = await setup();
      env.preferences.setState({ battlefieldCardDisplay: "full_card" });
      const object = card({ owner: 1, controller: 0, zone: kind === "ghost" ? "Exile" : "Battlefield" });
      const host = card({ printing: null });
      const overrides: Partial<GameState> = {
        derived: {
          linked_exile_ids: { [host.id]: [object.id] },
          archenemy: { archenemy: 0, scheme_deck_count: 1, active_scheme_ids: [object.id] },
          planechase: { active_plane: object.id, planar_deck_count: 1, can_roll: false,
            current_roll_cost: { type: "NoCost" } },
        },
      };
      env.seed(kind === "ghost" || kind === "attachment" ? [host, object] : [object], overrides);
      const { PermanentCard } = await import("../../board/PermanentCard.tsx");
      const { ArtCropCard } = await import("../ArtCropCard.tsx");
      const { AttachmentFan } = await import("../../board/AttachmentFan.tsx");
      const { ArchenemyPanel } = await import("../../board/ArchenemyPanel.tsx");
      const { PlanechasePanel } = await import("../../board/PlanechasePanel.tsx");
      if (kind === "attachment") {
        env.ui.setState({ attachmentFanHostId: host.id });
        env.store.setState({ viewerInteraction: {
          waitingForKind: { simultaneous: null, terminal: false, code: "choose" },
          authorizedSubmitters: [0], canSubmit: false, autoPassRecommended: false,
          opportunities: [], attachmentFans: {}, availability: { type: "waiting" },
          attachmentViews: { [host.id]: { hostId: host.id, cards: [{ objectId: object.id, submission: null }] } },
        } });
      }
      const surface = render(kind === "crop" ? <ArtCropCard objectId={object.id} />
        : kind === "attachment" ? <AttachmentFan />
        : kind === "scheme" ? <ArchenemyPanel />
        : kind === "plane" ? <PlanechasePanel />
        : <PermanentCard objectId={kind === "ghost" ? host.id : object.id} />);
      const container = kind === "ghost"
        ? surface.container.querySelector<HTMLElement>(`[data-exile-ghost="${object.id}"]`)!
        : kind === "attachment" ? document.body.querySelector<HTMLElement>(`[data-object-id="${object.id}"]`)! : surface.container;
      expect(container).not.toBeNull();
      await toggleCycle(env, container, kind === "crop" ? CROP_A : URL_A, kind === "crop" ? CROP_V : URL_V);
      const current = env.store.getState().gameState!;
      act(() => env.store.setState({ gameState: { ...current,
        objects: { ...current.objects, [object.id]: { ...object, printing: undefined } },
      } }));
      await expectImage(container, kind === "crop" ? CROP_V : URL_V);
    },
  );

  it("V2 preserves a concealed board back beside a public positive card", async () => {
    const env = await setup();
    env.preferences.setState({ battlefieldCardDisplay: "full_card" });
    const visible = card();
    const hidden = card({ name: "", printed_ref: null, printing: undefined, face_down: true,
      display_visible_to_viewer: false });
    env.seed([visible, hidden]);
    const { PermanentCard } = await import("../../board/PermanentCard.tsx");
    const surface = render(<><PermanentCard objectId={visible.id} /><PermanentCard objectId={hidden.id} /></>);
    await toggleCycle(env, surface.container);
    expect(within(surface.container).getByRole("img", { name: "Card back" })).toBeInTheDocument();
  });

  it.each(["hand", "Exile", "Graveyard"] as const)(
    "V3 retains same-name printing identities through the memoized %s fan", async (kind) => {
      const env = await setup();
      const zone = kind === "hand" ? "Hand" : kind;
      const a = card({ zone, owner: 0, controller: 1 });
      const b = card({ zone, printing: printingB });
      const state: Partial<GameState> = {
        players: gameStateFactory.withPlayers({ id: 0, hand: kind === "hand" ? [a.id, b.id] : [],
          graveyard: kind === "Graveyard" ? [a.id, b.id] : [] }, 1).build().players,
        exile: kind === "Exile" ? [a.id, b.id] : [],
      };
      env.seed([a, b], state);
      act(() => env.store.setState({ legalActionsByObject: {
        [a.id]: [{ type: "CastSpell", data: { object_id: a.id, card_id: a.card_id, targets: [] } }],
        [b.id]: [{ type: "CastSpell", data: { object_id: b.id, card_id: b.card_id, targets: [] } }],
      } }));
      const { PlayerHand } = await import("../../hand/PlayerHand.tsx");
      const surface = render(<PlayerHand />);
      const target = (id: number) => surface.container.querySelector<HTMLElement>(`[data-object-id="${id}"]`)!;
      await expectImage(target(a.id), URL_A);
      await expectImage(target(b.id), URL_B);
      await toggleCycle(env, target(a.id));
      await expectImage(target(b.id), URL_B);
      const current = env.store.getState().gameState!;
      act(() => env.store.setState({ gameState: { ...current,
        objects: { ...current.objects, [a.id]: { ...a, printing: printingB }, [b.id]: { ...b, printing: null } },
        players: current.players.map((player) => player.id === 0 ? { ...player,
          hand: [...player.hand].reverse(), graveyard: [...player.graveyard].reverse() } : player),
        exile: [...current.exile].reverse(),
      } }));
      await expectImage(target(a.id), URL_B);
      await expectImage(target(b.id), URL_V);
    },
  );

  it.each(["drawer", "held", "opponent"] as const)(
    "V3 forwards the visible object on the %s hand surface", async (kind) => {
      const env = await setup();
      const object = card({ zone: "Hand", owner: kind === "opponent" ? 1 : 0,
        controller: kind === "opponent" ? 1 : 0 });
      const state: Partial<GameState> = { players: gameStateFactory.withPlayers(
        { id: 0, hand: kind === "opponent" ? [] : [object.id] },
        { id: 1, hand: kind === "opponent" ? [object.id] : [] },
      ).build().players };
      env.seed([object], state);
      env.ui.setState({ mobileHandOpen: true });
      const { MobileHandDrawer } = await import("../../hand/MobileHandDrawer.tsx");
      const { MobileHeldHandCard } = await import("../../hand/MobileHeldHandCard.tsx");
      const { OpponentHand } = await import("../../hand/OpponentHand.tsx");
      const held = (value: GameObject) => <MobileHeldHandCard object={value} gesture={{
        objectId: value.id, phase: "drag", offsetX: 0, offsetY: 0, playable: false, castReady: false,
        sourceOrigin: { bottom: 748, centerX: 280, height: 168, rotation: 0, top: 580, width: 120 },
      }} />;
      const surface = render(kind === "drawer" ? <MobileHandDrawer />
        : kind === "held" ? held(object) : <OpponentHand playerId={1} />);
      const container = kind === "opponent" ? surface.container : document.body;
      await toggleCycle(env, container);
      if (kind === "held") surface.rerender(held({ ...object, printing: undefined }));
      else env.seed([{ ...object, printing: undefined }], state);
      await expectImage(container, URL_V);
    },
  );

  it("V3 leaves a hidden opponent card backed while its projected sibling shows owner art", async () => {
    const env = await setup();
    const visible = card({ zone: "Hand", owner: 1, controller: 1 });
    const hidden = card({ zone: "Hand", owner: 1, controller: 1, name: "", printing: undefined,
      printed_ref: null, display_visible_to_viewer: false });
    env.seed([visible, hidden], { players: gameStateFactory.withPlayers(0,
      { id: 1, hand: [visible.id, hidden.id] }).build().players });
    const { OpponentHand } = await import("../../hand/OpponentHand.tsx");
    const surface = render(<OpponentHand playerId={1} />);
    await toggleCycle(env, surface.container);
    expect(within(surface.container).getByRole("img", { name: "Card back" })).toBeInTheDocument();
  });

  it.each(["peek", "attackers", "attachment"] as const)(
    "V4 forwards the selected HUD object through %s", async (kind) => {
      const env = await setup();
      const object = card({ owner: 0, controller: 1 });
      env.seed([object]);
      const { BattlefieldPeekPopover } = await import("../../hud/BattlefieldPeekPopover.tsx");
      const { IncomingAttackersPopover } = await import("../../hud/IncomingAttackersPopover.tsx");
      const { DialogAttachmentCard } = await import("../../hud/DialogAttachmentCard.tsx");
      const surface = render(kind === "peek" ? <BattlefieldPeekPopover playerId={1}
        opponentName="Opponent" seatColor="#fff" isTargeting={false} legalTargetIds={[]} />
        : kind === "attackers" ? <IncomingAttackersPopover attackerIds={[object.id]} opponentName="Opponent" />
        : <DialogAttachmentCard objectId={object.id} widthPx={120} />);
      await toggleCycle(env, surface.container);
      env.seed([{ ...object, printing: null }]);
      await expectImage(surface.container, URL_V);
      env.seed([]);
      expect(surface.container.querySelector("img")).toBeNull();
    },
  );

  it.each(["graveyard", "library", "viewer", "dock", "commander", "signature"] as const)(
    "V5 forwards the visible %s zone authority", async (kind) => {
      const env = await setup();
      env.preferences.setState({ commandZoneDisplay: "compact" });
      const object = card({
        zone: kind === "library" ? "Library" : kind === "graveyard" || kind === "viewer" ? "Graveyard" : "Command",
        is_commander: kind === "commander" || kind === "dock",
        signature_spell: kind === "signature" ? {} : undefined,
      });
      const state: Partial<GameState> = {
        players: gameStateFactory.withPlayers({ id: 0, library: [object.id], graveyard: [object.id] }, 1).build().players,
        command_zone: [object.id],
      };
      env.seed([object], state);
      const { GraveyardPile } = await import("../../zone/GraveyardPile.tsx");
      const { LibraryPile } = await import("../../zone/LibraryPile.tsx");
      const { ZoneViewer } = await import("../../zone/ZoneViewer.tsx");
      const { CommandDock } = await import("../../zone/CommandDock.tsx");
      const { CommanderCardZone } = await import("../../zone/CommanderCardZone.tsx");
      const surface = render(kind === "graveyard" ? <GraveyardPile playerId={0} onClick={vi.fn()} />
        : kind === "library" ? <LibraryPile playerId={0} />
        : kind === "viewer" ? <ZoneViewer playerId={0} zone="graveyard" onClose={vi.fn()} />
        : kind === "dock" ? <CommandDock playerId={0} isMirrored={false} />
        : <CommanderCardZone playerId={0} />);
      const container = kind === "viewer" ? document.body : surface.container;
      await toggleCycle(env, container);
      env.seed([{ ...object, printing: undefined }], state);
      await expectImage(container, URL_V);
    },
  );

  it.each(["departed", "conflicting-live", "null", "omitted", "legacy", "foreign"] as const)(
    "V6 respects present stack details for %s source printing", async (kind) => {
      const env = await setup();
      const source = card({ zone: "Stack", printing: printingB });
      const entry = buildStackEntry({ source_id: source.id });
      env.seed(kind === "departed" ? [] : [source], { stack: [entry] });
      const details = kind === "legacy" ? undefined : { source_name: "Alpha", kind_label: "Spell",
        printed_ref: { oracle_id: ORACLE, face_name: "Alpha" },
        ...(kind === "omitted" ? {} : { printing: kind === "null" ? null
          : kind === "foreign" ? foreignPrinting : printingA }) };
      const { StackEntry } = await import("../../stack/StackEntry.tsx");
      const surface = render(<StackEntry entry={entry}
        details={{ source_name: "Alpha", kind_label: "Spell", printing: printingA,
          printed_ref: { oracle_id: ORACLE, face_name: "Alpha" } }} index={0} isTop
        cardSize={{ width: 120, height: 168 }} />);
      await expectImage(surface.container, URL_A);
      surface.rerender(<StackEntry entry={entry} details={details} index={0} isTop
        cardSize={{ width: 120, height: 168 }} />);
      const expected = kind === "legacy" ? URL_B
        : kind === "null" || kind === "omitted" || kind === "foreign" ? URL_V : URL_A;
      await toggleCycle(env, surface.container, expected);
    },
  );

  it.each(["TriggeredAbility", "ActivatedAbility"] as const)(
    "C1 keeps captured emblem art authoritative on a %s stack row", async (kind) => {
      const env = await setup();
      const unrelated = card({ printing: printingB });
      const emblem = card({ zone: "Command", name: "Emblem", printed_ref: null, is_emblem: true,
        printing: printingB, emblem_source: { name: "Alpha", printing: printingA,
          printed_ref: { oracle_id: ORACLE, face_name: "Alpha" } } });
      const entry = buildStackEntry({ source_id: emblem.id, kind: kind === "TriggeredAbility"
        ? { type: kind, data: { source_id: emblem.id, source_name: "Emblem", ability: { targets: [] } } }
        : { type: kind, data: { source_id: emblem.id, ability: { targets: [] } } } });
      env.seed([emblem, unrelated], { stack: [entry], command_zone: [emblem.id] });
      // resetModules gives each case fresh resolver/store instances; static imports retain stale ones.
      const { StackEntry } = await import("../../stack/StackEntry.tsx");
      const { PreferencesModal } = await import("../../settings/PreferencesModal.tsx");
      const details = { source_name: "Emblem", kind_label: "Ability", printing: printingA,
        printed_ref: { oracle_id: ORACLE, face_name: "Alpha" } };
      const surface = render(<StackEntry entry={entry} details={details} index={0} isTop
        cardSize={{ width: 120, height: 168 }} />);
      await expectImage(surface.container, URL_A);
      const settings = render(<PreferencesModal onClose={vi.fn()} initialTab="visual" />);
      const checkbox = screen.getByRole("checkbox", { name: "Show cards as their owners chose" });
      fireEvent.click(checkbox);
      await expectImage(surface.container, URL_V);
      fireEvent.click(checkbox);
      await expectImage(surface.container, URL_A);
      settings.unmount();
      for (const printing of [null, undefined]) {
        surface.rerender(<StackEntry entry={entry} details={{ ...details, printing }} index={0} isTop
          cardSize={{ width: 120, height: 168 }} />);
        await expectImage(surface.container, URL_V);
      }
      env.seed([{ ...emblem, emblem_source: { ...emblem.emblem_source!, printing: undefined } }, unrelated],
        { stack: [entry], command_zone: [emblem.id] });
      surface.rerender(<StackEntry entry={entry} details={{ source_name: "Emblem", kind_label: "Ability" }}
        index={0} isTop cardSize={{ width: 120, height: 168 }} />);
      await expectImage(surface.container, URL_V);
    },
  );

  it("V7 uses emblem-source printing rather than the emblem or another live object", async () => {
    const env = await setup();
    const unrelated = card({ printing: printingB });
    const emblem = card({ zone: "Command", name: "Emblem", printed_ref: null, is_emblem: true,
      printing: printingB, emblem_source: { name: "Alpha", printing: printingA,
        printed_ref: { oracle_id: ORACLE, face_name: "Alpha" } } });
    const state = { command_zone: [emblem.id] };
    env.seed([emblem, unrelated], state);
    const { CommandZone } = await import("../../zone/CommandZone.tsx");
    const surface = render(<CommandZone playerId={0} />);
    await toggleCycle(env, surface.container, CROP_A, CROP_V);
    env.seed([{ ...emblem, emblem_source: { ...emblem.emblem_source!, printing: undefined } }, unrelated], state);
    await expectImage(surface.container, CROP_V);
  });

  it("V2 keeps synthesized token art independent of an absent owner printing", async () => {
    const env = await setup();
    env.preferences.setState({ battlefieldCardDisplay: "full_card" });
    const visible = card();
    const token = card({ name: "Spirit", printed_ref: null, printing: undefined, display_source: "Token",
      token_image_ref: { scryfall_id: OWNER_B, scryfall_oracle_id: OTHER_ORACLE, face_name: "Spirit", preset_id: "" } });
    env.seed([visible, token]);
    const { PermanentCard } = await import("../../board/PermanentCard.tsx");
    const surface = render(<><PermanentCard objectId={visible.id} /><PermanentCard objectId={token.id} /></>);
    await toggleCycle(env, surface.container);
    await expectImage(surface.container, URL_B);
  });

  it("V5 keeps a hidden library top backed beside an authorized public surface", async () => {
    const env = await setup();
    const visible = card({ zone: "Graveyard" });
    const hidden = card({ zone: "Library", name: "", printing: undefined, printed_ref: null,
      display_visible_to_viewer: false });
    env.seed([visible, hidden], { players: gameStateFactory.withPlayers(
      { id: 0, graveyard: [visible.id], library: [hidden.id] }, 1).build().players });
    const { GraveyardPile } = await import("../../zone/GraveyardPile.tsx");
    const { LibraryPile } = await import("../../zone/LibraryPile.tsx");
    const surface = render(<><GraveyardPile playerId={0} onClick={vi.fn()} /><LibraryPile playerId={0} /></>);
    await toggleCycle(env, surface.container);
    expect(within(surface.container).getByRole("img", { name: "Card back" })).toBeInTheDocument();
  });
  it.each([undefined, null, foreignPrinting])(
    "V14 rejects absent or foreign printing %j without changing normal identity", async (pair) => {
      const env = await setup();
      const { CardImage } = await import("../CardImage.tsx");
      const surface = render(<CardImage cardName="Alpha" oracleId={ORACLE} ownerPrinting={printingA} />);
      await toggleCycle(env, surface.container);
      surface.rerender(<CardImage cardName="Alpha" oracleId={ORACLE} ownerPrinting={pair} />);
      await expectImage(surface.container, URL_V);
    },
  );
});

describe("owner printing remaining carrier surfaces (group 2)", () => {
  it.each(["Declare", "BottomCards", "OpeningHandBottomCards"] as const)(
    "T2 binds homonymous owner art on the GamePage %s route", async (route) => {
      const env = await setup({ homonyms: true });
      const object = card({ name: "Twin", zone: "Hand", printing: printingB,
        printed_ref: { oracle_id: OTHER_ORACLE, face_name: "Twin" } });
      const waitingFor: WaitingFor = route === "OpeningHandBottomCards"
        ? { type: "OpeningHandBottomCards", data: { pending: [{ player: 0, count: 1 }], reason: { type: "TinyLeadersMultiCommander" } } }
        : { type: "MulliganDecision", data: { free_first_mulligan: false, pending: [{
          player: 0, mulligan_count: 1, phase: route === "Declare" ? { type: "Declare" }
            : { type: "BottomCards", count: 1, then: { type: "Keep" } },
        }] } };
      env.seed([object], { waiting_for: waitingFor,
        players: gameStateFactory.withPlayers({ id: 0, hand: [object.id] }, 1).build().players });
      env.store.setState({ gameId: "owner-art", legalActions: [
        { type: "MulliganDecision", data: { choice: { type: "Keep" } } },
        { type: "MulliganDecision", data: { choice: { type: "Mulligan" } } },
      ] });
      // resetModules requires the real route to bind this fixture's fresh stores.
      const { GamePage } = await import("../../../pages/GamePage.tsx");
      render(<MemoryRouter initialEntries={["/game/owner-art?mode=join"]}>
        <Routes><Route path="/game/:id" element={<GamePage />} /></Routes>
      </MemoryRouter>);
      const heading = await screen.findByRole("heading", { name: route === "Declare"
        ? "Review your opening hand" : "Put 1 card on the bottom" });
      const prompt = heading.closest<HTMLElement>(".card-scale-reset")!;
      await toggleCycle(env, prompt, URL_B);
      const current = env.store.getState().gameState!;
      act(() => env.store.setState({ gameState: { ...current, objects: {
        ...current.objects, [object.id]: { ...object, printing: printingA },
      } } }));
      await expectImage(prompt, URL_V);
      expect(prompt.querySelector(`img[src="${URL_A}"]`)).toBeNull();
    },
  );

  it.each(["Declare", "BottomCards", "OpeningHandBottomCards"] as const)(
    "V3a/V3b renders ordered same-name owner art through the real GamePage %s route", async (route) => {
      const env = await setup();
      const a = card({ zone: "Hand" });
      const b = card({ zone: "Hand", printing: printingB });
      const hovered = card({ printing: printingB });
      const waitingFor: WaitingFor = route === "OpeningHandBottomCards"
        ? { type: "OpeningHandBottomCards", data: { pending: [{ player: 0, count: 1 }], reason: { type: "TinyLeadersMultiCommander" } } }
        : { type: "MulliganDecision", data: { free_first_mulligan: false, pending: [{
          player: 0, mulligan_count: 1, phase: route === "Declare" ? { type: "Declare" }
            : { type: "BottomCards", count: 1, then: { type: "Keep" } },
        }] } };
      env.seed([a, b, hovered], { waiting_for: waitingFor,
        players: gameStateFactory.withPlayers({ id: 0, hand: [a.id, b.id] },
          { id: 1, hand: [hovered.id] }).build().players });
      act(() => {
        env.store.setState({ gameId: "owner-art", legalActions: [
          { type: "MulliganDecision", data: { choice: { type: "Keep" } } },
          { type: "MulliganDecision", data: { choice: { type: "Mulligan" } } },
        ] });
        env.ui.setState({ inspectedObjectId: hovered.id });
      });
      const { GamePage } = await import("../../../pages/GamePage.tsx");
      render(<MemoryRouter initialEntries={["/game/owner-art?mode=join"]}>
        <Routes><Route path="/game/:id" element={<GamePage />} /></Routes>
      </MemoryRouter>);
      const heading = await screen.findByRole("heading", { name: route === "Declare"
        ? "Review your opening hand" : "Put 1 card on the bottom" });
      const prompt = heading.closest<HTMLElement>(".card-scale-reset")!;
      const urls = () => [...prompt.querySelectorAll<HTMLImageElement>(".modal-card-area img")].map((image) => image.getAttribute("src"));
      await waitFor(() => expect(urls()).toEqual([URL_A, URL_B]));
      if (route !== "Declare") {
        const confirm = within(prompt).getByRole("button", { name: "Confirm Selection" });
        expect(confirm).toBeDisabled();
        fireEvent.click(prompt.querySelectorAll<HTMLImageElement>(".modal-card-area img")[1].closest("button")!);
        expect(confirm).toBeEnabled();
      }
      act(() => env.preferences.getState().setShowOwnerPrintings(false));
      await waitFor(() => expect(urls()).toEqual([URL_V, URL_V]));
      act(() => env.preferences.getState().setShowOwnerPrintings(true));
      await waitFor(() => expect(urls()).toEqual([URL_A, URL_B]));
      const current = env.store.getState().gameState!;
      act(() => env.store.setState({ gameState: { ...current, objects: {
        ...current.objects, [a.id]: { ...a, printing: undefined },
      } } }));
      await waitFor(() => expect(urls()).toEqual([URL_V, URL_B]));
      if (route !== "Declare") {
        expect(env.ui.getState().selectedCardIds).toEqual([b.id]);
        fireEvent.click(within(prompt).getByRole("button", { name: "Confirm Selection" }));
        expect(dispatch).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [b.id] } });
      }
      const other: WaitingFor = route === "OpeningHandBottomCards"
        ? { type: "OpeningHandBottomCards", data: { pending: [{ player: 1, count: 1 }], reason: { type: "TinyLeadersMultiCommander" } } }
        : { type: "MulliganDecision", data: { free_first_mulligan: false,
          pending: [route === "BottomCards"
            ? { player: 1, mulligan_count: 1, phase: { type: "BottomCards", count: 1, then: { type: "Keep" } } }
            : { player: 1, mulligan_count: 0, phase: { type: "Declare" } }] } };
      act(() => env.store.setState({ waitingFor: other, gameState: { ...env.store.getState().gameState!, waiting_for: other } }));
      await waitFor(() => {
        expect(screen.queryByRole("heading", { name: heading.textContent! })).toBeNull();
        if (route !== "Declare") {
          expect(screen.queryByRole("button", { name: "Confirm Selection" })).toBeNull();
        }
      });
    },
  );

  it.each(["commander", "reveal-until", "partition", "pile-choice", "choice", "alternative", "type-slot"] as const)(
    "V8 shows the exact object's printing in the %s modal", async (kind) => {
      const env = await setup();
      const object = card({ zone: "Library" });
      const hovered = card({ printing: printingB });
      let waitingFor: WaitingFor = { type: "Priority", data: { player: 0 } };
      if (kind === "commander") waitingFor = { type: "CommanderZoneChoice", data: { player: 0, commander_id: object.id, current_zone: "Graveyard" } };
      if (kind === "reveal-until") waitingFor = { type: "RevealUntilKeptChoice", data: {
        player: 0, hit_card: object.id, source_id: hovered.id, accept_zone: "Battlefield", decline_zone: "Graveyard",
        enter_tapped: false, enters_attacking: false, revealed_misses: [], rest_destination: "Graveyard",
      } };
      if (kind === "alternative") waitingFor = { type: "AlternativeCastChoice", data: {
        player: 0, object_id: object.id, card_id: object.card_id, keyword: { type: "Cleave" },
        normal_cost: { type: "NoCost" }, alternative_cost: { type: "NoCost" },
        alternative_additional_cost: null, alternative_additional_cost_description: null,
      } };
      if (kind === "type-slot") waitingFor = { type: "ChoosePermanentTypeSlot", data: {
        player: 0, object_id: object.id, card_id: object.card_id, source: hovered.id, available_slots: ["Creature"],
      } };
      env.seed([object, hovered], { waiting_for: waitingFor });
      env.ui.setState({ inspectedObjectId: hovered.id });
      const { CardChoiceModal } = await import("../../modal/CardChoiceModal.tsx");
      const { SeparatePilesPartitionModal, SeparatePilesChoiceModal } = await import("../../modal/SeparatePilesModal.tsx");
      const { ChoiceModal } = await import("../../modal/ChoiceModal.tsx");
      const { AlternativeCostModal } = await import("../../modal/AlternativeCostModal.tsx");
      const { PermanentTypeSlotModal } = await import("../../modal/PermanentTypeSlotModal.tsx");
      render(kind === "partition" ? <SeparatePilesPartitionModal data={{
        player: 0, eligible: [object.id], remaining_subjects: [], completed: [], chooser: 1, source_id: hovered.id,
      }} /> : kind === "pile-choice" ? <SeparatePilesChoiceModal data={{
        player: 0, pending: [], current: { subject: 1, pile_a: [object.id], pile_b: [] }, source_id: hovered.id,
      }} /> : kind === "choice" ? <ChoiceModal title="Object choice" previewCardName="Alpha"
        previewObjectId={object.id} options={[{ id: "yes", label: "Choose" }]} onChoose={dispatch} />
        : kind === "alternative" ? <AlternativeCostModal />
        : kind === "type-slot" ? <PermanentTypeSlotModal /> : <CardChoiceModal />);
      await toggleCycle(env, document.body);
      const current = env.store.getState().gameState!;
      act(() => env.store.setState({ gameState: { ...current, objects: { ...current.objects,
        [object.id]: { ...object, printing: null },
      } } }));
      await expectImage(document.body, URL_V);
      expect(document.querySelector(`img[src="${URL_B}"]`)).toBeNull();
    },
  );

  it.each([undefined, 99999])("V8 keeps a missing ChoiceModal subject %j ownerless despite another hover", async (subject) => {
    const env = await setup();
    const hovered = card({ printing: printingB });
    env.seed([hovered]);
    env.ui.setState({ inspectedObjectId: hovered.id });
    const { ChoiceModal } = await import("../../modal/ChoiceModal.tsx");
    const surface = render(<ChoiceModal title="Ownerless" previewCardName="Alpha" previewObjectId={subject}
      options={[{ id: "yes", label: "Choose" }]} onChoose={dispatch} />);
    await expectImage(document.body, URL_V);
    surface.rerender(<ChoiceModal title="Bound" previewCardName="Alpha" previewObjectId={hovered.id}
      options={[{ id: "yes", label: "Choose" }]} onChoose={dispatch} />);
    await toggleCycle(env, document.body, URL_B);
  });

  it("V8 carries GamePage's ability-choice subject into the real textbox", async () => {
    const env = await setup();
    const object = card();
    env.seed([object]);
    env.store.setState({ gameId: "owner-art" });
    env.ui.setState({ pendingAbilityChoice: { objectId: object.id,
      actions: [{ type: "ActivateAbility", data: { source_id: object.id, ability_index: 0 } }] } });
    const { GamePage } = await import("../../../pages/GamePage.tsx");
    render(<MemoryRouter initialEntries={["/game/owner-art?mode=join"]}>
      <Routes><Route path="/game/:id" element={<GamePage />} /></Routes>
    </MemoryRouter>);
    const prompt = (await screen.findByRole("heading", { name: "Alpha" })).closest<HTMLElement>('[role="dialog"]') ?? document.body;
    await toggleCycle(env, prompt);
  });

  it.each(["search", "library", "grid", "retarget"] as const)("V9 inherits owner art in the shared %s family", async (kind) => {
    const env = await setup();
    const a = card({ zone: "Library" });
    const b = card({ zone: "Library", printing: printingB });
    env.seed([a, b], {
      waiting_for: { type: "SearchChoice", data: { player: 0, cards: [a.id], count: 1 } },
      active_library_searches: { 0: { searcher: 0, searched_zone_owner: 0, effective_library_owner: 0,
        learned_audience: [0], looked_at: [[0, "Library", { object_id: a.id, incarnation: 0 }],
          [0, "Library", { object_id: b.id, incarnation: 0 }]] } },
    });
    const { CardChoiceModal } = await import("../../modal/CardChoiceModal.tsx");
    const { ReorderableTopChoice } = await import("../../modal/cardChoice/libraryModals.tsx");
    const { default: SelectableCardGrid } = await import("../../modal/cardChoice/SelectableCardGrid.tsx");
    const { RetargetChoiceModal } = await import("../../modal/RetargetChoiceModal.tsx");
    render(kind === "search" ? <CardChoiceModal />
      : kind === "library" ? <ReorderableTopChoice cards={[a.id]} title="Order" subtitle=""
        keepLabel="Top" restLabel="Bottom" reorderHint="" keepTone="emerald" />
      : kind === "grid" ? <SelectableCardGrid cards={[a.id, 99999]} objects={{ [a.id]: a }}
        value={new Set()} onChange={vi.fn()} cap={1} tone={{ ring: "", overlay: "", badge: "" }}
        badgeLabel="Choose" counterText="" hoverProps={() => ({})} />
      : <RetargetChoiceModal data={{ player: 0, stack_entry_index: 0, scope: { type: "Single" },
        current_targets: [], slots: [], slot_pools: [], legal_new_targets: [{ Object: a.id }] }} />);
    await toggleCycle(env, document.body);
    if (kind === "search") {
      await expectImage(document.body, URL_B);
      const disabled = document.querySelector<HTMLImageElement>(`img[src="${URL_B}"]`)!.closest("button");
      expect(disabled).toBeDisabled();
      fireEvent.click(document.querySelector<HTMLImageElement>(`img[src="${URL_A}"]`)!.closest("button")!);
      fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
      expect(dispatch).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [a.id] } });
    }
  });

  it("F1 keeps a catalog hover ownerless while explicit game previews follow their object", async () => {
    const env = await setup({ holdCatalog: true });
    const object = card();
    env.seed([object]);
    env.preferences.setState({ cardPreviewMode: "follow" });
    env.ui.setState({ inspectedObjectId: object.id, previewSticky: true });
    const { HoverCardPreview } = await import("../HoverCardPreview.tsx");
    const { GameCardPreview } = await import("../GameCardPreview.tsx");
    const catalog = render(<HoverCardPreview mode="follow" hoverDelayMs={0} card={{
      name: "Alpha", scryfallId: VIEWER, sourcePrinting: { setCode: "pin", collectorNumber: "3" },
    }} />);
    const linked = render(<GameCardPreview />);
    const expectCatalog = async () => {
      await waitFor(() => expect(within(catalog.container).getByRole("img", { name: "Alpha" }))
        .toHaveAttribute("src", URL_V));
    };
    await expectCatalog();
    await expectImage(linked.container, URL_V);
    await act(async () => env.admit());
    await expectImage(linked.container, URL_A);
    await expectCatalog();
    for (const show of [false, true]) {
      act(() => env.preferences.getState().setShowOwnerPrintings(show));
      await expectImage(linked.container, show ? URL_A : URL_V);
      await expectCatalog();
    }
    const current = env.store.getState().gameState!;
    act(() => env.store.setState({ gameState: { ...current, objects: { ...current.objects,
      [object.id]: { ...object, printing: printingB },
    } } }));
    await expectImage(linked.container, URL_B);
    await expectCatalog();
  });

  it.each([false, true])("V10 uses physical owner printing for a real preview (mobile=%s)", async (mobile) => {
    const env = await setup({ dfc: true });
    observation.mobile = mobile;
    const object = card({ back_face: {
      name: "Reverse", power: 3, toughness: 3, mana_cost: { type: "NoCost" },
      card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] }, color: [], keywords: [], abilities: [],
      printed_ref: { oracle_id: ORACLE, face_name: "Reverse" },
      layout_kind: "Transform",
    } });
    const sibling = card({ printing: printingB });
    env.seed([object, sibling]);
    env.ui.setState({ inspectedObjectId: object.id, previewSticky: true });
    const { GameCardPreview } = await import("../GameCardPreview.tsx");
    render(<GameCardPreview />);
    await toggleCycle(env, document.body);
    if (!mobile) {
      fireEvent.keyDown(window, { key: "Control", ctrlKey: true });
      await toggleCycle(env, document.body, "/owner-a-back.jpg", "/viewer-back.jpg");
      fireEvent.keyUp(window, { key: "Control" });
    }
    act(() => env.ui.setState({ inspectedObjectId: sibling.id }));
    await expectImage(document.body, URL_B);
    const current = env.store.getState().gameState!;
    act(() => env.store.setState({ gameState: { ...current, objects: { ...current.objects,
      [sibling.id]: { ...sibling, printing: undefined },
    } } }));
    await expectImage(document.body, URL_V);
  });

  it("V7 shows an emblem's source printing in its object-linked preview", async () => {
    const env = await setup();
    const emblem = card({ name: "Emblem", printed_ref: null, is_emblem: true, printing: printingB,
      emblem_source: { name: "Alpha", printing: printingA, printed_ref: { oracle_id: ORACLE, face_name: "Alpha" } } });
    env.seed([emblem]);
    env.ui.setState({ inspectedObjectId: emblem.id, previewSticky: true });
    const { GameCardPreview } = await import("../GameCardPreview.tsx");
    render(<GameCardPreview />);
    await toggleCycle(env, document.body);
    env.seed([{ ...emblem, emblem_source: { ...emblem.emblem_source!, printing: undefined } }]);
    await expectImage(document.body, URL_V);
  });

  it("V11 captures each revealed library row independently of same-name siblings", async () => {
    const env = await setup();
    const a = card({ zone: "Library" });
    const b = card({ zone: "Library", printing: printingB });
    const excluded = card({ zone: "Hand" });
    env.seed([a, b, excluded], { revealed_cards: [a.id, b.id, excluded.id],
      players: gameStateFactory.withPlayers({ id: 0, library: [a.id, b.id], hand: [excluded.id] }, 1).build().players });
    const { RevealOverlay } = await import("../../animation/RevealOverlay.tsx");
    const surface = render(<RevealOverlay />);
    await toggleCycle(env, surface.container);
    await expectImage(surface.container, URL_B);
    expect(surface.container.querySelectorAll("img")).toHaveLength(2);
    const current = env.store.getState().gameState!;
    act(() => env.store.setState({ gameState: { ...current, objects: { ...current.objects,
      [a.id]: { ...a, printing: null },
    } } }));
    await expectImage(surface.container, URL_V);
    await expectImage(surface.container, URL_B);
  });

  it.each(["normal", "art_crop"] as const)("V12 retains captured printing after departure for %s", async (size) => {
    const env = await setup();
    const a = card();
    env.seed([a]);
    const { visibleAnimationImageSnapshot, ResolvedAnimationImage } = await import("../../animation/ResolvedAnimationImage.tsx");
    const snapshot = visibleAnimationImageSnapshot(a)!;
    expect(visibleAnimationImageSnapshot({ ...a, display_visible_to_viewer: false })).toBeNull();
    env.seed([{ ...a, printing: printingB }]);
    const surface = render(<ResolvedAnimationImage snapshot={snapshot} size={size} alt="Captured" fallback={null} />);
    await toggleCycle(env, surface.container, size === "normal" ? URL_A : CROP_A, size === "normal" ? URL_V : CROP_V);
    env.seed([]);
    await expectImage(surface.container, size === "normal" ? URL_A : CROP_A);
    surface.rerender(<ResolvedAnimationImage snapshot={{ ...snapshot, ownerPrinting: undefined }}
      size={size} alt="Captured" fallback={null} />);
    await expectImage(surface.container, size === "normal" ? URL_V : CROP_V);
  });

  it.each(["cast", "mill"] as const)("V12 forwards a captured face through real %s consumers", async (kind) => {
    const env = await setup();
    const object = card();
    env.seed([object]);
    const { visibleAnimationImageSnapshot } = await import("../../animation/ResolvedAnimationImage.tsx");
    const snapshot = visibleAnimationImageSnapshot(object)!;
    const { CastArcAnimation } = await import("../../animation/CastArcAnimation.tsx");
    const { MillRevealAnimation } = await import("../../animation/MillRevealAnimation.tsx");
    const surface = render(kind === "cast" ? <CastArcAnimation snapshot={snapshot} from={{ x: 10, y: 10 }}
      to={{ x: 100, y: 100 }} mode="cast" onComplete={vi.fn()} />
      : <MillRevealAnimation cards={[{ objectId: object.id, snapshot, colors: [] }]}
        from={{ x: 10, y: 10 }} to={{ x: 100, y: 100 }} onComplete={vi.fn()} />);
    await toggleCycle(env, surface.container);
  });

  it("V13 uses only the authorized debug row, not a contradictory concealed object", async () => {
    const env = await setup();
    const hidden = card({ zone: "Library", name: "", printed_ref: null, printing: printingB,
      display_visible_to_viewer: false });
    env.seed([hidden], { derived: { debug_library_cards: [{ object_id: hidden.id, name: "Alpha",
      printing: printingA, printed_ref: { oracle_id: ORACLE, face_name: "Alpha" } }] } });
    env.ui.setState({ debugLibraryViewer: { playerId: 0 } });
    const { DebugLibraryViewer } = await import("../../chrome/DebugLibraryViewer.tsx");
    render(<DebugLibraryViewer />);
    await toggleCycle(env, document.body);
    const current = env.store.getState().gameState!;
    act(() => env.store.setState({ gameState: { ...current, derived: { ...current.derived,
      debug_library_cards: [{ object_id: hidden.id, name: "Alpha", printing: null }],
    } } }));
    await expectImage(document.body, URL_V);
    act(() => env.store.setState({ gameState: { ...current, derived: { ...current.derived, debug_library_cards: [] } } }));
    await waitFor(() => expect(document.querySelector(`img[src="${URL_V}"]`)).toBeNull());
  });

  it.each(["gl", "death"] as const)(
    "V12a observes cold normal consumption, retired loader, persistent admission and warm owner in real %s loader", async (leg) => {
      const env = await setup({ holdCatalog: true });
      env.preferences.setState({ cardAnimationStyle: "classic", vfxQuality: "full", animationSpeedMultiplier: 1 });
      const coldObject = card();
      env.seed([coldObject]);
      const { ResolvedAnimationImage, visibleAnimationImageSnapshot } = await import("../../animation/ResolvedAnimationImage.tsx");
      const captured = visibleAnimationImageSnapshot(coldObject)!;
      const persistent = render(<>
        <ResolvedAnimationImage snapshot={captured} size="normal" alt="Persistent full" fallback={null} />
        <ResolvedAnimationImage snapshot={captured} size="art_crop" alt="Persistent crop" fallback={null} />
      </>);
      await expectImage(persistent.container, URL_V);
      await expectImage(persistent.container, CROP_V);
      vi.spyOn(HTMLImageElement.prototype, "decode").mockResolvedValue(undefined);
      const anchor = (id: number, zone: "hand" | "stack") => {
        const element = document.createElement("div");
        element.setAttribute("data-owner-probe", "");
        element.setAttribute(zone === "hand" ? "data-hand-card" : "data-stack-entry", zone === "hand" ? "" : String(id));
        element.setAttribute("data-object-id", String(id));
        Object.defineProperty(element, "offsetWidth", { configurable: true, value: 80 });
        Object.defineProperty(element, "offsetHeight", { configurable: true, value: 112 });
        element.getBoundingClientRect = () => new DOMRect(20, zone === "hand" ? 400 : 80, 80, 112);
        document.body.appendChild(element);
      };
      const { CardVfxLayer } = await import("../../animation/cardVfx/CardVfxLayer.tsx");
      const { AnimationOverlay } = await import("../../animation/AnimationOverlay.tsx");
      const ref = createRef<CardVfxLayerHandle>();
      const surface = render(leg === "gl" ? <CardVfxLayer tier="full" ref={ref} />
        : <AnimationOverlay containerRef={{ current: null }} />);
      const spec = (object: GameObject): CardFlightSpec => ({
        kind: "flight", objectId: object.id, sourceId: object.id,
        route: { from: "Hand", to: "Stack", ownerId: 0 },
        startFace: visibleAnimationImageSnapshot(object), endFace: visibleAnimationImageSnapshot(object),
        endColors: null, pace: 1, owningStepMs: 2000, snapshotSeq: env.store.getState().lastCommittedSeq + 1,
        delayMs: 0, wash: null, sweep: null,
      });
      const destroy = (object: GameObject) => {
        observation.snapshot.set(object.id, new DOMRect(20, 30, 80, 112));
        act(() => {
          env.animation.getState().clearQueue();
          env.seed([object]);
          env.animation.getState().setAnimationNewState(gameStateFactory.withPlayers(0, 1).build());
          env.animation.getState().enqueueSteps([{ duration: 2000, effects: [{
            duration: 2000, event: { type: "CreatureDestroyed", data: { object_id: object.id, source_id: null } },
          }] }], 1);
        });
      };
      if (leg === "gl") {
        anchor(coldObject.id, "hand");
        anchor(coldObject.id, "stack");
        await act(async () => { await vi.dynamicImportSettled(); });
        act(() => ref.current!.present(spec(coldObject), vi.fn()));
        const back = await waitFor(() => {
          const image = surface.container.querySelector<HTMLImageElement>("img");
          expect(image).not.toBeNull();
          return image!;
        });
        fireEvent.load(back);
        await waitFor(() => expect(env.animation.getState().cardVfxReady).toBe(true));
        act(() => ref.current!.present(spec(coldObject), vi.fn()));
      } else destroy(coldObject);
      const coldUrl = leg === "gl" ? URL_V : CROP_V;
      await expectImage(surface.container, coldUrl);
      const coldLoader = surface.container.querySelector<HTMLImageElement>(`img[src="${coldUrl}"]`)!;
      fireEvent.load(coldLoader);
      await waitFor(() => expect(leg === "gl"
        ? observation.textures.some((texture) => texture.image === coldLoader)
        : observation.deaths.includes(coldLoader)).toBe(true));
      await waitFor(() => expect(surface.container.contains(coldLoader)).toBe(false));
      const coldCount = leg === "gl" ? observation.textures.length : observation.deaths.length;
      await act(async () => env.admit());
      await expectImage(persistent.container, URL_A);
      await expectImage(persistent.container, CROP_A);
      expect(coldLoader.getAttribute("src")).toBe(coldUrl);
      expect(leg === "gl" ? observation.textures.length : observation.deaths.length).toBe(coldCount);
      if (leg === "gl") expect(env.animation.getState().flightVeiledObjectIds.has(coldObject.id)).toBe(true);
      else expect(screen.getByTestId("consumed-death-image")).toBeInTheDocument();
      const warmObject = card();
      if (leg === "gl") {
        anchor(warmObject.id, "hand");
        anchor(warmObject.id, "stack");
        act(() => ref.current!.present(spec(warmObject), vi.fn()));
      } else {
        await act(async () => { await delay(650); });
        destroy(warmObject);
      }
      const warmUrl = leg === "gl" ? URL_A : CROP_A;
      // Inspect the first mounted face before waiting or firing load: a warm
      // resolver must not need a normal-frame race to settle on owner art.
      const firstWarmImages = surface.container.querySelectorAll<HTMLImageElement>("div[hidden] img");
      expect(firstWarmImages[firstWarmImages.length - 1]?.getAttribute("src")).toBe(warmUrl);
      await expectImage(surface.container, warmUrl);
      const warmLoader = surface.container.querySelector<HTMLImageElement>(`img[src="${warmUrl}"]`)!;
      fireEvent.load(warmLoader);
      await waitFor(() => expect(leg === "gl"
        ? observation.textures.some((texture) => texture.image === warmLoader)
        : observation.deaths.includes(warmLoader)).toBe(true));
      await waitFor(() => expect(surface.container.contains(warmLoader)).toBe(false));
      expect(warmLoader.getAttribute("src")).toBe(warmUrl);
      act(() => env.store.setState((state) => ({ lastCommittedSeq: state.lastCommittedSeq + 1, engineCommitEpoch: state.engineCommitEpoch + 1 })));
      await act(async () => { await delay(1200); });
      if (leg === "death") expect(screen.queryByTestId("consumed-death-image")).toBeNull();
    },
  );

  // The census parses the resolver-bearing files of the whole client tree. It takes
  // about 1.3 s alone but several times that inside the parallel suite, so it carries
  // its own limit instead of vitest's 5 s default (the work, not the limit, is the check).
  const V15_CENSUS_TIMEOUT_MS = 30_000;

  it("V15 classifies every import-bound image resolver and intermediate carrier", () => {
    const src = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
    const objectHooks: Record<string, number> = {
      "components/card/ArtCropCard.tsx": 1, "components/card/CardImage.tsx": 1,
      "components/card/CardPreview.tsx": 2, "components/hand/MobileHandDrawer.tsx": 1,
      "components/hand/OpponentHand.tsx": 1, "components/modal/CardTextboxPreview.tsx": 1,
      "components/stack/StackEntry.tsx": 1, "components/zone/CommandDock.tsx": 1,
      "components/zone/CommandZone.tsx": 1, "components/zone/CommanderCardZone.tsx": 1,
      "components/zone/GraveyardPile.tsx": 1, "components/zone/LibraryPile.tsx": 1,
      "components/animation/ResolvedAnimationImage.tsx": 1, "components/animation/RevealOverlay.tsx": 1,
    };
    const excludedHooks: Record<string, number> = {
      "components/card/CardReportDialog.tsx": 1, "components/hand/CompanionFanCard.tsx": 1,
      "components/controls/CardCoverageDashboard.tsx": 1, "components/deck-builder/CardGrid.tsx": 1,
      "components/deck-builder/DeckStack.tsx": 1, "components/deck-builder/PrintingPickerModal.tsx": 1,
      "components/draft/DraftCardFace.tsx": 1, "components/draft/LimitedDeckBuilder.tsx": 1,
      "components/draft/SealedPackOpening.tsx": 1, "components/menu/MyDecks.tsx": 1,
      "components/menu/home/HomeDashboard.tsx": 1, "pages/GameSetupPage.tsx": 1,
      "hooks/usePlayerAvatarImage.ts": 1,
    };
    const expectedImages: Record<string, number> = {
      "components/board/PermanentCard.tsx": 2, "components/board/AttachmentFan.tsx": 1,
      "components/board/ArchenemyPanel.tsx": 1, "components/board/PlanechasePanel.tsx": 1,
      "components/hand/PlayerHand.tsx": 2, "components/hand/MobileHeldHandCard.tsx": 1,
      "components/hud/BattlefieldPeekPopover.tsx": 1, "components/hud/IncomingAttackersPopover.tsx": 1,
      "components/hud/DialogAttachmentCard.tsx": 1, "components/zone/ZoneViewer.tsx": 1,
      "components/modal/CardChoiceModal.tsx": 22, "components/modal/cardChoice/libraryModals.tsx": 6,
      "components/modal/cardChoice/SelectableCardGrid.tsx": 1, "components/modal/RetargetChoiceModal.tsx": 1,
      "components/modal/SeparatePilesModal.tsx": 2, "components/chrome/DebugLibraryViewer.tsx": 1,
      "pages/GamePage.tsx": 2,
    };
    const expectedHelpers: Record<string, number> = {
      "components/animation/ResolvedAnimationImage.tsx": 1, "components/stack/StackEntry.tsx": 1,
      "components/zone/CommandDock.tsx": 1, "components/zone/CommanderCardZone.tsx": 1,
      "components/zone/GraveyardPile.tsx": 1, "components/zone/LibraryPile.tsx": 1,
      "components/zone/ZoneViewer.tsx": 1, "components/modal/CardChoiceModal.tsx": 20,
      "components/modal/cardChoice/libraryModals.tsx": 6,
      "components/modal/cardChoice/SelectableCardGrid.tsx": 1, "components/modal/RetargetChoiceModal.tsx": 1,
    };
    const hooks: Record<string, number> = {};
    const images: Record<string, number> = {};
    const helpers: Record<string, number> = {};
    const exclusions: Record<string, number> = {};
    const intermediates: Record<string, number> = {};
    const failures: string[] = [];
    function enclosing(node: ts.Node): string {
      for (let current: ts.Node | undefined = node.parent; current; current = current.parent) {
        if ((ts.isFunctionDeclaration(current) || ts.isFunctionExpression(current)) && current.name) return current.name.text;
        if (ts.isVariableDeclaration(current) && ts.isIdentifier(current.name)) return current.name.text;
      }
      return "<module>";
    }
    function scan(directory: string) {
      for (const entry of readdirSync(directory, { withFileTypes: true })) {
        const path = join(directory, entry.name);
        if (entry.isDirectory()) {
          if (entry.name !== "__tests__") scan(path);
          continue;
        }
        if (!/\.tsx?$/.test(path) || /\.(test|stories)\./.test(path)) continue;
        const file = relative(src, path).split("\\").join("/");
        const text = readFileSync(path, "utf8");
        // Every count and every failure branch below requires one of these identifiers
        // somewhere in the file (imports keep the imported name, and the special-cased
        // files contain them too), so a file without them cannot change the census.
        // Skipping its parse keeps discovery whole-tree inside the test's time budget.
        if (!/useCardImage|CardImage|objectImageProps|HandCard|ZoneFanCard|DrawerCard|CardTextboxPreview/.test(text)) continue;
        const source = ts.createSourceFile(path, text, ts.ScriptTarget.Latest, true);
        const bindings: Record<string, Set<string>> = { useCardImage: new Set(), CardImage: new Set(), objectImageProps: new Set() };
        const namespaces: Record<string, string> = {};
        const expectedModule: Record<string, string> = {
          useCardImage: "hooks/useCardImage", CardImage: "components/card/CardImage", objectImageProps: "services/cardImageLookup",
        };
        for (const statement of source.statements) {
          if (!ts.isImportDeclaration(statement) || !ts.isStringLiteral(statement.moduleSpecifier)) continue;
          const module = relative(src, resolve(dirname(path), statement.moduleSpecifier.text)).split("\\").join("/").replace(/\.tsx?$/, "");
          const named = statement.importClause?.namedBindings;
          if (named && ts.isNamedImports(named)) {
            for (const binding of named.elements) {
              const name = binding.propertyName?.text ?? binding.name.text;
              if (expectedModule[name] === module) bindings[name].add(binding.name.text);
            }
          } else if (named && ts.isNamespaceImport(named)) namespaces[named.name.text] = module;
        }
        function matches(expression: ts.Node, kind: string): boolean {
          return ts.isIdentifier(expression) ? bindings[kind].has(expression.text)
            : ts.isPropertyAccessExpression(expression) && ts.isIdentifier(expression.expression)
              && namespaces[expression.expression.text] === expectedModule[kind] && expression.name.text === kind;
        }
        const ownerField = (properties: ts.NodeArray<ts.ObjectLiteralElementLike>) => properties.find((property) =>
          property.name && ts.isIdentifier(property.name) && property.name.text === "ownerPrinting");
        function visit(node: ts.Node) {
          const location = `${file}:${enclosing(node)}`;
          if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.initializer) {
            for (const kind of Object.keys(bindings)) if (matches(node.initializer, kind)) bindings[kind].add(node.name.text);
          }
          if (ts.isCallExpression(node) && matches(node.expression, "useCardImage")) {
            hooks[file] = (hooks[file] ?? 0) + 1;
            const options = node.arguments[1];
            const properties = options && ts.isObjectLiteralExpression(options) ? options.properties : undefined;
            if (objectHooks[file]) {
              const owner = properties && ownerField(properties);
              if (!owner || ts.isPropertyAssignment(owner) && ts.isIdentifier(owner.initializer) && owner.initializer.text === "undefined")
                failures.push(`unforwarded useCardImage at ${location}`);
              if (properties?.some((property) => property.name?.getText(source) === "showOwnerPrintings"))
                failures.push(`explicit toggle override at ${location}; the public facade owns selection`);
            } else if (!excludedHooks[file]) failures.push(`unclassified useCardImage at ${location}`);
          }
          if (ts.isCallExpression(node) && matches(node.expression, "objectImageProps")) helpers[file] = (helpers[file] ?? 0) + 1;
          if (ts.isJsxSelfClosingElement(node) || ts.isJsxOpeningElement(node)) {
            const attributes = node.attributes.properties;
            const attr = (name: string) => attributes.find((attribute) => ts.isJsxAttribute(attribute) && attribute.name.getText(source) === name);
            const tag = node.tagName.getText(source);
            if (["HandCard", "ZoneFanCard", "DrawerCard", "CardTextboxPreview"].includes(tag)) {
              if (!attr("ownerPrinting")) failures.push(`unforwarded intermediate ${tag} at ${location}`);
              intermediates[tag] = (intermediates[tag] ?? 0) + 1;
            }
            if (file === "pages/GamePage.tsx" && tag === "ChoiceModal" && enclosing(node) === "AbilityChoiceModal" && !attr("previewObjectId"))
              failures.push(`missing ability subject at ${location}`);
            if (matches(node.tagName, "CardImage")) {
              const excluded = file === "components/modal/BoosterPackModal.tsx"
                || file === "pages/GamePage.tsx" && enclosing(node) === "CompanionRevealPrompt";
              if (excluded) exclusions[file] = (exclusions[file] ?? 0) + 1;
              else {
                images[file] = (images[file] ?? 0) + 1;
                const approvedSpread = attributes.some((attribute) => ts.isJsxSpreadAttribute(attribute)
                  && ts.isCallExpression(attribute.expression)
                  && (matches(attribute.expression.expression, "objectImageProps")
                    || file === "components/modal/SeparatePilesModal.tsx" && attribute.expression.expression.getText(source) === "objectImageProps"));
                if (!attr("ownerPrinting") && !approvedSpread) failures.push(`unforwarded CardImage at ${location}`);
              }
            }
          }
          if (ts.isReturnStatement(node) && node.expression && ts.isObjectLiteralExpression(node.expression)
            && (file === "components/animation/ResolvedAnimationImage.tsx" && enclosing(node) === "visibleAnimationImageSnapshot"
              || file === "components/modal/SeparatePilesModal.tsx" && enclosing(node) === "objectImageProps")) {
            if (!ownerField(node.expression.properties)) failures.push(`dropped capture/projection printing at ${location}`);
          }
          if (file === "components/animation/RevealOverlay.tsx" && ts.isCallExpression(node)
            && ts.isPropertyAccessExpression(node.expression) && node.expression.expression.getText(source) === "cards"
            && node.expression.name.text === "push" && node.arguments[0] && ts.isObjectLiteralExpression(node.arguments[0])
            && !ownerField(node.arguments[0].properties)) failures.push(`dropped reveal row printing at ${location}`);
          ts.forEachChild(node, visit);
        }
        visit(source);
      }
    }
    scan(src);
    expect(failures, failures.join("\n")).toEqual([]);
    expect(hooks).toEqual({ ...objectHooks, ...excludedHooks });
    expect(Object.values(hooks).reduce((sum, count) => sum + count, 0)).toBe(28);
    expect(Object.values(objectHooks).reduce((sum, count) => sum + count, 0)).toBe(15);
    expect(images).toEqual(expectedImages);
    expect(Object.values(images).reduce((sum, count) => sum + count, 0)).toBe(47);
    expect(helpers).toEqual(expectedHelpers);
    expect(Object.values(helpers).reduce((sum, count) => sum + count, 0)).toBe(35);
    expect(Object.keys(helpers)).toHaveLength(11);
    expect(exclusions).toEqual({ "components/modal/BoosterPackModal.tsx": 1, "pages/GamePage.tsx": 1 });
    expect(intermediates).toEqual({ HandCard: 1, ZoneFanCard: 2, DrawerCard: 1, CardTextboxPreview: 3 });
  }, V15_CENSUS_TIMEOUT_MS);
});
