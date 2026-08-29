import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import { LimitedDeckBuilder } from "../LimitedDeckBuilder";
import { createDefaultDraftWorkspacePreferences } from "../workspace/workspacePreferences";

afterEach(cleanup);

vi.mock("../../../stores/draftStore", () => ({
  useDraftStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector({
      view: null,
      mainDeck: [],
      landCounts: {},
      addToDeck: () => {},
      removeFromDeck: () => {},
      setLandCount: () => {},
      autoSuggestDeck: async () => {},
      autoSuggestLands: async () => {},
      submitDeck: async () => {},
    }),
}));

// Exit animations would keep filtered-out pool tiles mounted past the
// assertion (#7507 rows); these tests are about which tiles the filter keeps,
// not how the others leave. Same idiom as NativeEngineProgressOverlay.test.
vi.mock("framer-motion", () => ({
  AnimatePresence: ({ children }: { children: React.ReactNode }) => <>{children}</>,
  motion: {
    div: ({
      children,
      layout: _layout,
      initial: _initial,
      animate: _animate,
      exit: _exit,
      transition: _transition,
      ...props
    }: {
      children?: React.ReactNode;
      layout?: unknown;
      initial?: unknown;
      animate?: unknown;
      exit?: unknown;
      transition?: unknown;
    } & Record<string, unknown>) => <div {...props}>{children}</div>,
  },
}));

// The engine (wasm) cannot load under vitest; stand in for its filtering
// authority with a contract-faithful fake. Presentation exports stay real.
let failFilterCalls = false;
let failOptionsCalls = false;
let deferredOptions:
  | {
      poolId: string;
      promise: Promise<{ types: string[]; colors: string[]; rarities: string[] }>;
    }
  | null = null;
vi.mock("../../../viewmodel/limitedPoolFilter", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("../../../viewmodel/limitedPoolFilter")>();
  return {
    ...actual,
    // Contract-faithful fake of the engine's stateless option path: classify
    // each instance from its own fields, exactly as draft-core does.
    fetchPoolFilterOptions: async (
      pool: Array<{ instance_id: string; colors: string[]; type_line: string; rarity: string }>,
    ) => {
      if (failOptionsCalls) throw new Error("engine unavailable");
      if (deferredOptions?.poolId === pool[0]?.instance_id) {
        return deferredOptions.promise;
      }
      const typeOrder = [
        "creature",
        "instant",
        "sorcery",
        "enchantment",
        "artifact",
        "planeswalker",
        "land",
      ];
      const types = typeOrder.filter((t) =>
        pool.some((c) => c.type_line.toLowerCase().includes(t)),
      );
      const colorOrder: Array<[string, string]> = [
        ["white", "W"],
        ["blue", "U"],
        ["black", "B"],
        ["red", "R"],
        ["green", "G"],
      ];
      const colors = colorOrder
        .filter(([, s]) => pool.some((c) => c.colors.includes(s)))
        .map(([kind]) => kind);
      if (pool.some((c) => c.colors.length >= 2)) colors.push("multicolor");
      if (pool.some((c) => c.colors.length === 0)) colors.push("colorless");
      const rarities = ["mythic", "rare", "uncommon", "common"].filter((r) =>
        pool.some((c) => c.rarity.toLowerCase() === r),
      );
      return { types, colors, rarities };
    },
    filterPoolListing: async (
      listing: Array<{ instance_id: string; name: string; type_line: string }>,
      filter: { query: string; types: string[] },
    ) => {
      if (failFilterCalls) throw new Error("engine unavailable");
      // Contract-faithful fake of the engine: classify each instance from
      // its own fields (the real authority does the same in draft-core).
      const q = filter.query.trim().toLowerCase();
      return listing
        .filter(
          (c) =>
            (q === "" || c.name.toLowerCase().includes(q)) &&
            (filter.types.length === 0 ||
              filter.types.some((t) => c.type_line.toLowerCase().includes(t))),
        )
        .map((c) => c.instance_id);
    },
  };
});

// CR 903.3 / CR 702.124: the ENGINE is the eligibility and pairing authority.
// It cannot load under vitest, so both published surfaces are replaced with
// per-test controllable fakes. Every other engineRuntime export stays real.
const engineEligible = vi.fn(async (_name: string, _format: string) => false);
const enginePartnerCandidates = vi.fn(
  async (_first: string, _candidates: string[], _draftSetCodes: readonly string[]) =>
    [] as string[],
);
vi.mock("../../../services/engineRuntime", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../services/engineRuntime")>();
  return {
    ...actual,
    isCardCommanderEligibleForFormat: (name: string, format: string) =>
      engineEligible(name, format),
    commanderPartnerCandidates: (
      first: string,
      candidates: string[],
      draftSetCodes: readonly string[],
    ) => enginePartnerCandidates(first, candidates, draftSetCodes),
  };
});

// The colour-identity pips are not what these rows assert, and the real hook
// would reach Scryfall. A stable empty cache keeps the render deterministic.
const EMPTY_CARD_DATA_CACHE = new Map();
vi.mock("../../../hooks/useDeckCardData", () => ({
  useDeckCardData: () => ({
    cardDataCache: EMPTY_CARD_DATA_CACHE,
    cacheCards: () => {},
  }),
}));

vi.mock("../../card/HoverCardPreview", () => ({
  HoverCardPreview: ({ card }: { card: { name: string } | null }) => (
    <div data-testid="hover-preview">{card?.name}</div>
  ),
}));

type BuilderView = NonNullable<NonNullable<Parameters<typeof LimitedDeckBuilder>[0]>["view"]>;

const TEST_VIEW: BuilderView = {
  status: "Deckbuilding",
  kind: "Quick",
  current_pack_number: 1,
  pick_number: 1,
  pass_direction: "Left",
  current_pack: null,
  required_pick_count: 0,
  pool: [
    {
      instance_id: "card-1",
      name: "Wind Drake",
      set_code: "dmu",
      collector_number: "58",
      rarity: "common",
      colors: ["U"],
      cmc: 3,
      type_line: "Creature - Drake",
    },
  ],
  draft_effects: [],
  pool_groups: {
    color_groups: [],
    type_groups: [],
    cmc_groups: [],
    rarity_groups: [],
    type_filter_options: [],
    color_filter_options: [],
    color_counts: { white: 0, blue: 1, black: 0, red: 0, green: 0 },
    workspace_capabilities: { rarity_group_order: [] },
    workspace_row_classification: { creature_instance_ids: [], noncreature_instance_ids: [] },
  },
  seats: [],
  cards_per_pack: 14,
  pack_sizes: [14, 14, 14],
  pack_set_codes: ["TST", "TST", "TST"],
  pack_pick_steps: [14, 14, 14],
  pick_steps_per_pack: 14,
  pack_count: 3,
  min_deck_size: 40,
  addable_cards: ["Plains", "Island", "Academy Ruins"],
  timer_remaining_ms: null,
  standings: [],
  current_round: 0,
  next_pairing_round: 1,
  tournament_format: "Swiss",
  pod_policy: "Competitive",
  pairings: [],
  match_config: { match_type: "Bo1" },
};

