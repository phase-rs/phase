import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

import { LimitedDeckBuilder } from "../LimitedDeckBuilder";

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
  },
  seats: [],
  cards_per_pack: 14,
  pack_count: 3,
  min_deck_size: 40,
  addable_cards: ["Plains", "Island", "Academy Ruins"],
  timer_remaining_ms: null,
  standings: [],
  current_round: 0,
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
