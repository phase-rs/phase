import type { ReactNode } from "react";
import type * as Motion from "framer-motion";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

import type { CardPrinting, GameState, StackEntryDisplay } from "../../../adapter/types.ts";
import { StackDisplay } from "../StackDisplay.tsx";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildStackEntry,
  targetSelectionWaitingForFactory,
} from "../../../test/factories/gameStateFactory.ts";
import { loadScryfallData, type PrintingEntry } from "../../../services/scryfall.ts";
import type * as Platform from "../../../services/platform.ts";
import type { ResolvedAsset } from "../../../services/visualPacks/types.ts";
import { assetKey, catalogRoot, installedRevision, packId } from "../../../services/visualPacks/types.ts";
import { encodeCandidateKey } from "../../../services/visualPacks/candidateKeys.ts";

vi.mock("../../../game/dispatch.ts", () => ({ dispatchAction: vi.fn() }));
vi.mock("../../../hooks/useLocalizedCardContent.ts", () => ({ useLocalizedCardContent: () => null }));
vi.mock("../../../hooks/useEngineCardData.ts", () => ({ useEngineCardData: () => null }));
// Only exit-animation retention is removed: StackEntry, its image facade and
// StackTargetArcs remain real. Regrouping assertions observe the current surface.
vi.mock("framer-motion", async (original) => ({
  ...await original<typeof Motion>(),
  AnimatePresence: ({ children }: { children: ReactNode }) => <>{children}</>,
}));
vi.mock("../../flexlayout/DraggableWidget.tsx", () => ({
  DraggableWidget: ({ children }: { children: ReactNode }) => <div>{children}</div>,
}));

const installed = vi.hoisted(() => new Map<string, ResolvedAsset[]>());
vi.mock("../../../services/platform.ts", async (original) => ({
  ...await original<typeof Platform>(),
  loadVisualPackBackend: async () => ({
    resolve: async (keys: { key: string }[]) => ({
      revision: installedRevision("0"),
      entries: keys.map((key, ordinal) => ({ key, ordinal, matches: installed.get(key.key) ?? [] })),
    }),
    subscribeRevision: async () => () => {},
  }),
}));
const ORACLE = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OTHER_ORACLE = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const A = "11111111-1111-4111-8111-111111111111";
const B = "22222222-2222-4222-8222-222222222222";
const V = "33333333-3333-4333-8333-333333333333";
const printingA: CardPrinting = { set_code: "own", collector_number: "1" };
const printingB: CardPrinting = { set_code: "own", collector_number: "2" };
let admitCatalog: () => void;

beforeAll(async () => {
  const entry = { oracle_id: ORACLE, name: "Alpha", face_names: ["alpha"], layout: "normal",
    mana_cost: "{1}", cmc: 1, type_line: "Creature", colors: [], color_identity: [], keywords: [],
    faces: [{ normal: "/canonical.jpg" }] };
  const catalog: Record<string, PrintingEntry[]> = { [ORACLE]: [
    [A, "own", "1"], [B, "own", "2"], [V, "pin", "3"],
  ].map(([id, set, collector_number]) => ({
    id, set, collector_number, set_name: set, released_at: "2026-01-01", border_color: "black",
    frame_effects: [], full_art: false, faces: [{ normal: `/${id}.jpg` }],
  })) };
  catalog[OTHER_ORACLE] = [{ ...catalog[ORACLE][0], id: B, faces: [{ normal: `/${B}.jpg` }] }];
  const gate = new Promise<Response>((resolve) => {
    admitCatalog = () => resolve(new Response(JSON.stringify(catalog)));
  });
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) =>
    new URL(String(input), "http://localhost").pathname === "/scryfall-printings.json"
      ? gate : new Response(JSON.stringify({ [ORACLE]: entry, alpha: entry,
        [OTHER_ORACLE]: { ...entry, oracle_id: OTHER_ORACLE } }))));
  for (const id of [A, B, V]) {
    for (const rung of ["small", "normal"] as const) {
      installed.set(encodeCandidateKey("english_printing", [id, 0, "full_card", rung]), [{
        packId: packId("complete"), assetKey: assetKey("asset:v1:exact_printing:QQ"),
        catalogRoot: catalogRoot("a".repeat(64)), url: `/${id}.jpg`, media: "image/jpeg",
      }]);
    }
  }
  await loadScryfallData();
});

