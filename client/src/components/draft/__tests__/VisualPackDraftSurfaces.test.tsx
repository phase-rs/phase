import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DraftCardInstance, DraftPlayerView } from "../../../adapter/draft-adapter.ts";
import { useCardImage } from "../../../hooks/useCardImage.ts";
import { LimitedDeckBuilder } from "../LimitedDeckBuilder.tsx";
import { PackDisplay, type PackDisplayController } from "../PackDisplay.tsx";
import { SealedPackOpening } from "../SealedPackOpening.tsx";

const imageMock = vi.hoisted(() => ({
  results: new Map<string, {
    src: string | null;
    isLoading: boolean;
    isRotated: boolean;
    isFlip: boolean;
    rungs?: { small: string; normal: string };
    advanceFailedSource?: (src: string) => void;
  }>(),
}));

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn((name: string) => imageMock.results.get(name) ?? {
    src: null,
    isLoading: false,
    isRotated: false,
    isFlip: false,
  }),
}));

vi.mock("../../../stores/draftStore.ts", () => ({
  useDraftStore: (selector: (state: Record<string, unknown>) => unknown) => selector({
    view: null,
    mainDeck: [],
    landCounts: {},
    selectedCard: null,
    addToDeck: vi.fn(),
    removeFromDeck: vi.fn(),
    setLandCount: vi.fn(),
    autoSuggestDeck: vi.fn(),
    autoSuggestLands: vi.fn(),
    submitDeck: vi.fn(),
    selectCard: vi.fn(),
    confirmPick: vi.fn(),
    pickCardWithDraftEffect: vi.fn(),
    autoPickCard: vi.fn(),
  }),
}));

vi.mock("../../../hooks/useDeckCardData.ts", () => ({
  useDeckCardData: () => ({ cardDataCache: new Map(), cacheCards: vi.fn() }),
}));

vi.mock("../../../services/engineRuntime.ts", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../services/engineRuntime.ts")>()),
  commanderPartnerCandidates: vi.fn(async () => []),
  isCardCommanderEligibleForFormat: vi.fn(async () => false),
}));

vi.mock("../../../viewmodel/limitedPoolFilter.ts", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../viewmodel/limitedPoolFilter.ts")>()),
  fetchPoolFilterOptions: vi.fn(async () => ({ types: [], colors: [], rarities: [] })),
  filterPoolListing: vi.fn(async (cards: DraftCardInstance[]) =>
    cards.map((card) => card.instance_id)),
}));

vi.mock("../../card/HoverCardPreview.tsx", () => ({
  HoverCardPreview: () => null,
}));

const PRINTING_A: DraftCardInstance = {
  instance_id: "same-a",
  name: "Same Name",
  set_code: "aaa",
  collector_number: "7",
  rarity: "common",
  colors: ["U"],
  cmc: 2,
  type_line: "Creature — Bird",
};

const PRINTING_B: DraftCardInstance = {
  ...PRINTING_A,
  instance_id: "same-b",
  set_code: "bbb",
  collector_number: "99",
};

function view(overrides: Partial<DraftPlayerView> = {}): DraftPlayerView {
  return {
    status: "Drafting",
    kind: "Premier",
    launch_capability: "None",
    commanders_required: 0,
    current_pack_number: 0,
    pick_number: 0,
    pass_direction: "Left",
    current_pack: [PRINTING_B],
    required_pick_count: 1,
    pick_selection_mode: "Direct",
    pool: [PRINTING_A],
    draft_effects: [],
    pool_groups: {
      color_groups: [],
      type_groups: [],
      cmc_groups: [],
      rarity_groups: [],
      type_filter_options: ["creature"],
      color_filter_options: ["blue"],
      color_counts: { white: 0, blue: 1, black: 0, red: 0, green: 0 },
      workspace_capabilities: { rarity_group_order: ["common"] },
      workspace_row_classification: { creature_instance_ids: ["same-a", "same-b"], noncreature_instance_ids: [] },
    },
    sealed_packs: [[PRINTING_A]],
    seats: [],
    cards_per_pack: 1,
    pack_sizes: [1],
    pack_set_codes: ["AAA"],
    pack_pick_steps: [1],
    pick_steps_per_pack: 1,
    pack_count: 1,
    min_deck_size: 40,
    addable_cards: [],
    timer_remaining_ms: null,
    standings: [],
    current_round: 0,
    next_pairing_round: 1,
    tournament_format: "Swiss",
    pod_policy: "Competitive",
    pairings: [],
    match_config: { match_type: "Bo1" },
    ...overrides,
  };
}

const packPresentation = { packScale: 1, setPackScale: vi.fn() };

function packController(packView: DraftPlayerView): Extract<PackDisplayController, { kind: "local-workspace" }> {
  return {
    kind: "local-workspace",
    view: packView,
    selectedCard: null,
    pendingIntent: null,
    interactionGeneration: 0,
    interactionLocked: false,
    doubleClickPick: false,
    dragController: {
      handlePointerDown: vi.fn(), handlePointerMove: vi.fn(), handlePointerUp: vi.fn(),
      handlePointerCancel: vi.fn(), handleLostPointerCapture: vi.fn(),
      consumeCompatibilityActivation: vi.fn(() => false),
    },
    selectCard: vi.fn(),
    pickCard: vi.fn(async () => ({ status: "acknowledged" as const })),
    pickCardStep: vi.fn(async () => ({ status: "acknowledged" as const })),
    confirmPick: vi.fn(async () => ({ status: "acknowledged" as const })),
    pickCardWithDraftEffect: vi.fn(async () => ({ status: "acknowledged" as const })),
    autoPickCard: vi.fn(async () => ({ status: "acknowledged" as const })),
  };
}