function Harness() {
  const [mainDeck, setMainDeck] = useState<string[]>([]);

  return (
    <LimitedDeckBuilder
      view={TEST_VIEW}
      mainDeck={mainDeck}
      landCounts={{}}
      onAddToDeck={(cardName) => setMainDeck((prev) => [...prev, cardName])}
      onRemoveFromDeck={(cardName) =>
        setMainDeck((prev) => {
          const idx = prev.indexOf(cardName);
          if (idx < 0) return prev;
          const next = prev.slice();
          next.splice(idx, 1);
          return next;
        })
      }
      onSetLandCount={() => {}}
      onSubmitDeck={() => {}}
      showSuggestions={false}
    />
  );
}

describe("LimitedDeckBuilder", () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("updates mana curve when a card is added from pool", () => {
    render(<Harness />);

    const threeDropBucket = screen.getByRole("meter", { name: "Mana value 3" });
    expect(threeDropBucket).toHaveAttribute("aria-valuenow", "0");

    fireEvent.click(screen.getByRole("button", { name: /wind drake/i }));

    expect(threeDropBucket).toHaveAttribute("aria-valuenow", "1");
  });

  it("filters custom addable cards by name", () => {
    render(<Harness />);

    fireEvent.change(screen.getByPlaceholderText("Search addable cards..."), {
      target: { value: "academy" },
    });

    expect(screen.getByText("Academy Ruins")).toBeInTheDocument();
    expect(screen.queryByText("Plains")).not.toBeInTheDocument();
    expect(screen.queryByText("Island")).not.toBeInTheDocument();
  });

  it("does not substitute basic lands when the engine exposes no addable cards", () => {
    render(
      <LimitedDeckBuilder
        view={{ ...TEST_VIEW, addable_cards: [] }}
        mainDeck={[]}
        landCounts={{}}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    expect(screen.queryByRole("button", { name: "Add Plains" })).not.toBeInTheDocument();
  });

  it("adds_basic_lands_through_the_phone_compact_lands_picker", () => {
    const onAddBasicLand = vi.fn();
    const onAutoSuggestLands = vi.fn();
    render(
      <LimitedDeckBuilder
        local={{
          view: TEST_VIEW,
          workspace: {
            schemaVersion: 1,
            placements: { "card-1": { zone: "deck", row: 0, column: 0, order: 0 } },
            virtualBasics: [],
          },
          preferences: createDefaultDraftWorkspacePreferences(),
          interactionLocked: false,
          onWorkspaceChange: () => {},
          onPreferencesChange: () => {},
          onSubmitDeck: () => {},
          onAddBasicLand,
          onRemoveBasicLand: () => {},
          onAutoSuggestLands,
        }}
        responsiveLayout="phone-portrait"
        showSuggestions
      />,
    );

    const addLands = screen.getByRole("button", { name: "Add Lands" });
    fireEvent.click(addLands);
    expect(addLands).toHaveClass("min-h-11");
    const picker = screen.getByRole("dialog", { name: "Add Lands" });
    expect(within(picker).getByRole("button", { name: "Auto Lands" })).toHaveClass("min-h-11");
    expect(screen.queryAllByRole("button", { name: "Auto Lands" })).toHaveLength(1);
    fireEvent.click(within(picker).getByRole("button", { name: "Auto Lands" }));
    expect(onAutoSuggestLands).toHaveBeenCalledOnce();
    fireEvent.click(within(picker).getByRole("button", { name: "Add Plains" }));
    fireEvent.click(within(picker).getByRole("button", { name: "Add Lands" }));
    expect(onAddBasicLand).toHaveBeenCalledWith("Plains");
  });

  it.each(["tablet-portrait", "tablet-landscape"] as const)(
    "uses Add lands in %s compact builder",
    (responsiveLayout) => {
      const onAutoSuggestLands = vi.fn();
      render(
        <LimitedDeckBuilder
          local={{
            view: TEST_VIEW,
            workspace: {
              schemaVersion: 1,
              placements: { "card-1": { zone: "deck", row: 0, column: 0, order: 0 } },
              virtualBasics: [],
            },
            preferences: { ...createDefaultDraftWorkspacePreferences(), explicitView: "compact" },
            interactionLocked: false,
            onWorkspaceChange: () => {},
            onPreferencesChange: () => {},
            onSubmitDeck: () => {},
            onAddBasicLand: () => {},
            onRemoveBasicLand: () => {},
            onAutoSuggestLands,
          }}
          responsiveLayout={responsiveLayout}
        />,
      );

      const addLands = screen.getByRole("button", { name: "Add Lands" });
      expect(addLands).toHaveClass("min-h-11");
      expect(screen.queryByRole("button", { name: "Lands" })).not.toBeInTheDocument();
      fireEvent.click(addLands);
      const picker = screen.getByRole("dialog", { name: "Add Lands" });
      const autoLands = within(picker).getByRole("button", { name: "Auto Lands" });
      expect(autoLands).toHaveClass("min-h-11");
      fireEvent.click(autoLands);
      expect(onAutoSuggestLands).toHaveBeenCalledOnce();
    },
  );

  it("keeps the tablet portrait generic summary and actions docked while leaving statistics tables on desktop", async () => {
      const suggestDeck = vi.fn();
      const rejectSubmission = vi.fn(async () => {
        throw new Error("submission rejected");
      });
      const land = {
        instance_id: "land-1",
        name: "Island",
        set_code: "dmu",
        collector_number: "259",
        rarity: "common" as const,
        colors: [],
        cmc: 0,
        type_line: "Basic Land - Island",
      };
      const view = {
        ...TEST_VIEW,
        min_deck_size: 1,
        pool: [...TEST_VIEW.pool, land],
      };
      const { container } = render(
        <LimitedDeckBuilder
          local={{
            view,
            workspace: {
              schemaVersion: 1,
              placements: {
                "card-1": { zone: "deck", row: 0, column: 0, order: 0 },
                "land-1": { zone: "deck", row: 0, column: 1, order: 0 },
              },
              virtualBasics: [],
            },
            preferences: { ...createDefaultDraftWorkspacePreferences(), explicitView: "compact" },
            interactionLocked: false,
            onWorkspaceChange: () => {},
            onPreferencesChange: () => {},
            onSubmitDeck: rejectSubmission,
            onAddBasicLand: () => {},
            onRemoveBasicLand: () => {},
            onAutoSuggestDeck: suggestDeck,
          }}
          responsiveLayout="tablet-portrait"
          showSuggestions={false}
        />,
      );

      const dock = container.querySelector<HTMLElement>("[data-tablet-builder-dock]")!;
      expect(container.querySelector("[data-responsive-builder-layout='tablet-portrait']"))
        .toHaveClass("h-[calc(100dvh_-_4rem)]");
      expect(container.querySelector("[data-tablet-builder-board]")).toHaveClass("flex-1");
      expect(dock).toHaveClass("shrink-0");
      expect(container.querySelector("[data-tablet-builder-summary]")).toHaveClass("grid-cols-4");
      expect(within(dock).getByText("Mana Curve").closest("section")).toHaveClass("col-span-3");
      expect(within(dock).getByText("Average Mana Cost").closest("section")).toHaveClass("col-span-1");
      expect(within(dock).getByText("3.00")).toBeInTheDocument();
      expect(container.querySelectorAll("table")).toHaveLength(0);

      const suggest = within(dock).getByRole("button", { name: "Suggest Deck" });
      expect(suggest).toBeDisabled();
      fireEvent.click(suggest);
      expect(suggestDeck).not.toHaveBeenCalled();

      const actions = container.querySelector<HTMLElement>("[data-tablet-builder-actions]")!;
      fireEvent.click(within(actions).getByRole("button", { name: "Submit Deck" }));
      expect(await screen.findByRole("alert")).toHaveTextContent("submission rejected");
      expect(container.querySelector("[data-tablet-builder-actions]")).toBe(actions);
      expect(container.querySelector("[data-tablet-landscape-builder-row]")).not.toBeInTheDocument();
    });

  it("uses container height unchanged for embedded tablet builders", () => {
    const { container } = render(
      <LimitedDeckBuilder
        local={{
          view: TEST_VIEW,
          workspace: { schemaVersion: 1, placements: {}, virtualBasics: [] },
          preferences: createDefaultDraftWorkspacePreferences(),
          interactionLocked: false,
          onWorkspaceChange: () => {},
          onPreferencesChange: () => {},
          onSubmitDeck: () => {},
          onAddBasicLand: () => {},
          onRemoveBasicLand: () => {},
        }}
        responsiveLayout="tablet-landscape"
        responsiveHeightMode="container"
      />,
    );

    expect(container.querySelector("[data-responsive-builder-layout='tablet-landscape']"))
      .toHaveClass("h-full");
    expect(container.querySelector("[data-responsive-builder-layout='tablet-landscape']"))
      .not.toHaveClass("h-[calc(100dvh_-_4rem)]");
  });

  it("uses one ordered four-cell dock row for the tablet landscape compact and visual builders", async () => {
    const suggestDeck = vi.fn();
    const rejectSubmission = vi.fn(async () => {
      throw new Error("submission rejected");
    });
    const preferences = { ...createDefaultDraftWorkspacePreferences(), explicitView: "compact" as const };
    const onPreferencesChange = vi.fn();
    const local = {
      view: { ...TEST_VIEW, min_deck_size: 1 },
      workspace: {
        schemaVersion: 1 as const,
        placements: { "card-1": { zone: "deck" as const, row: 0, column: 0, order: 0 } },
        virtualBasics: [],
      },
      preferences,
      interactionLocked: false,
      onWorkspaceChange: () => {},
      onPreferencesChange,
      onSubmitDeck: rejectSubmission,
      onAddBasicLand: () => {},
      onRemoveBasicLand: () => {},
      onAutoSuggestDeck: suggestDeck,
    };
    const { container, rerender } = render(
      <LimitedDeckBuilder local={local} responsiveLayout="tablet-landscape" showSuggestions />,
    );

    const board = container.querySelector<HTMLElement>("[data-tablet-landscape-builder-board]")!;
    const dock = container.querySelector<HTMLElement>("[data-tablet-landscape-builder-dock]")!;
    const row = container.querySelector<HTMLElement>("[data-tablet-landscape-builder-row]")!;
    expect(board).toHaveClass("flex-1");
    expect(dock).toHaveClass("shrink-0");
    expect(row).toHaveClass(
      "grid-cols-[minmax(0,45fr)_minmax(0,15fr)_minmax(0,20fr)_minmax(0,20fr)]",
    );
    expect(row).not.toHaveClass("overflow-hidden");
    expect(Array.from(row.children).map((slot) => slot.getAttribute("data-tablet-landscape-builder-slot")))
      .toEqual(["curve", "average", "suggest", "submit"]);
    for (const slot of Array.from(row.children)) expect(slot).toHaveClass("min-w-0");
    const compactCurve = row.querySelector<HTMLElement>("[data-mana-curve-presentation='compact']")!;
    expect(compactCurve).toBeInTheDocument();
    expect(compactCurve.querySelectorAll("[data-mana-curve-count]")).toHaveLength(7);
    expect(Array.from(compactCurve.querySelectorAll("[data-mana-curve-bucket]"), (bucket) => bucket.textContent))
      .toEqual(["0", "1", "2", "3", "4", "5", "6+"]);
    expect(container.querySelector("[data-tablet-builder-summary]")).not.toBeInTheDocument();
    expect(container.querySelector("[data-tablet-builder-actions]")).not.toBeInTheDocument();

    const compactControls = container.querySelector<HTMLElement>("[data-compact-pool-primary-controls]")!;
    fireEvent.click(within(compactControls).getByRole("button", { name: "Visual builder" }));
    expect(onPreferencesChange).toHaveBeenLastCalledWith(expect.objectContaining({ explicitView: "board" }));

    rerender(
      <LimitedDeckBuilder
        local={{ ...local, preferences: { ...preferences, explicitView: "board" } }}
        responsiveLayout="tablet-landscape"
        showSuggestions
      />,
    );
    expect(container.querySelector("[data-board-columns]")).toBeInTheDocument();
    expect(container.querySelectorAll("[data-mana-curve-presentation='compact'] [data-mana-curve-bucket]")).toHaveLength(7);
    fireEvent.click(screen.getByRole("button", { name: "Text builder" }));
    expect(onPreferencesChange).toHaveBeenLastCalledWith(expect.objectContaining({ explicitView: "compact" }));

    const suggest = within(row).getByRole("button", { name: "Suggest Deck" });
    expect(suggest).toHaveClass("min-h-11", "px-4", "py-2", "text-sm");
    fireEvent.click(suggest);
    expect(suggestDeck).toHaveBeenCalledOnce();
    const submit = within(row).getByRole("button", { name: "Submit Deck" });
    expect(submit).toHaveClass("min-h-11", "px-4", "py-2", "text-sm");
    fireEvent.click(submit);
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("submission rejected");
    expect(row.compareDocumentPosition(alert) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
  });

  it("prevents concurrent workspace deck submissions", async () => {
    let resolveSubmission!: () => void;
    const submitDeck = vi.fn(() => new Promise<void>((resolve) => {
      resolveSubmission = resolve;
    }));
    render(
      <LimitedDeckBuilder
        local={{
          view: { ...TEST_VIEW, min_deck_size: 1 },
          workspace: {
            schemaVersion: 1,
            placements: { "card-1": { zone: "deck", row: 0, column: 0, order: 0 } },
            virtualBasics: [],
          },
          preferences: createDefaultDraftWorkspacePreferences(),
          interactionLocked: false,
          onWorkspaceChange: () => {},
          onPreferencesChange: () => {},
          onSubmitDeck: submitDeck,
          onAddBasicLand: () => {},
          onRemoveBasicLand: () => {},
        }}
        responsiveLayout="tablet-landscape"
        showSuggestions={false}
      />,
    );

    const submit = screen.getByRole("button", { name: "Submit Deck" });
    fireEvent.click(submit);
    expect(submitDeck).toHaveBeenCalledOnce();
    expect(submit).toBeDisabled();
    fireEvent.click(submit);
    expect(submitDeck).toHaveBeenCalledOnce();

    await act(async () => resolveSubmission());
    expect(submit).not.toBeDisabled();
  });

  it.each(["tablet-portrait", "phone-portrait", "phone-landscape", "desktop"] as const)(
    "does not emit tablet landscape markers in %s",
    (responsiveLayout) => {
      const { container } = render(
        <LimitedDeckBuilder
          local={{
            view: TEST_VIEW,
            workspace: {
              schemaVersion: 1,
              placements: { "card-1": { zone: "deck", row: 0, column: 0, order: 0 } },
              virtualBasics: [],
            },
            preferences: createDefaultDraftWorkspacePreferences(),
            interactionLocked: false,
            onWorkspaceChange: () => {},
            onPreferencesChange: () => {},
            onSubmitDeck: () => {},
            onAddBasicLand: () => {},
            onRemoveBasicLand: () => {},
          }}
          responsiveLayout={responsiveLayout}
        />,
      );

      expect(container.querySelector("[data-tablet-landscape-builder-board]")).not.toBeInTheDocument();
      expect(container.querySelector("[data-tablet-landscape-builder-dock]")).not.toBeInTheDocument();
      expect(container.querySelector("[data-tablet-landscape-builder-row]")).not.toBeInTheDocument();
      expect(container.querySelector("[data-tablet-landscape-builder-slot]")).not.toBeInTheDocument();
      expect(container.querySelector("[data-mana-curve-presentation='compact']")).not.toBeInTheDocument();
      for (const curve of container.querySelectorAll("[data-mana-curve-presentation]")) {
        expect(curve).toHaveAttribute("data-mana-curve-presentation", "default");
      }
    },
  );

  it("keeps the desktop DeckStatistics average nonland-only for the spell-and-land fixture", () => {
    const land = {
      instance_id: "land-1",
      name: "Island",
      set_code: "dmu",
      collector_number: "259",
      rarity: "common" as const,
      colors: [],
      cmc: 0,
      type_line: "Basic Land - Island",
    };
    render(
      <LimitedDeckBuilder
        local={{
          view: { ...TEST_VIEW, min_deck_size: 1, pool: [...TEST_VIEW.pool, land] },
          workspace: {
            schemaVersion: 1,
            placements: {
              "card-1": { zone: "deck", row: 0, column: 0, order: 0 },
              "land-1": { zone: "deck", row: 0, column: 1, order: 0 },
            },
            virtualBasics: [],
          },
          preferences: createDefaultDraftWorkspacePreferences(),
          interactionLocked: false,
          onWorkspaceChange: () => {},
          onPreferencesChange: () => {},
          onSubmitDeck: () => {},
          onAddBasicLand: () => {},
          onRemoveBasicLand: () => {},
        }}
        responsiveLayout="desktop"
      />,
    );

    expect(screen.getByText("Average Mana Cost")).toBeInTheDocument();
    expect(screen.getByText("3.00")).toBeInTheDocument();
  });

  it("disables the tablet suggestion button when the workspace cannot suggest", () => {
    render(
      <LimitedDeckBuilder
        local={{
          view: { ...TEST_VIEW, min_deck_size: 1 },
          workspace: {
            schemaVersion: 1,
            placements: { "card-1": { zone: "deck", row: 0, column: 0, order: 0 } },
            virtualBasics: [],
          },
          preferences: createDefaultDraftWorkspacePreferences(),
          interactionLocked: false,
          onWorkspaceChange: () => {},
          onPreferencesChange: () => {},
          onSubmitDeck: () => {},
          onAddBasicLand: () => {},
          onRemoveBasicLand: () => {},
          capabilities: { kind: "editable-pool", suggestions: false },
          onAutoSuggestDeck: vi.fn(),
        }}
        responsiveLayout="tablet-portrait"
        showSuggestions
      />,
    );

    expect(screen.getByRole("button", { name: "Suggest Deck" })).toBeDisabled();
  });

  it("opens a preview on touch long press without moving the card", () => {
    vi.useFakeTimers();
    render(<Harness />);

    const card = screen.getByRole("button", { name: /wind drake/i });
    fireEvent.pointerDown(card, {
      button: 0,
      clientX: 10,
      clientY: 10,
      isPrimary: true,
      pointerId: 1,
      pointerType: "touch",
    });
    act(() => vi.advanceTimersByTime(500));
    fireEvent.click(card, { detail: 0 });

    expect(screen.getByTestId("hover-preview")).toHaveTextContent("Wind Drake");
    expect(screen.getByRole("meter", { name: "Mana value 3" })).toHaveAttribute(
      "aria-valuenow",
      "0",
    );
  });

  it("does not suppress activation after a canceled long press", () => {
    vi.useFakeTimers();
    render(<Harness />);

    const card = screen.getByRole("button", { name: /wind drake/i });
    fireEvent.pointerDown(card, {
      button: 0,
      clientX: 10,
      clientY: 10,
      isPrimary: true,
      pointerId: 1,
      pointerType: "touch",
    });
    act(() => vi.advanceTimersByTime(500));
    fireEvent.pointerCancel(card, { pointerId: 1, pointerType: "touch" });
    fireEvent.click(card, { detail: 0 });

    expect(screen.getByRole("meter", { name: "Mana value 3" })).toHaveAttribute(
      "aria-valuenow",
      "1",
    );
  });

  it("shows the engine validation reason when deck submission fails", async () => {
    render(
      <LimitedDeckBuilder
        view={TEST_VIEW}
        mainDeck={Array.from({ length: 40 }, () => "Wind Drake")}
        landCounts={{}}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={async () => {
          throw new Error("card 'Watery Grave' is not in the drafted pool");
        }}
        showSuggestions={false}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Submit Deck" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Deck needs attention: card 'Watery Grave' is not in the drafted pool",
    );
  });
});

// ── #7507: pool filter row ──────────────────────────────────────────────

const FILTER_VIEW: BuilderView = {
  ...TEST_VIEW,
  pool: [
    ...TEST_VIEW.pool,
    {
      instance_id: "card-2",
      name: "Shock",
      set_code: "dmu",
      collector_number: "9",
      rarity: "common",
      colors: ["R"],
      cmc: 1,
      type_line: "Instant",
    },
  ],
  pool_groups: {
    ...TEST_VIEW.pool_groups,
    type_filter_options: ["creature", "instant"],
    type_groups: [
      {
        kind: "creature",
        total: 1,
        cards: [{ card: TEST_VIEW.pool[0], count: 1, instance_ids: ["card-1"] }],
      },
      {
        kind: "instant",
        total: 1,
        cards: [
          {
            card: {
              instance_id: "card-2",
              name: "Shock",
              set_code: "dmu",
              collector_number: "9",
              rarity: "common",
              colors: ["R"],
              cmc: 1,
              type_line: "Instant",
            },
            count: 1,
            instance_ids: ["card-2"],
          },
        ],
      },
    ],
  },
};

describe("LimitedDeckBuilder pool filters", () => {
  afterEach(cleanup);

  it("narrows the pool grid through an engine type chip and restores on untoggle", async () => {
    render(
      <LimitedDeckBuilder
        view={FILTER_VIEW}
        mainDeck={[]}
        landCounts={{}}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    expect(screen.getByRole("button", { name: /wind drake/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /shock/i })).toBeInTheDocument();

    const chip = screen.getByRole("button", { name: "Instant", pressed: false });
    fireEvent.click(chip);

    await waitFor(() =>
      expect(screen.queryByRole("button", { name: /wind drake/i })).toBeNull(),
    );
    expect(screen.getByRole("button", { name: /shock/i })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Instant", pressed: true }));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: /wind drake/i }),
      ).toBeInTheDocument(),
    );
  });

  it("searches the pool by name, independent of the addable-cards box", async () => {
    render(
      <LimitedDeckBuilder
        view={FILTER_VIEW}
        mainDeck={[]}
        landCounts={{}}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    fireEvent.change(screen.getByPlaceholderText("Search your pool..."), {
      target: { value: "shock" },
    });

    await waitFor(() =>
      expect(screen.queryByRole("button", { name: /wind drake/i })).toBeNull(),
    );
    expect(screen.getByRole("button", { name: /shock/i })).toBeInTheDocument();
    // The addable-cards list is untouched by the pool query.
    expect(
      screen.getByRole("button", { name: "Add Academy Ruins" }),
    ).toBeInTheDocument();
  });

  it("keeps the 44px coarse-pointer floor on both chip dimensions", () => {
    render(
      <LimitedDeckBuilder
        view={FILTER_VIEW}
        mainDeck={[]}
        landCounts={{}}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    const chip = screen.getByRole("button", { name: "Instant", pressed: false });
    // Review round 4: the floor must hold in BOTH dimensions and be relaxed
    // only for fine pointers — never at a viewport breakpoint.
    expect(chip.className).toContain("min-h-[44px]");
    expect(chip.className).toContain("min-w-[44px]");
    expect(chip.className).toContain("pointer-fine:min-h-0");
    expect(chip.className).not.toContain("sm:min-h-0");
  });

  const LEGACY_VIEW: BuilderView = {
    ...FILTER_VIEW,
    pool: [
      {
        instance_id: "golem-1",
        name: "Chrome Golem",
        set_code: "dmu",
        collector_number: "1",
        rarity: "uncommon",
        colors: [],
        cmc: 3,
        type_line: "Artifact Creature — Golem",
      },
      {
        instance_id: "charm-1",
        name: "Azorius Charm",
        set_code: "dmu",
        collector_number: "2",
        rarity: "common",
        colors: ["W", "U"],
        cmc: 2,
        type_line: "Instant",
      },
    ],
    pool_groups: {
      ...FILTER_VIEW.pool_groups,
      // v10 shape: no option lists; the exclusive buckets are present but
      // lossy (no Artifact, no per-color entries).
      type_filter_options: [],
      color_filter_options: [],
    },
  };

  it("offers a legacy view's chips from the engine, memberships included", async () => {
    render(
      <LimitedDeckBuilder
        view={LEGACY_VIEW}
        mainDeck={[]}
        landCounts={{}}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    // Review round 5: the Artifact and White chips exist only in the
    // engine-computed memberships — the exclusive buckets would offer
    // neither.
    expect(
      await screen.findByRole("button", { name: "Artifact", pressed: false }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "White", pressed: false }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Multicolor", pressed: false }),
    ).toBeInTheDocument();
  });

  it("hides the axes of a legacy view when the engine options fail", async () => {
    failOptionsCalls = true;
    try {
      render(
        <LimitedDeckBuilder
          view={LEGACY_VIEW}
          mainDeck={[]}
          landCounts={{}}
          onAddToDeck={() => {}}
          onRemoveFromDeck={() => {}}
          onSetLandCount={() => {}}
          onSubmitDeck={() => {}}
          showSuggestions={false}
        />,
      );

      // Never the lossy exclusive-bucket fallback: with the engine
      // unavailable there are NO type/color chips at all — not even the
      // buckets the legacy view carries.
      await waitFor(() =>
        expect(
          screen.queryByRole("button", { name: "Creature", pressed: false }),
        ).toBeNull(),
      );
      expect(
        screen.queryByRole("button", { name: "Artifact", pressed: false }),
      ).toBeNull();
      expect(
        screen.queryByRole("button", { name: "Multicolor", pressed: false }),
      ).toBeNull();
    } finally {
      failOptionsCalls = false;
    }
  });

  it("clears prior legacy chips while the next legacy pool's options are pending", async () => {
    const nextLegacyView: BuilderView = {
      ...LEGACY_VIEW,
      pool: [
        {
          instance_id: "seal-1",
          name: "Seal of Cleansing",
          set_code: "dmu",
          collector_number: "3",
          rarity: "common",
          colors: ["W"],
          cmc: 2,
          type_line: "Enchantment",
        },
        {
          instance_id: "field-1",
          name: "Plains",
          set_code: "dmu",
          collector_number: "4",
          rarity: "common",
          colors: [],
          cmc: 0,
          type_line: "Land",
        },
      ],
    };
    let resolveOptions!: (value: { types: string[]; colors: string[]; rarities: string[] }) => void;
    deferredOptions = {
      poolId: "seal-1",
      promise: new Promise((resolve) => {
        resolveOptions = resolve;
      }),
    };
    try {
      const { rerender } = render(
        <LimitedDeckBuilder
          view={LEGACY_VIEW}
          mainDeck={[]}
          landCounts={{}}
          onAddToDeck={() => {}}
          onRemoveFromDeck={() => {}}
          onSetLandCount={() => {}}
          onSubmitDeck={() => {}}
          showSuggestions={false}
        />,
      );

      await screen.findByRole("button", { name: "Artifact", pressed: false });

      rerender(
        <LimitedDeckBuilder
          view={nextLegacyView}
          mainDeck={[]}
          landCounts={{}}
          onAddToDeck={() => {}}
          onRemoveFromDeck={() => {}}
          onSetLandCount={() => {}}
          onSubmitDeck={() => {}}
          showSuggestions={false}
        />,
      );

      expect(screen.queryByRole("button", { name: "Artifact", pressed: false })).toBeNull();

      await act(async () => {
        resolveOptions({
          types: ["enchantment", "land"],
          colors: ["white", "colorless"],
          rarities: ["common"],
        });
        await Promise.resolve();
      });

      expect(screen.getByRole("button", { name: "Enchantment", pressed: false })).toBeInTheDocument();
    } finally {
      deferredOptions = null;
    }
  });

  it("announces a failed engine filter and shows the unfiltered listing", async () => {
    failFilterCalls = true;
    try {
      render(
        <LimitedDeckBuilder
          view={FILTER_VIEW}
          mainDeck={[]}
          landCounts={{}}
          onAddToDeck={() => {}}
          onRemoveFromDeck={() => {}}
          onSetLandCount={() => {}}
          onSubmitDeck={() => {}}
          showSuggestions={false}
        />,
      );

      fireEvent.click(screen.getByRole("button", { name: "Instant", pressed: false }));

      // Review round 3: the grid must not silently contradict the active
      // controls — the fallback shows everything AND says so.
      expect(await screen.findByRole("alert")).toHaveTextContent(
        "Filters are unavailable right now — showing all cards.",
      );
      expect(screen.getByRole("button", { name: /wind drake/i })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: /shock/i })).toBeInTheDocument();
    } finally {
      failFilterCalls = false;
    }
  });
});

// ── P8: CR 903.3 commander designation ──────────────────────────────────

const NO_LANDS: Record<string, number> = {};

const VEHICLE_COMMANDER = {
  instance_id: "cmd-1",
  name: "Vehicle Commander",
  // CR 903.3 admits Vehicles. A `type_line.includes("Legendary Creature")`
  // client-side check would wrongly refuse this card.
  type_line: "Legendary Artifact — Vehicle",
  set_code: "dmu",
  collector_number: "1",
  rarity: "rare",
  colors: ["W"],
  cmc: 4,
};

const DECOY_LEGEND = {
  instance_id: "cmd-2",
  name: "Decoy Legend",
  // Reads as a commander to a substring check; the engine says no.
  type_line: "Legendary Creature — Human",
  set_code: "dmu",
  collector_number: "2",
  rarity: "rare",
  colors: ["W"],
  cmc: 2,
};

const SECOND_COMMANDER = {
  instance_id: "cmd-3",
  name: "Second Commander",
  type_line: "Legendary Creature — Elf",
  set_code: "dmu",
  collector_number: "3",
  rarity: "rare",
  colors: ["G"],
  cmc: 3,
};

const PRISMATIC_PIPER = {
  instance_id: "cmd-4",
  // The OTHER CR 903.13e filler. Present in the pool so a pool-derived
  // implementation offers the wrong filler name (V9).
  name: "The Prismatic Piper",
  type_line: "Legendary Creature — Shapeshifter",
  set_code: "dmu",
  collector_number: "4",
  rarity: "common",
  colors: [],
  cmc: 3,
};

const COMMANDER_VIEW: BuilderView = {
  ...TEST_VIEW,
  kind: "CommanderDraft",
  min_deck_size: 60,
  // CR 903.13f(3): the ENGINE-latched tokens. Every pool card below is printed
  // in "dmu", so an implementation reading a card's printing gets "dmu" here.
  draft_set_codes: ["CMM"],
  pool: [
    ...TEST_VIEW.pool,
    VEHICLE_COMMANDER,
    DECOY_LEGEND,
    SECOND_COMMANDER,
    PRISMATIC_PIPER,
  ],
};

// 60 cards: 59 spells plus one designatable card, all backed by the pool.
const SIXTY_CARD_DECK = [
  ...Array.from({ length: 59 }, () => "Wind Drake"),
  "Vehicle Commander",
];

function commanderPanelScope() {
  return within(
    screen.getByRole("heading", { name: "Commander", level: 4 })
      .parentElement as HTMLElement,
  );
}

function candidateScope() {
  return within(screen.getByText("Set as commander:").parentElement as HTMLElement);
}

function sectionScope(headingName: string) {
  return within(
    screen.getByRole("heading", { name: headingName }).parentElement as HTMLElement,
  );
}

describe("LimitedDeckBuilder — CR 903.3 commander designation", () => {
  afterEach(() => {
    cleanup();
    engineEligible.mockReset();
    engineEligible.mockResolvedValue(false);
    enginePartnerCandidates.mockReset();
    enginePartnerCandidates.mockResolvedValue([]);
  });

  function onlyVehicleIsEligible() {
    engineEligible.mockImplementation(async (name: string) => name === "Vehicle Commander");
  }

  /**
   * V1 — CR 903.3: submission is blocked until a commander is designated, even
   * though the card count already satisfies `min_deck_size`.
   *
   * The "ready to submit" marker is the positive reach-guard: `DeckStatus`
   * paints it purely on `spells + lands >= min`, so its presence proves the
   * SIZE gate is already satisfied and it is the DESIGNATION gate refusing.
   * Without it, `toBeDisabled()` would also pass on a builder whose size gate
   * simply had not been met.
   */
  it("blocks submission of a Commander Draft deck until a commander is designated", async () => {
    onlyVehicleIsEligible();
    render(
      <LimitedDeckBuilder
        view={COMMANDER_VIEW}
        mainDeck={SIXTY_CARD_DECK}
        landCounts={NO_LANDS}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    expect(screen.getByText(/ready to submit/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Submit Deck" })).toBeDisabled();
    expect(
      screen.getByText("Designate a commander from your pool to submit."),
    ).toBeInTheDocument();

    fireEvent.click(
      await screen.findByRole("button", { name: "Vehicle Commander" }),
    );

    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Submit Deck" })).not.toBeDisabled(),
    );
  });

  /**
   * V2 — CR 903.3 eligibility is the ENGINE's predicate, asked per name, per
   * format. Reds in BOTH directions on a `type_line.includes("Legendary
   * Creature")` implementation: it would offer the decoy and hide the Vehicle.
   */
  it("offers only the commanders the engine says are eligible", async () => {
    onlyVehicleIsEligible();
    render(
      <LimitedDeckBuilder
        view={COMMANDER_VIEW}
        mainDeck={["Vehicle Commander", "Decoy Legend"]}
        landCounts={NO_LANDS}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    // Positive half — the Vehicle IS offered.
    expect(
      await screen.findByRole("button", { name: "Vehicle Commander" }),
    ).toBeInTheDocument();
    // Negative half, reach-guarded by the positive half in the same render.
    expect(
      candidateScope().queryByRole("button", { name: "Decoy Legend" }),
    ).toBeNull();
    // The format is passed through, not assumed: "Commander" would be wrong.
    expect(engineEligible).toHaveBeenCalledWith("Vehicle Commander", "CommanderDraft");
  });

  /**
   * V3 — CR 702.124b / CR 903.5a: the designated card stays IN the main deck
   * (the opposite of the constructed builder, which filters it out of `main`),
   * and a designation whose last backing copy leaves the deck is dropped.
   */
  it("keeps a designated commander inside the main deck and drops it when its last copy leaves", async () => {
    onlyVehicleIsEligible();
    const removeSpy = vi.fn();

    function CommanderHarness() {
      const [mainDeck, setMainDeck] = useState<string[]>(SIXTY_CARD_DECK);
      return (
        <LimitedDeckBuilder
          view={COMMANDER_VIEW}
          mainDeck={mainDeck}
          landCounts={NO_LANDS}
          onAddToDeck={() => {}}
          onRemoveFromDeck={(cardName) => {
            removeSpy(cardName);
            setMainDeck((prev) => {
              const idx = prev.indexOf(cardName);
              if (idx < 0) return prev;
              const next = prev.slice();
              next.splice(idx, 1);
              return next;
            });
          }}
          onSetLandCount={() => {}}
          onSubmitDeck={() => {}}
          showSuggestions={false}
        />
      );
    }

    render(<CommanderHarness />);

    fireEvent.click(
      await screen.findByRole("button", { name: "Vehicle Commander" }),
    );
    await waitFor(() =>
      expect(commanderPanelScope().getByText("Vehicle Commander")).toBeInTheDocument(),
    );

    // (a) The designation did not remove the card from the deck.
    expect(removeSpy).not.toHaveBeenCalled();
    const deckTile = sectionScope("Main Deck").getByRole("button", {
      name: /vehicle commander/i,
    });
    expect(deckTile).toBeInTheDocument();

    // (b) Removing its last copy drops the designation and re-blocks submit.
    fireEvent.click(deckTile);
    await waitFor(() =>
      expect(screen.getByText("No commander selected")).toBeInTheDocument(),
    );
    expect(screen.getByRole("button", { name: "Submit Deck" })).toBeDisabled();
  });

  /**
   * V5 — CR 903.13f(3): the partner query receives the VIEW's latched set
   * codes, never a pool card's printing. Every pool card is printed in "dmu"
   * while the view says "CMM", so the two authorities disagree on purpose.
   */
  it("pairs a second commander under the drafted set's CR 903.13f(3) grant", async () => {
    engineEligible.mockImplementation(
      async (name: string) => name === "Vehicle Commander" || name === "Second Commander",
    );
    enginePartnerCandidates.mockImplementation(
      async (_first: string, candidates: string[], draftSetCodes: readonly string[]) =>
        draftSetCodes.includes("CMM") ? candidates : [],
    );

    render(
      <LimitedDeckBuilder
        view={COMMANDER_VIEW}
        mainDeck={["Vehicle Commander", "Second Commander"]}
        landCounts={NO_LANDS}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "Vehicle Commander" }));
    fireEvent.click(await screen.findByRole("button", { name: "Second Commander" }));

    await waitFor(() =>
      expect(commanderPanelScope().getAllByRole("button", { name: "Remove" })).toHaveLength(2),
    );
    expect(enginePartnerCandidates).toHaveBeenCalledWith(
      "Vehicle Commander",
      ["Second Commander"],
      ["CMM"],
    );
  });

  /**
   * V5's paired sibling. With no latched set codes the engine grants no
   * partner, so the second designation SWAPS. Neither row discriminates alone:
   * without this one a hard-coded `["CMM"]` passes the row above.
   */
  it("swaps rather than pairs when the draft grants no partner ability", async () => {
    engineEligible.mockImplementation(
      async (name: string) => name === "Vehicle Commander" || name === "Second Commander",
    );
    enginePartnerCandidates.mockImplementation(
      async (_first: string, candidates: string[], draftSetCodes: readonly string[]) =>
        draftSetCodes.includes("CMM") ? candidates : [],
    );

    render(
      <LimitedDeckBuilder
        view={{ ...COMMANDER_VIEW, draft_set_codes: [] }}
        mainDeck={["Vehicle Commander", "Second Commander"]}
        landCounts={NO_LANDS}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "Vehicle Commander" }));
    fireEvent.click(await screen.findByRole("button", { name: "Second Commander" }));

    await waitFor(() =>
      expect(commanderPanelScope().getByText("Second Commander")).toBeInTheDocument(),
    );
    expect(commanderPanelScope().getAllByRole("button", { name: "Remove" })).toHaveLength(1);
    expect(enginePartnerCandidates).toHaveBeenCalledWith(
      "Vehicle Commander",
      ["Second Commander"],
      [],
    );
  });

  /**
   * V6 — green tree: the four non-Commander `DraftKind`s render exactly as
   * before, because `DECK_FORMAT_FOR_KIND` maps each to `null`. Reach-guarded
   * by asserting the pool tile still renders, so a component that crashed
   * could not satisfy the negative.
   *
   * No CR is cited here on purpose. The repo's "four CR 905.1a kinds" idiom is
   * about cards-per-pick (CR 905.1a: "drafts one card"), which is not what this
   * row asserts, and CR 905 is the Conspiracy Draft section.
   */
  it("shows no commander section for a non-Commander draft", () => {
    onlyVehicleIsEligible();
    render(
      <LimitedDeckBuilder
        view={TEST_VIEW}
        mainDeck={Array.from({ length: 40 }, () => "Wind Drake")}
        landCounts={NO_LANDS}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    expect(screen.getByRole("button", { name: /wind drake/i })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Commander", level: 4 })).toBeNull();
    expect(screen.getByRole("button", { name: "Submit Deck" })).not.toBeDisabled();
    expect(engineEligible).not.toHaveBeenCalled();
  });

  /**
   * V9 — CR 903.13e: the filler is offered by the name the ENGINE published.
   * The pool holds the OTHER filler, so a pool-derived implementation offers
   * the wrong name and a hard-coded one offers a name the engine did not grant.
   */
  it("offers the granted commander filler the engine names", async () => {
    onlyVehicleIsEligible();
    render(
      <LimitedDeckBuilder
        view={{
          ...COMMANDER_VIEW,
          grantable_commander_fillers: [{ card_name: "Faceless One", max_copies: 2 }],
        }}
        mainDeck={["Vehicle Commander"]}
        landCounts={NO_LANDS}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    expect(screen.getByRole("button", { name: "Add Faceless One" })).toBeInTheDocument();
    expect(
      await screen.findByText(
        "Your pool also includes up to 2 × Faceless One, usable only as your commander.",
      ),
    ).toBeInTheDocument();
  });

  it("offers no filler when the draft's set grants none", async () => {
    onlyVehicleIsEligible();
    render(
      <LimitedDeckBuilder
        view={COMMANDER_VIEW}
        mainDeck={["Vehicle Commander"]}
        landCounts={NO_LANDS}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    // Reach-guard: the addable list itself is rendering.
    expect(screen.getByRole("button", { name: "Add Plains" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Add Faceless One/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Add The Prismatic Piper/ })).toBeNull();
  });

  /**
   * V11 — the designation is PASSED to the submit handler.
   *
   * This proves the seam exists at THIS surface; it does not itself prove the
   * value is consumed downstream. It now is: `multiplayerDraftStore.submitDeck`
   * forwards it to `DraftAction::SubmitDeck.commanders`, which
   * `submit_deck_inner_carries_the_designation_to_the_session` asserts at the
   * `draft-wasm` seam.
   */
  it("passes the designated commanders to the submit handler", async () => {
    onlyVehicleIsEligible();
    const submitSpy = vi.fn();
    render(
      <LimitedDeckBuilder
        view={COMMANDER_VIEW}
        mainDeck={SIXTY_CARD_DECK}
        landCounts={NO_LANDS}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={submitSpy}
        showSuggestions={false}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "Vehicle Commander" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Submit Deck" })).not.toBeDisabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: "Submit Deck" }));

    await waitFor(() =>
      expect(submitSpy).toHaveBeenCalledWith(["Vehicle Commander"]),
    );
  });

  /**
   * V13 — CR 903.5a, the composition contract THROUGH the real caller. A
   * drafted Commander deck is commanders-INSIDE, so a designated card is a
   * label on a deck card and must be counted ONCE.
   *
   * V7 cannot reach this: it renders `CommanderPanel` with literal props in its
   * own file and never exercises the caller's declared composition.
   */
  it("counts a designated commander once, not twice, in the deck-size indicator", async () => {
    onlyVehicleIsEligible();
    render(
      <LimitedDeckBuilder
        view={COMMANDER_VIEW}
        mainDeck={SIXTY_CARD_DECK}
        landCounts={NO_LANDS}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    // Reach-guard: the indicator exists and already reads 60/60 undesignated,
    // so a render with no panel at all cannot pass the assertion below.
    expect(screen.getByText("60/60 cards")).toBeInTheDocument();

    fireEvent.click(await screen.findByRole("button", { name: "Vehicle Commander" }));
    await waitFor(() =>
      expect(commanderPanelScope().getByText("Vehicle Commander")).toBeInTheDocument(),
    );

    expect(screen.getByText("60/60 cards")).toHaveClass("text-green-400");
    expect(screen.queryByText("61/60 cards")).toBeNull();
  });

  /**
   * V13's false-green sibling. With 59 real cards plus one designation, the
   * commanders-OUTSIDE arithmetic paints the indicator GREEN at "60/60" while
   * `deckValid` (59 < 60) keeps Submit disabled — two adjacent indicators
   * contradicting each other, with the green one wrong.
   */
  it("does not let a designation paint an under-sized deck as complete", async () => {
    onlyVehicleIsEligible();
    render(
      <LimitedDeckBuilder
        view={COMMANDER_VIEW}
        mainDeck={SIXTY_CARD_DECK.slice(1)}
        landCounts={NO_LANDS}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "Vehicle Commander" }));
    await waitFor(() =>
      expect(commanderPanelScope().getByText("Vehicle Commander")).toBeInTheDocument(),
    );

    expect(screen.getByText("59/60 cards")).toHaveClass("text-yellow-400");
    expect(screen.queryByText("60/60 cards")).toBeNull();
    expect(screen.getByRole("button", { name: "Submit Deck" })).toBeDisabled();
  });

  /**
   * The engine-unavailable path. A silent empty candidate list would leave
   * Submit permanently un-satisfiable with no explanation — a dead end, which
   * is worse than a degraded surface. Same standard as the pool filter's
   * `limitedDeck.filterUnavailable`.
   */
  it("announces that commander designation is unavailable when the engine rejects", async () => {
    engineEligible.mockRejectedValue(new Error("engine unavailable"));
    render(
      <LimitedDeckBuilder
        view={COMMANDER_VIEW}
        mainDeck={SIXTY_CARD_DECK}
        landCounts={NO_LANDS}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Commander designation is unavailable right now — the card database could not be loaded.",
    );
    // Reach-guard: the panel itself rendered; it simply offers nothing.
    expect(screen.getByText("No commander selected")).toBeInTheDocument();
  });

  /**
   * V12 — CR 702.124g: no partner ability or combination of them can ever let a
   * player have more than two commanders, INCLUDING when two designations race
   * inside one in-flight partner query.
   *
   * Both clicks land while the first query is still unresolved, so both read
   * the same captured `commanders` and both are answered "pairs". The gate that
   * runs BEFORE the await cannot see the other click; only a re-check against
   * live state at commit time can.
   *
   * Reach-guarded positively, in the same render, twice over: the query is
   * asked TWICE (so neither click was swallowed by an eligibility or
   * already-designated filter), and the first answer genuinely PAIRS to two
   * commanders (so the append path is the one under test, not a click that
   * quietly did nothing). A pre-fix build satisfies both guards and then shows
   * three commanders with Submit enabled.
   */
  it("cannot stack a third commander when two designations race one query", async () => {
    engineEligible.mockImplementation(
      async (name: string) =>
        name === "Vehicle Commander" ||
        name === "Second Commander" ||
        name === "The Prismatic Piper",
    );
    // Hold every partner query open, so both clicks land before either answer.
    const answer: Array<() => void> = [];
    enginePartnerCandidates.mockImplementation(
      (_first: string, candidates: string[]) =>
        new Promise<string[]>((resolve) => {
          answer.push(() => resolve(candidates));
        }),
    );

    render(
      <LimitedDeckBuilder
        view={COMMANDER_VIEW}
        mainDeck={["Vehicle Commander", "Second Commander", "The Prismatic Piper"]}
        landCounts={NO_LANDS}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    // The first designation takes the free slot: no partner query, no await.
    fireEvent.click(await screen.findByRole("button", { name: "Vehicle Commander" }));
    await waitFor(() =>
      expect(commanderPanelScope().getAllByRole("button", { name: "Remove" })).toHaveLength(1),
    );
    expect(enginePartnerCandidates).not.toHaveBeenCalled();

    // Two clicks inside ONE in-flight query. Neither re-renders the panel, so
    // both handlers close over the same single-commander value.
    fireEvent.click(screen.getByRole("button", { name: "Second Commander" }));
    fireEvent.click(screen.getByRole("button", { name: "The Prismatic Piper" }));
    await waitFor(() => expect(enginePartnerCandidates).toHaveBeenCalledTimes(2));

    // Reach-guard: the first answer really does pair. If this is 1, the row
    // below would pass on a builder where designation never worked at all.
    await act(async () => {
      answer[0]();
    });
    await waitFor(() =>
      expect(commanderPanelScope().getAllByRole("button", { name: "Remove" })).toHaveLength(2),
    );

    // The second answer's premise — a SINGLE commander, the one it was asked
    // about — no longer holds, so it may not append. It replaces instead, which
    // is exactly what these two clicks do when they resolve one after the other.
    await act(async () => {
      answer[1]();
    });
    await waitFor(() =>
      expect(commanderPanelScope().getByText("The Prismatic Piper")).toBeInTheDocument(),
    );
    expect(
      commanderPanelScope().getAllByRole("button", { name: "Remove" }),
    ).toHaveLength(1);
  });

  /**
   * V13 — one deck card, one candidate, when two disjoint SOURCES name it.
   *
   * The phase's headline case: the CR 903.13e grant offers *The Prismatic
   * Piper* as an addable row while the player also drafted a copy into the main
   * deck. The designation candidates are drawn from `deckGroups` (pool → main
   * deck) and from the addable rows, which cannot overlap as sources but can
   * collide by NAME. Unmerged, the same candidate renders twice under one React
   * key, and the prune effect's name-keyed Map sees only the last of the two.
   *
   * Reach-guard, positive and in the same render: a zero would THROW at
   * `getAllByRole` rather than pass, and the single-source commander beside it
   * is still offered exactly once — so this cannot go green on a panel that
   * stopped offering candidates.
   */
  it("offers one candidate for a name the deck and the granted filler both hold", async () => {
    engineEligible.mockImplementation(
      async (name: string) =>
        name === "Vehicle Commander" || name === "The Prismatic Piper",
    );

    render(
      <LimitedDeckBuilder
        view={{
          ...COMMANDER_VIEW,
          grantable_commander_fillers: [{ card_name: "The Prismatic Piper", max_copies: 1 }],
        }}
        mainDeck={["Vehicle Commander", "The Prismatic Piper"]}
        // The granted copy, taken: the same name from the OTHER source.
        landCounts={{ "The Prismatic Piper": 1 }}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    await waitFor(() =>
      expect(
        candidateScope().getAllByRole("button", { name: "The Prismatic Piper" }),
      ).toHaveLength(1),
    );
    expect(
      candidateScope().getAllByRole("button", { name: "Vehicle Commander" }),
    ).toHaveLength(1);

    // The merge must SUM across the two sources, not merely dedupe across them,
    // and that is a separate property from the one above -- measured, not
    // asserted: making the land loop overwrite (`byName.set(name, count)`)
    // instead of add reds THIS line alone, with the two candidate assertions
    // above still green and the other 26 rows in this file untouched. No other
    // test here fixes a name held by BOTH sources, so nothing else can see it.
    //
    // What it protects is `in_deck` -- the quantity draft-core's
    // `validate_limited_deck` step 5 compares `designated` against. A merge
    // that dedupes without summing halves it for every collided name and
    // submits a deck the engine reads as short of copies. The panel renders
    // that sum directly (`commanders-inside`, so no designation is added on
    // top): a faithful merge reads 3 -- two main-deck cards plus the one
    // granted copy of a name the deck already holds -- and a non-summing one
    // reads 2.
    expect(screen.getByText("3/60 cards")).toBeInTheDocument();
  });
});