interface StackFixture {
  printings?: Array<CardPrinting | null | undefined>;
  livePrintings?: Array<CardPrinting | null | undefined>;
  missingRows?: number[];
  legalIds?: number[];
  grouped?: boolean;
  departed?: boolean;
  holdCatalog?: boolean;
  sourceKind?: "spell" | "emblem";
}

function seed({
  printings = [printingA, { ...printingA }, printingB],
  livePrintings = [printingB, printingA, printingA],
  missingRows = [], legalIds = [], grouped = true, departed = false, holdCatalog = false,
  sourceKind = "spell",
}: StackFixture = {}): GameState {
  if (!holdCatalog) admitCatalog();
  const ids = printings.map((_, index) => 10 + index);
  const details: Record<string, StackEntryDisplay> = {};
  for (const [index, id] of ids.entries()) {
    if (!missingRows.includes(id)) {
      details[String(id)] = { source_name: sourceKind === "emblem" ? "Emblem" : "Alpha",
        kind_label: sourceKind === "emblem" ? "Triggered Ability" : "Spell", printing: printings[index],
        printed_ref: { oracle_id: ORACLE, face_name: "Alpha" } };
    }
  }
  const waitingFor = legalIds.length > 0
    ? targetSelectionWaitingForFactory.withData({
      selection: { current_slot: 0, current_legal_targets: legalIds.map((id) => ({ Object: id })) },
    }).build()
    : undefined;
  const state = buildGameState({
    stack: ids.map((id) => buildStackEntry({ id, source_id: id + 1000, controller: 0,
      ...(sourceKind === "emblem" ? { kind: { type: "TriggeredAbility" as const,
        data: { source_id: id + 1000, source_name: "Emblem", ability: { targets: [] } } } } : {}),
    })),
    objects: departed ? {} : buildObjectMap(...ids.map((id, index) => buildGameObject({
      id: id + 1000, owner: 1, controller: 0, name: "Alpha", zone: "Stack",
      printed_ref: { oracle_id: ORACLE, face_name: "Alpha" }, printing: livePrintings[index],
      display_visible_to_viewer: true,
      ...(sourceKind === "emblem" ? { name: "Emblem", zone: "Command" as const, is_emblem: true,
        printed_ref: null, emblem_source: { name: "Alpha",
          printed_ref: { oracle_id: ORACLE, face_name: "Alpha" }, printing: printings[index] } } : {}),
    }))),
    derived: {
      stack_display_groups: grouped ? [{ representative: 10, count: ids.length, member_ids: ids }] : undefined,
      stack_entry_details: details,
    },
    ...(waitingFor ? { waiting_for: waitingFor } : {}),
  });
  useGameStore.setState({ gameMode: "local", gameState: state, waitingFor: state.waiting_for });
  return state;
}

function renderedIds() {
  return Array.from(document.querySelectorAll("[data-stack-entry]"),
    (element) => Number(element.getAttribute("data-stack-entry")));
}

function row(id: number) {
  const element = document.querySelector<HTMLElement>(`[data-stack-entry="${id}"]`);
  expect(element, `stack row ${id}`).not.toBeNull();
  return element!;
}

let nextFrame = 0;
const frames = new Map<number, FrameRequestCallback>();
function frame() {
  act(() => {
    const pending = [...frames.values()];
    frames.clear();
    for (const callback of pending) callback(performance.now());
  });
}