function result(prefix: string, advanceFailedSource = vi.fn()) {
  return {
    src: `${prefix}-normal.png`,
    isLoading: false,
    isRotated: false,
    isFlip: false,
    rungs: { small: `${prefix}-small.png`, normal: `${prefix}-normal.png` },
    advanceFailedSource,
  };
}

beforeEach(() => {
  imageMock.results.clear();
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("visual-pack draft and sealed surfaces", () => {
  it("keeps the deck-builder tile on its exact printing, rungs, and named exhaustion", () => {
    const advance = vi.fn();
    imageMock.results.set(PRINTING_A.name, result("builder", advance));

    const { rerender } = render(
      <LimitedDeckBuilder
        view={view({ current_pack: null })}
        mainDeck={[]}
        landCounts={{}}
        onAddToDeck={vi.fn()}
        onRemoveFromDeck={vi.fn()}
        onSetLandCount={vi.fn()}
        onSubmitDeck={vi.fn()}
        showSuggestions={false}
      />,
    );
    expect(vi.mocked(useCardImage)).toHaveBeenCalledWith(
      PRINTING_A.name,
      expect.objectContaining({
        size: "normal",
        sourcePrinting: { setCode: "aaa", collectorNumber: "7" },
      }),
    );
    const image = screen.getByAltText(PRINTING_A.name);
    expect(image).toHaveAttribute(
      "srcset",
      "builder-small.png 146w, builder-normal.png 488w",
    );
    fireEvent.error(image);
    expect(advance).toHaveBeenCalledWith("builder-normal.png");

    imageMock.results.set(PRINTING_A.name, {
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    rerender(
      <LimitedDeckBuilder
        view={view({ current_pack: null })}
        mainDeck={[]}
        landCounts={{}}
        onAddToDeck={vi.fn()}
        onRemoveFromDeck={vi.fn()}
        onSetLandCount={vi.fn()}
        onSubmitDeck={vi.fn()}
        showSuggestions={false}
      />,
    );
    const fallback = screen.getByRole("img", { name: PRINTING_A.name });
    expect(fallback).toHaveTextContent(PRINTING_A.name);
    expect(fallback).not.toHaveClass("animate-pulse");
  });

  it("keeps the pack tile on the sibling printing and renders the next active source", () => {
    const advance = vi.fn();
    imageMock.results.set(PRINTING_B.name, result("pack-first", advance));
    const onHover = vi.fn();
    const packView = view({ pool: [], current_pack: [PRINTING_B] });

    const { rerender } = render(
      <PackDisplay controller={packController(packView)} presentation={packPresentation} onCardHover={onHover} />,
    );
    expect(vi.mocked(useCardImage)).toHaveBeenCalledWith(
      PRINTING_B.name,
      expect.objectContaining({
        sourcePrinting: { setCode: "bbb", collectorNumber: "99" },
      }),
    );
    const first = screen.getByAltText(PRINTING_B.name);
    fireEvent.mouseEnter(first.closest("div")!);
    expect(onHover).toHaveBeenCalledWith({
      name: PRINTING_B.name,
      sourcePrinting: { setCode: "bbb", collectorNumber: "99" },
    });
    fireEvent.error(first);
    expect(advance).toHaveBeenCalledWith("pack-first-normal.png");

    imageMock.results.set(PRINTING_B.name, result("pack-next"));
    rerender(<PackDisplay controller={packController(packView)} presentation={packPresentation} onCardHover={onHover} />);
    expect(screen.getByAltText(PRINTING_B.name)).toHaveAttribute(
      "src",
      "pack-next-normal.png",
    );
  });

  it("preserves the exact sealed printing for a remote normal source", async () => {
    const advance = vi.fn();
    imageMock.results.set(PRINTING_A.name, {
      ...result("sealed", advance),
      src: "https://cards.scryfall.io/normal/sealed.jpg",
      rungs: {
        small: "https://cards.scryfall.io/small/sealed.jpg",
        normal: "https://cards.scryfall.io/normal/sealed.jpg",
      },
    });

    render(<SealedPackOpening view={view()} onComplete={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Open pack" }));
    const image = await screen.findByAltText(PRINTING_A.name);
    expect(vi.mocked(useCardImage)).toHaveBeenCalledWith(
      PRINTING_A.name,
      expect.objectContaining({
        sourcePrinting: { setCode: "aaa", collectorNumber: "7" },
      }),
    );
    expect(image).toHaveAttribute(
      "srcset",
      "https://cards.scryfall.io/small/sealed.jpg 146w, https://cards.scryfall.io/normal/sealed.jpg 488w",
    );
    fireEvent.error(image);
    expect(advance).toHaveBeenCalledWith("https://cards.scryfall.io/normal/sealed.jpg");
  });
});
