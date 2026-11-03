import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { loadScryfallData, type PrintingEntry } from "../../../services/scryfall.ts";
import type * as Platform from "../../../services/platform.ts";
import type { ResolvedAsset } from "../../../services/visualPacks/types.ts";
import { assetKey, catalogRoot, installedRevision, packId } from "../../../services/visualPacks/types.ts";
import { encodeCandidateKey } from "../../../services/visualPacks/candidateKeys.ts";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers, buildPriorityWaitingFor } from "../../../test/factories/gameStateFactory.ts";
import { BattlefieldPeekPopover } from "../BattlefieldPeekPopover.tsx";

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
vi.mock("../../../hooks/useEngineCardData.ts", () => ({ useEngineCardData: () => null }));

const ORACLE = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const A = "11111111-1111-4111-8111-111111111111";
const B = "22222222-2222-4222-8222-222222222222";
const V = "33333333-3333-4333-8333-333333333333";
let admitCatalog: () => void;

beforeAll(async () => {
  const catalog: Record<string, PrintingEntry[]> = { [ORACLE]: [
    [A, "own", "1"], [B, "own", "2"], [V, "pin", "3"],
  ].map(([id, set, collector_number]) => ({
    id, set, collector_number, set_name: set, released_at: "2026-01-01", border_color: "black",
    frame_effects: [], full_art: false, faces: [{ normal: `/${id}.jpg` }],
  })) };
  const entry = { oracle_id: ORACLE, name: "Alpha", face_names: ["alpha"], layout: "normal",
    mana_cost: "{1}", cmc: 1, type_line: "Creature",
    faces: [{ normal: "/canonical.jpg" }], colors: [], color_identity: [], keywords: [] };
  const main = Object.fromEntries([ORACLE, "alpha", "elf warrior", "elvish mystic", "pest"]
    .map((name) => [name, entry]));
  const gate = new Promise<Response>((resolve) => {
    admitCatalog = () => resolve(new Response(JSON.stringify(catalog)));
  });
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) =>
    new URL(String(input), "http://localhost").pathname === "/scryfall-printings.json"
      ? gate : new Response(JSON.stringify(main))));
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

beforeEach(() => {
  usePreferencesStore.setState({ ...usePreferencesStore.getInitialState(), language: "en", showOwnerPrintings: true,
    artChain: [], artOverrides: { [ORACLE]: { scryfallId: V, setCode: "pin", collectorNumber: "3" } } });
});

function makeObject(id: number, name: string, power = 1, toughness = 1): GameObject {
  return buildGameObjectWithCoreTypes(["Creature"], {
    id,
    card_id: id,
    owner: 1,
    controller: 1,
    zone: "Battlefield",
    name,
    power,
    toughness,
    color: ["Green"],
    base_power: power,
    base_toughness: toughness,
    base_color: ["Green"],
    timestamp: id,
    entered_battlefield_turn: null,
  });
}

function setState(objects: GameObject[], unboundedPile: number[] = []) {
  useGameStore.setState({
    gameState: buildGameState({
      players: buildPlayers([1]),
      objects: buildObjectMap(...objects),
      battlefield: objects.map((object) => object.id),
      exile: [],
      stack: [],
      waiting_for: buildPriorityWaitingFor(),
      // CR 732.2a: engine-authored ∞-pile membership, mirrored from
      // `DerivedViews::unbounded_pile`. Only present when the loop is active.
      derived: unboundedPile.length > 0 ? { unbounded_pile: unboundedPile } : undefined,
    }),
  });
}