describe("StackDisplay real grouped surfaces", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    useUiStore.setState(useUiStore.getInitialState());
    useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false });
    usePreferencesStore.setState({ ...usePreferencesStore.getInitialState(), language: "en",
      showOwnerPrintings: true, artChain: [],
      artOverrides: { [ORACLE]: { scryfallId: V, setCode: "pin", collectorNumber: "3" } } });
    vi.mocked(dispatchAction).mockReset();
    vi.mocked(dispatchAction).mockResolvedValue(undefined);
    frames.clear();
    vi.spyOn(globalThis, "requestAnimationFrame").mockImplementation((callback: FrameRequestCallback) => {
      frames.set(++nextFrame, callback);
      return nextFrame;
    });
    vi.spyOn(globalThis, "cancelAnimationFrame").mockImplementation((id: number) => { frames.delete(id); });
  });

  afterEach(() => {
    cleanup();
    usePreferencesStore.setState(usePreferencesStore.getInitialState());
    useUiStore.setState(useUiStore.getInitialState());
    vi.restoreAllMocks();
  });

  it("splits A/A/B before admission, preserves member order and cycles live without replacing state", async () => {
    const state = seed({ holdCatalog: true });
    const { container } = render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);
    expect(renderedIds()).toEqual([10, 11, 12]);
    expect(screen.queryByText("×3")).toBeNull();
    expect(screen.getByText("Stack").parentElement).toHaveTextContent("3");
    await waitFor(() => expect(container.querySelectorAll(`img[src="/${V}.jpg"]`)).toHaveLength(3));
    admitCatalog();
    await waitFor(() => {
      expect(row(10).querySelector("img")).toHaveAttribute("src", `/${A}.jpg`);
      expect(row(11).querySelector("img")).toHaveAttribute("src", `/${A}.jpg`);
      expect(row(12).querySelector("img")).toHaveAttribute("src", `/${B}.jpg`);
    });
    act(() => usePreferencesStore.getState().setShowOwnerPrintings(false));
    expect(renderedIds()).toEqual([10]);
    expect(row(10)).toHaveAttribute("data-grouped-ids", "10 11 12");
    expect(screen.getByText("×3")).toBeInTheDocument();
    await waitFor(() => expect(row(10).querySelector("img")).toHaveAttribute("src", `/${V}.jpg`));
    act(() => usePreferencesStore.getState().setShowOwnerPrintings(true));
    expect(renderedIds()).toEqual([10, 11, 12]);
    await waitFor(() => expect(row(12).querySelector("img")).toHaveAttribute("src", `/${B}.jpg`));
    expect(useGameStore.getState().gameState).toBe(state);
  });

  it("C2 splits emblem triggers by captured A/A/B and cycles the unchanged engine group", async () => {
    const state = seed({ sourceKind: "emblem" });
    render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);
    expect(renderedIds()).toEqual([10, 11, 12]);
    await waitFor(() => {
      expect(row(10).querySelector("img")).toHaveAttribute("src", `/${A}.jpg`);
      expect(row(11).querySelector("img")).toHaveAttribute("src", `/${A}.jpg`);
      expect(row(12).querySelector("img")).toHaveAttribute("src", `/${B}.jpg`);
    });
    act(() => usePreferencesStore.getState().setShowOwnerPrintings(false));
    expect(renderedIds()).toEqual([10]);
    expect(screen.getByText("×3")).toBeInTheDocument();
    expect(row(10)).toHaveAttribute("data-grouped-ids", "10 11 12");
    await waitFor(() => expect(row(10).querySelector("img")).toHaveAttribute("src", `/${V}.jpg`));
    act(() => usePreferencesStore.getState().setShowOwnerPrintings(true));
    expect(renderedIds()).toEqual([10, 11, 12]);
    await waitFor(() => {
      expect(row(10).querySelector("img")).toHaveAttribute("src", `/${A}.jpg`);
      expect(row(11).querySelector("img")).toHaveAttribute("src", `/${A}.jpg`);
      expect(row(12).querySelector("img")).toHaveAttribute("src", `/${B}.jpg`);
    });
    expect(useGameStore.getState().gameState).toBe(state);
  });

  it.each([
    ["equal captured pairs", [printingA, { ...printingA }], [10]],
    ["absent captured pairs", [null, undefined], [10]],
    ["captured versus absent", [printingA, undefined], [10, 11]],
  ] as const)("C2 preserves emblem grouping for %s", async (_name, pairs, expected) => {
    seed({ sourceKind: "emblem", printings: [...pairs] });
    render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);
    expect(renderedIds()).toEqual(expected);
    await waitFor(() => expect(row(10).querySelector("img")).toHaveAttribute("src",
      pairs[0] ? `/${A}.jpg` : `/${V}.jpg`));
    act(() => usePreferencesStore.getState().setShowOwnerPrintings(false));
    expect(renderedIds()).toEqual([10]);
    await waitFor(() => expect(row(10).querySelector("img")).toHaveAttribute("src", `/${V}.jpg`));
  });

  it.each([
    ["equal allocated pairs", printingA, { collector_number: "1", set_code: "own" }, false],
    ["collector differs", printingA, printingB, true],
    ["set differs", printingA, { set_code: "other", collector_number: "1" }, true],
    ["present versus absent", printingA, null, true],
    ["null versus omitted", null, undefined, false],
    ["raw set case", printingA, { set_code: "OWN", collector_number: "1" }, true],
    ["raw collector whitespace", printingA, { set_code: "own", collector_number: "1 " }, true],
    ["raw leading zero", printingA, { set_code: "own", collector_number: "01" }, true],
    ["empty present pair", { set_code: "", collector_number: "" }, undefined, true],
    ["delimiter pairs", { set_code: "a|b", collector_number: "c" }, { set_code: "a", collector_number: "b|c" }, true],
  ] as const)("uses delivered details values: %s", (_name, first, second, split) => {
    seed({ printings: [first, second] });
    render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);
    expect(renderedIds()).toEqual(split ? [10, 11] : [10]);
    act(() => usePreferencesStore.getState().setShowOwnerPrintings(false));
    expect(renderedIds()).toEqual([10]);
    expect(screen.getByText("×2")).toBeInTheDocument();
  });

  it.each(["different", "missing", "equal", "legacy"] as const)(
    "groups by effective row owner authority: %s, independent of representative order", async (authority) => {
      for (const reverse of [false, true]) {
        const state = seed({ printings: [printingA, printingA] });
        usePreferencesStore.setState({ showOwnerPrintings: true, artOverrides: {
          [ORACLE]: { scryfallId: V, setCode: "pin", collectorNumber: "3" },
          [OTHER_ORACLE]: { scryfallId: V, setCode: "pin", collectorNumber: "3" },
        } });
        const details = state.derived!.stack_entry_details!;
        if (authority === "different") details["11"].printed_ref = { oracle_id: OTHER_ORACLE, face_name: "Alpha" };
        if (authority === "missing" || authority === "legacy") details["11"].printed_ref = undefined;
        if (authority === "legacy") details["10"].printed_ref = undefined;
        state.objects[1011].printed_ref = { oracle_id: OTHER_ORACLE, face_name: "Alpha" };
        if (reverse) {
          state.stack.reverse();
          state.derived!.stack_display_groups = [{ representative: 11, member_ids: [11, 10], count: 2 }];
        }
        // Observe both real images before collapsing the engine-authored display group.
        const groups = state.derived!.stack_display_groups;
        act(() => useGameStore.setState({ gameState: { ...state, derived: { ...state.derived, stack_display_groups: [] } } }));
        const surface = render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);
        for (const id of [10, 11]) {
          const expected = authority === "legacy" || (authority === "missing" && id === 11)
            ? V : authority === "different" && id === 11 ? B : A;
          await waitFor(() => expect(row(id).querySelector("img")).toHaveAttribute("src", `/${expected}.jpg`));
        }
        act(() => useGameStore.setState({ gameState: { ...state, derived: { ...state.derived, stack_display_groups: groups } } }));
        const split = authority === "different" || authority === "missing";
        expect(renderedIds().sort()).toEqual(split ? [10, 11] : [reverse ? 11 : 10]);
        act(() => usePreferencesStore.getState().setShowOwnerPrintings(false));
        expect(renderedIds()).toEqual([reverse ? 11 : 10]);
        await waitFor(() => expect(row(reverse ? 11 : 10).querySelector("img")).toHaveAttribute("src", `/${V}.jpg`));
        surface.unmount();
      }
    },
  );

  it.each([false, true])("preserves legal-member choice identity independently of printing (%s)", (enabled) => {
    usePreferencesStore.setState({ showOwnerPrintings: enabled });
    for (const heterogeneous of [false, true]) {
      for (const legalIds of [[], [11], [10, 11]]) {
        seed({ printings: [printingA, heterogeneous ? printingB : { ...printingA }], legalIds });
        render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);
        const split = legalIds.length === 2 || (enabled && heterogeneous);
        expect(renderedIds()).toEqual(split ? [10, 11] : [10]);
        vi.mocked(dispatchAction).mockClear();
        for (const id of renderedIds()) fireEvent.click(row(id));
        const selected = split ? legalIds : legalIds.slice(0, 1);
        expect(vi.mocked(dispatchAction).mock.calls.map(([action]) => action)).toEqual(
          selected.map((id) => ({ type: "ChooseTarget", data: { target: { Object: id } } })));
        if (legalIds.length === 0) expect(useUiStore.getState().inspectedObjectId).toBe(split ? 1011 : 1010);
        cleanup();
      }
    }
  });

  it("keeps present absent-printing rows authoritative over contradictory live objects", async () => {
    const state = seed({ printings: [null, undefined], livePrintings: [printingA, printingB] });
    render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);
    expect(renderedIds()).toEqual([10]);
    await waitFor(() => expect(row(10).querySelector("img")).toHaveAttribute("src", `/${V}.jpg`));
    act(() => useGameStore.setState({ gameState: {
      ...state, derived: { ...state.derived, stack_entry_details: {
        ...state.derived!.stack_entry_details,
        "11": { source_name: "Alpha", kind_label: "Spell", printing: printingA,
          printed_ref: { oracle_id: ORACLE, face_name: "Alpha" } },
      } },
    } }));
    expect(renderedIds()).toEqual([10, 11]);
    await waitFor(() => expect(row(11).querySelector("img")).toHaveAttribute("src", `/${A}.jpg`));
    expect(row(10).querySelector("img")).toHaveAttribute("src", `/${V}.jpg`);
  });

  it.each([
    ["one absent row, contrasting live", [printingA, printingA], [11], [printingB, printingB]],
    ["all absent rows, contrasting live", [printingA, printingA], [10, 11], [printingA, printingB]],
    ["all absent rows, identical live triples", [printingA, printingA, printingA], [10, 11, 12], [printingA, printingA, printingA]],
    ["one absent row, equal derived and live", [printingA, printingA], [11], [printingA, printingA]],
  ] as const)("splits missing comparison authority, not missing printing: %s", async (_name, printings, missingRows, livePrintings) => {
    const state = seed({ printings: [...printings], missingRows: [...missingRows], livePrintings: [...livePrintings] });
    const missing = new Set<number>(missingRows);
    render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);
    expect(renderedIds()).toEqual(printings.map((_, index) => 10 + index));
    for (const [index, id] of renderedIds().entries()) {
      const expected = missing.has(id) ? livePrintings[index] : printings[index];
      await waitFor(() => expect(row(id).querySelector("img")).toHaveAttribute("src", `/${expected.collector_number === "2" ? B : A}.jpg`));
    }
    act(() => usePreferencesStore.getState().setShowOwnerPrintings(false));
    expect(renderedIds()).toEqual([10]);
    expect(screen.getByText(`×${printings.length}`)).toBeInTheDocument();
    act(() => usePreferencesStore.getState().setShowOwnerPrintings(true));
    expect(renderedIds()).toEqual(printings.map((_, index) => 10 + index));
    expect(useGameStore.getState().gameState).toBe(state);
  });

  it("renders raw entries with their live fallback and derived entries after source departure", async () => {
    seed({ printings: [undefined, undefined], missingRows: [10, 11], grouped: false,
      livePrintings: [printingA, printingB] });
    render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);
    expect(renderedIds()).toEqual([10, 11]);
    await waitFor(() => {
      expect(row(10).querySelector("img")).toHaveAttribute("src", `/${A}.jpg`);
      expect(row(11).querySelector("img")).toHaveAttribute("src", `/${B}.jpg`);
    });
    cleanup();
    seed({ printings: [printingA, printingB], departed: true });
    render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);
    expect(renderedIds()).toEqual([10, 11]);
    await waitFor(() => expect(row(11).querySelector("img")).toHaveAttribute("src", `/${B}.jpg`));
    cleanup();
    useGameStore.setState({ gameState: buildGameState({ stack: [] }) });
    const { container } = render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);
    expect(container).toBeEmptyDOMElement();
  });

  it("splits distinct unadmitted values even when both images resolve to the viewer", async () => {
    seed({ printings: [{ set_code: "invalid", collector_number: "x" }, { set_code: "foreign", collector_number: "x" }] });
    const { container } = render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);
    expect(renderedIds()).toEqual([10, 11]);
    await waitFor(() => expect(container.querySelectorAll(`img[src="/${V}.jpg"]`)).toHaveLength(2));
  });

  it("uses the same split decision for real SVG endpoints and raises each hovered row", () => {
    const state = seed({ printings: [printingA, printingB] });
    state.stack.push(buildStackEntry({ id: 20, source_id: 1020, controller: 0 }));
    state.derived!.stack_display_groups!.push({ representative: 20, count: 1, member_ids: [20] });
    state.derived!.stack_entry_details!["10"].targets = [{ target: { Player: 1 }, label: "Opponent" }];
    state.derived!.stack_entry_details!["11"].targets = [{ target: { Player: 1 }, label: "Opponent" }];
    state.derived!.stack_entry_details!["20"] = {
      source_name: "Alpha", kind_label: "Spell", printing: printingA,
      printed_ref: { oracle_id: ORACLE, face_name: "Alpha" },
      targets: [{ target: { Object: 11 }, label: "Second stack member" }],
    };
    const originalRect = HTMLElement.prototype.getBoundingClientRect;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      const id = this.getAttribute("data-stack-entry");
      if (id === "10") return new DOMRect(100, 100, 80, 120);
      if (id === "11") return new DOMRect(400, 100, 80, 120);
      if (id === "20") return new DOMRect(800, 400, 80, 120);
      if (this.hasAttribute("data-player-hud")) return new DOMRect(1000, 500, 40, 40);
      return originalRect.call(this);
    });
    const hud = document.createElement("div");
    hud.dataset.playerHud = "1";
    document.body.appendChild(hud);
    try {
      render(<StackDisplay effectiveMultiplayerBoardLayout="focused" />);
      for (const enabled of [true, false, true]) {
        act(() => usePreferencesStore.getState().setShowOwnerPrintings(enabled));
        frame();
        expect(renderedIds()).toEqual(enabled ? [10, 11, 20] : [10, 20]);
        const targetArc = Array.from(document.querySelectorAll<SVGPathElement>('path[fill="none"]'))
          .find((path) => path.getAttribute("d")?.startsWith("M 840 460 "))!;
        expect(targetArc).toHaveAttribute("d", expect.stringMatching(enabled ? / 440 160$/ : / 140 160$/));
        for (const id of renderedIds()) {
          fireEvent.mouseEnter(row(id));
          frame();
          expect(row(id)).toHaveStyle({ zIndex: renderedIds().length + 1 });
          const rectX = id === 10 ? 140 : id === 11 ? 440 : 840;
          const active = Array.from(document.querySelectorAll<SVGPathElement>('path[fill="none"]'))
            .find((path) => path.getAttribute("d")?.startsWith(`M ${rectX} `))!;
          expect(active).toHaveAttribute("stroke", "#C9B037");
          expect(active).toHaveAttribute("opacity", "1");
          expect(document.querySelectorAll('path[fill="none"][stroke="#C9B037"]')).toHaveLength(1);
          fireEvent.mouseLeave(row(id));
        }
      }
    } finally {
      hud.remove();
    }
  });
});