describe("BattlefieldPeekPopover", () => {
  afterEach(() => {
    cleanup();
    useGameStore.setState({ gameState: null });
    usePreferencesStore.setState(usePreferencesStore.getInitialState());
  });

  it("groups identical battlefield objects behind one representative", async () => {
    setState([
      makeObject(1, "Elf Warrior", 2, 2),
      makeObject(2, "Elf Warrior", 2, 2),
      makeObject(3, "Elvish Mystic", 1, 1),
    ]);
    const { container } = render(
      <BattlefieldPeekPopover
        playerId={1}
        opponentName="Lathril"
        seatColor="#a78bfa"
        isTargeting={false}
        legalTargetIds={[]}
      />,
    );

    await waitFor(() => expect(container.querySelectorAll('img[alt="Elf Warrior"]')).toHaveLength(1));
    expect(container.querySelectorAll("img")).toHaveLength(2);
    expect(screen.getByText("×2")).toBeInTheDocument();
  });

  // CR 732.2a: an accepted object-growth loop's members render `∞`, not `×N`.
  // Discriminating: the pile group (Elf Warrior) and the non-pile group
  // (Elvish Mystic) have the SAME count (2). Only pile membership decides the
  // badge, so removing the `unboundedPileIds` thread in the popover flips the
  // ∞ badge to a second `×2` and the `getByText("∞")` assertion throws.
  it("renders ∞ for unbounded-pile members while non-pile groups keep ×N", () => {
    setState(
      [
        makeObject(1, "Elf Warrior", 2, 2),
        makeObject(2, "Elf Warrior", 2, 2),
        makeObject(3, "Elvish Mystic", 1, 1),
        makeObject(4, "Elvish Mystic", 1, 1),
      ],
      [1, 2],
    );
    render(
      <BattlefieldPeekPopover
        playerId={1}
        opponentName="Lathril"
        seatColor="#a78bfa"
        isTargeting={false}
        legalTargetIds={[]}
      />,
    );

    // Pile group: ∞ instead of ×2.
    expect(screen.getByText("∞")).toBeInTheDocument();
    // Non-pile group of the same count still reads ×2 (proves ∞ is not blanket).
    expect(screen.getByText("×2")).toBeInTheDocument();
  });

  // CR 732.2a: pile membership is count-independent — a single visible member
  // still reads `∞` (main-board "singleton trap", GroupedPermanent.tsx). A plain
  // singleton renders no badge, so a bare `getByText("∞")` here is non-vacuous.
  it("renders ∞ for a single-member unbounded pile (count-independent)", () => {
    setState([makeObject(7, "Pest", 1, 1)], [7]);
    render(
      <BattlefieldPeekPopover
        playerId={1}
        opponentName="Lathril"
        seatColor="#a78bfa"
        isTargeting={false}
        legalTargetIds={[]}
      />,
    );

    expect(screen.getByText("∞")).toBeInTheDocument();
    expect(screen.queryByText("×1")).not.toBeInTheDocument();
  });

  it("regroups live A/A/B before admission, then renders admitted art through on/off/on", async () => {
    setState([1, 2, 3].map((id) => ({
      ...makeObject(id, "Alpha"), owner: 0,
      printed_ref: { oracle_id: ORACLE, face_name: "Alpha" },
      printing: { set_code: "own", collector_number: id === 3 ? "2" : "1" },
    })));
    const state = useGameStore.getState().gameState;
    const { container } = render(<BattlefieldPeekPopover playerId={1} opponentName="Other"
      seatColor="#a78bfa" isTargeting legalTargetIds={[3]} />);
    expect(screen.getByText("×2")).toBeInTheDocument();
    expect(screen.queryByText("×3")).toBeNull();
    // Admission is held: image fallback cannot decide whether these members group.
    await waitFor(() => expect(container.querySelectorAll(`img[src="/${V}.jpg"]`)).toHaveLength(2));
    const firstFrame = container.querySelector(".grid")!.firstElementChild!;
    expect(firstFrame.querySelector(".ring-cyan-300\\/80")).toBeTruthy();
    admitCatalog();
    await waitFor(() => {
      expect(container.querySelectorAll(`img[src="/${A}.jpg"]`)).toHaveLength(1);
      expect(container.querySelectorAll(`img[src="/${B}.jpg"]`)).toHaveLength(1);
    });
    act(() => usePreferencesStore.getState().setShowOwnerPrintings(false));
    expect(screen.getByText("×3")).toBeInTheDocument();
    await waitFor(() => expect(container.querySelectorAll(`img[src="/${V}.jpg"]`)).toHaveLength(1));
    act(() => usePreferencesStore.getState().setShowOwnerPrintings(true));
    expect(screen.getByText("×2")).toBeInTheDocument();
    await waitFor(() => expect(container.querySelector(`img[src="/${B}.jpg"]`)).not.toBeNull());
    expect(useGameStore.getState().gameState).toBe(state);
  });

  it.each([false, true])("keeps absent twins together, concealed members solo and legal B ahead of the cap (%s)", async (enabled) => {
    usePreferencesStore.setState({ showOwnerPrintings: enabled });
    const objects = [
      makeObject(1, "Alpha"), { ...makeObject(2, "Alpha"), printing: null },
      { ...makeObject(3, "Hidden"), face_down: true },
      { ...makeObject(4, "Hidden"), face_down: true },
      ...Array.from({ length: 12 }, (_, i) => makeObject(10 + i, `Other ${i}`)),
      { ...makeObject(99, "Alpha"), printing: { set_code: "own", collector_number: "2" },
        printed_ref: { oracle_id: ORACLE, face_name: "Alpha" } },
    ];
    setState(objects);
    const props = { playerId: 1, opponentName: "Other", seatColor: "#a78bfa", isTargeting: true, legalTargetIds: [99] };
    const { container, rerender } = render(<BattlefieldPeekPopover {...props} />);
    await waitFor(() => expect(container.querySelector('img[alt="Alpha"]')).not.toBeNull());
    expect(screen.getByText(enabled ? "×2" : "×3")).toBeInTheDocument();
    expect(container.querySelector(".grid")!.children).toHaveLength(12);
    expect(container.firstElementChild!.lastElementChild).toHaveTextContent(enabled ? /\+4\b/ : /\+3\b/);
    expect(container.querySelectorAll('[aria-label="Hidden"]')).toHaveLength(2);
    expect(container.querySelector(".grid")!.firstElementChild!.querySelector(".ring-cyan-300\\/80")).toBeTruthy();
    rerender(<BattlefieldPeekPopover {...props} isTargeting={false} />);
    expect(container.querySelector(".grid")!.children).toHaveLength(12);
    expect(container.querySelectorAll(".opacity-60")).toHaveLength(0);
  });
});
