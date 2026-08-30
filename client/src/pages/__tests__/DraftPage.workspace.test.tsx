// @vitest-environment happy-dom

import type { ReactNode } from "react";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DraftCardInstance, DraftPlayerView } from "../../adapter/draft-adapter";
import { DRAFT_WORKSPACE_PREFERENCES_KEY } from "../../constants/storage";
import type { LocalDeckBuilderController } from "../../components/draft/LimitedDeckBuilder";
import type { PackDisplayController, PackDisplayPresentation } from "../../components/draft/PackDisplay";
import { projectWorkspaceLandCounts } from "../../components/draft/workspace/workspaceProjection";

interface DraftIntroCapture {
  mode: string;
  podSize: number;
  packCount: number;
  cardsPerPack: number;
  packSizes?: number[];
  minDeckSize: number;
  onContinue(): void;
}

const wasm = vi.hoisted(() => ({
  ...(() => {
    Object.assign(globalThis, {
      __DEFAULT_MULTIPLAYER_SERVER_URL__: "wss://lobby.phase-rs.dev/ws",
      __TELEMETRY_URL__: "",
      __CARD_DATA_URL__: "test://card-data",
    });
    return {};
  })(),
  default: vi.fn(async () => undefined),
  start_quick_draft: vi.fn(),
  start_sealed_draft: vi.fn(),
  start_quick_cube_draft: vi.fn(),
  load_card_database: vi.fn(() => 0),
  submit_pick: vi.fn(),
  auto_pick: vi.fn(),
  submit_deck: vi.fn(),
  suggest_deck: vi.fn(),
  suggest_lands: vi.fn(),
  export_draft_session: vi.fn(() => "session"),
}));

const persistence = vi.hoisted(() => ({
  cleanupQuickDraftLifecycle: vi.fn(async () => undefined),
  drainQuickDraftPersistence: vi.fn(async () => undefined),
  inspectActiveQuickDraftLifecycle: vi.fn(async () => null),
  loadDraftRun: vi.fn(async () => null),
  loadQuickDraftSession: vi.fn(async () => null),
  persistQuickDraftSnapshot: vi.fn(async () => undefined),
  publishInitialDraftMatch: vi.fn(async () => undefined),
  publishStagedDraftMatch: vi.fn(async () => undefined),
  recordDraftMatchResult: vi.fn(async () => null),
  runLimits: vi.fn(() => ({ maxWins: 1, maxLosses: 1 })),
}));

// Captures the controller DraftPage hands the deckbuilder so the wiring back
// to the real store can be exercised without rendering the full board.
const captured = vi.hoisted(() => ({
  local: null as LocalDeckBuilderController | null,
  preview: null as { mode?: string; hoverDelayMs?: number } | null,
  menuShell: null as { layout?: string; contentWidthClass?: string; compactTopPadding?: boolean } | null,
  pack: null as PackDisplayController | null,
  presentation: null as PackDisplayPresentation | null,
  phoneToolbarPinned: null as boolean | null,
  shellMode: null as string | null,
  steps: null as { phase?: string; compact?: boolean; arrowSeparators?: boolean } | null,
  intro: null as DraftIntroCapture | null,
}));

vi.mock("@wasm/draft", () => wasm);
vi.mock("../../services/quickDraftPersistence", () => persistence);
vi.mock("../../services/engineRuntime", () => ({
  ensureCardDatabase: vi.fn(async () => 0),
  ensureCardLocale: vi.fn(async () => new Map()),
  getCardFaceData: vi.fn(async () => null),
  getCardParseDetails: vi.fn(async () => null),
  getCardRulings: vi.fn(async () => []),
}));
vi.mock("../../hooks/useCardImage", () => ({ useCardImage: () => ({ src: null, isLoading: false }) }));
vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/chrome/ShellContext", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../components/chrome/ShellContext")>(),
  useDraftShellChrome: (mode: string) => { captured.shellMode = mode; },
}));
vi.mock("../../components/menu/MenuShell", () => ({ MenuShell: (props: { children: ReactNode; layout?: string; contentWidthClass?: string; compactTopPadding?: boolean }) => {
  captured.menuShell = props;
  return <>{props.children}</>;
} }));
vi.mock("../../components/draft/DraftSteps", () => ({ DraftSteps: (props: { phase?: string; compact?: boolean; arrowSeparators?: boolean }) => {
  captured.steps = props;
  return null;
} }));
vi.mock("../../components/draft/DraftProgress", () => ({ DraftProgress: () => <div data-testid="draft-progress" /> }));
vi.mock("../../components/draft/PackDisplay", () => ({ PackDisplay: ({ controller, presentation, phoneToolbarPinned }: { controller: PackDisplayController; presentation: PackDisplayPresentation; phoneToolbarPinned?: boolean }) => {
  captured.pack = controller;
  captured.presentation = presentation;
  captured.phoneToolbarPinned = phoneToolbarPinned ?? false;
  return <div data-testid="pack-display" />;
} }));
vi.mock("../../components/card/HoverCardPreview", () => ({
  HoverCardPreview: (props: { mode?: string; hoverDelayMs?: number }) => {
    captured.preview = props;
    return null;
  },
}));
vi.mock("../../components/draft/BotDifficultySelector", () => ({ BotDifficultySelector: () => null }));
vi.mock("../../components/draft/CubeSetupPanel", () => ({ CubeSetupPanel: () => null }));
vi.mock("../../components/draft/SetSelector", () => ({ SetSelector: () => null }));
vi.mock("../../components/draft/LimitedDeckBuilder", () => ({
  LimitedDeckBuilder: (props: { local?: LocalDeckBuilderController }) => {
    captured.local = props.local ?? null;
    return <div data-testid="limited-deck-builder" />;
  },
}));
vi.mock("../../components/draft/SealedPackOpening", () => ({
  SealedPackOpening: ({ onComplete }: { onComplete(): void }) => (
    <button type="button" data-testid="complete-opening" onClick={onComplete}>open</button>
  ),
}));
vi.mock("../../components/draft/DraftIntro", () => ({
  DraftIntro: (props: DraftIntroCapture) => {
    captured.intro = props;
    return <button type="button" onClick={props.onContinue}>Continue</button>;
  },
}));

import { useDraftStore } from "../../stores/draftStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { DraftPage } from "../DraftPage";

function card(instanceId: string, name = instanceId): DraftCardInstance {
  return {
    instance_id: instanceId, name, set_code: "TST", collector_number: instanceId,
    rarity: "common", colors: [], cmc: 1, type_line: "Card",
  };
}

function view(overrides: Partial<DraftPlayerView> = {}): DraftPlayerView {
  return {
    status: "Drafting", kind: "Quick", pool: [], current_pack: [], draft_effects: [],
    pool_groups: {
      color_groups: [], type_groups: [], cmc_groups: [], rarity_groups: [],
      type_filter_options: [], color_filter_options: [],
      color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
      workspace_capabilities: { rarity_group_order: null },
      workspace_row_classification: { creature_instance_ids: [], noncreature_instance_ids: [] },
    },
    seats: [], current_pack_number: 1, pick_number: 1, pass_direction: "Left",
    cards_per_pack: 14, pack_count: 3, min_deck_size: 40, addable_cards: [],
    timer_remaining_ms: null, standings: [], current_round: 0, tournament_format: "Swiss",
    pod_policy: "Casual", pairings: [], match_config: { match_type: "Bo1" },
    ...overrides,
  } as DraftPlayerView;
}

describe("DraftPage local deckbuilding wiring", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    captured.local = null;
    captured.preview = null;
    captured.menuShell = null;
    captured.pack = null;
    captured.presentation = null;
    captured.phoneToolbarPinned = null;
    captured.shellMode = null;
    captured.steps = null;
    captured.intro = null;
    usePreferencesStore.setState({ draftCardPreviewMode: "none", draftDoubleClickConfirmPick: true });
    useDraftStore.getState().reset();
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1440 });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 900 });
    vi.stubGlobal("fetch", vi.fn(async () => ({ text: async () => "db" }) as unknown as Response));
    localStorage.clear();
  });

  afterEach(() => {
    cleanup();
    useDraftStore.getState().reset();
    vi.unstubAllGlobals();
    localStorage.clear();
  });

  it("hands the deckbuilder a workspace-wired local controller after drafting", async () => {
    wasm.start_quick_draft.mockReturnValue(view({ pool: [card("c1", "Grizzly Bears")] }));
    await act(async () => {
      await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
    });
    act(() => { useDraftStore.setState({ phase: "deckbuilding" }); });

    render(<MemoryRouter><DraftPage /></MemoryRouter>);

    expect(screen.getByTestId("limited-deck-builder")).toBeInTheDocument();
    const controller = captured.local;
    expect(controller).toBeTruthy();
    expect(controller!.view).toBe(useDraftStore.getState().view);
    expect(controller!.workspace).toBe(useDraftStore.getState().workspaceState);
    expect(controller!.interactionLocked).toBe(false);

    // Basic-land control flows to the store's typed virtual-basic action.
    act(() => controller!.onAddBasicLand("Plains"));
    expect(projectWorkspaceLandCounts(useDraftStore.getState().workspaceState!)).toEqual({ Plains: 1 });

    // Submission projects the deck (drafted card plus the added basic) through
    // the real store submit path.
    wasm.submit_deck.mockReturnValue(view({ status: "Pairing" }));
    await act(async () => { await controller!.onSubmitDeck(); });
    expect(wasm.submit_deck).toHaveBeenCalledWith(JSON.stringify(["Grizzly Bears", "Plains"]), JSON.stringify([]));
  });

  it("forwards global draft visual preferences while drafting", async () => {
    usePreferencesStore.setState({ draftCardPreviewMode: "side", draftDoubleClickConfirmPick: true });
    wasm.start_quick_draft.mockReturnValue(view());
    await act(async () => {
      await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
    });

    const { container } = render(<MemoryRouter><DraftPage /></MemoryRouter>);
    const stepsSpacing = container.querySelector("[data-draft-steps-spacing]");
    expect(stepsSpacing).toHaveClass("mb-12");
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(stepsSpacing).toHaveClass("mb-4");
    expect(captured.preview).toMatchObject({ mode: "side", hoverDelayMs: 0 });
    expect(captured.pack).toMatchObject({ kind: "local-workspace", doubleClickPick: true });
  });

  it("forwards the engine-published procedure to the draft intro", async () => {
    wasm.start_quick_draft.mockReturnValue(view({
      pack_count: 4,
      cards_per_pack: 12,
      pack_sizes: [12, 12, 12, 12],
      min_deck_size: 35,
    }));
    await act(async () => {
      await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
    });

    render(<MemoryRouter><DraftPage /></MemoryRouter>);

    expect(captured.intro).toMatchObject({
      mode: "quick",
      podSize: 0,
      packCount: 4,
      cardsPerPack: 12,
      packSizes: [12, 12, 12, 12],
      minDeckSize: 35,
    });
  });

  it("hides_draft_progress_on_phone_viewports", async () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 430 });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 932 });
    wasm.start_quick_draft.mockReturnValue(view({ current_pack: [card("c1")] }));
    await act(async () => useDraftStore.getState().startDraft("pool", "TST", "Test", 2));

    const { container } = render(<MemoryRouter><DraftPage /></MemoryRouter>);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(screen.queryByTestId("draft-progress")).not.toBeInTheDocument();
    expect(screen.getByTestId("pack-display")).toBeInTheDocument();
    expect(container.querySelector('[data-responsive-draft-layout="phone-portrait"]'))
      .toHaveClass("h-[calc(100dvh_-_11rem)]", "min-h-0");
    expect(container.querySelector('[data-responsive-draft-layout="phone-portrait"]'))
      .not.toHaveClass("gap-2", "pb-[112px]");
    expect(container.querySelector('[data-responsive-workspace-layout="phone-portrait"]')?.parentElement)
      .toHaveClass("h-0", "min-h-0");
    expect(captured.menuShell).toMatchObject({ compactTopPadding: true });
    expect(captured.phoneToolbarPinned).toBe(true);
    expect(captured.shellMode).toBe("phone-drafting");
    expect(captured.steps).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show Deck workspace" }));
    expect(captured.phoneToolbarPinned).toBe(false);
  });

  it.each([
    ["tablet-portrait", 768, 1024],
    ["tablet-landscape", 1024, 768],
  ])("hides_draft_progress_on_%s", async (responsiveLayout, width, height) => {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: height });
    wasm.start_quick_draft.mockReturnValue(view({ current_pack: [card("c1")] }));
    await act(async () => useDraftStore.getState().startDraft("pool", "TST", "Test", 2));

    const { container } = render(<MemoryRouter><DraftPage /></MemoryRouter>);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(screen.queryByTestId("draft-progress")).not.toBeInTheDocument();
    expect(container.querySelector(`[data-responsive-draft-layout="${responsiveLayout}"]`)).toBeInTheDocument();
    expect(screen.getByTestId("pack-display")).toBeInTheDocument();
  });

  it("uses_phone_chrome_and_compact_arrow_steps_while_deckbuilding", async () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 924 });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 412 });
    wasm.start_quick_draft.mockReturnValue(view({ pool: [card("c1")] }));
    await act(async () => useDraftStore.getState().startDraft("pool", "TST", "Test", 2));
    act(() => useDraftStore.setState({ phase: "deckbuilding" }));

    const { container } = render(<MemoryRouter><DraftPage /></MemoryRouter>);

    expect(captured.shellMode).toBe("phone-deckbuilding");
    expect(captured.steps).toBeNull();
    expect(container.querySelector("[data-draft-steps-spacing]")).not.toBeInTheDocument();
    expect(captured.menuShell).toMatchObject({ compactTopPadding: true });
    expect(screen.getByTestId("limited-deck-builder")).toBeInTheDocument();
  });

  it.each([
    ["tablet-portrait", 768, 1024],
    ["tablet-landscape", 1024, 768],
  ] as const)("uses_compact_shell_steps_while_deckbuilding_on_%s", async (_layout, width, height) => {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: height });
    wasm.start_quick_draft.mockReturnValue(view({ pool: [card("c1")] }));
    await act(async () => useDraftStore.getState().startDraft("pool", "TST", "Test", 2));
    act(() => useDraftStore.setState({ phase: "deckbuilding" }));

    const { container } = render(<MemoryRouter><DraftPage /></MemoryRouter>);

    expect(captured.shellMode).toBe("tablet-deckbuilding");
    expect(captured.steps).toBeNull();
    expect(container.querySelector("[data-draft-steps-spacing]")).not.toBeInTheDocument();
    expect(captured.menuShell).toMatchObject({ compactTopPadding: true });
    expect(screen.getByTestId("limited-deck-builder")).toBeInTheDocument();
  });

  it("places_a_local_auto_pick_with_the_active_sort_resolver", async () => {
    const candidate = { ...card("three-drop"), cmc: 3, type_line: "Creature" };
    wasm.start_quick_draft.mockReturnValue(view({ current_pack: [candidate] }));
    await act(async () => {
      await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
    });
    render(<MemoryRouter><DraftPage /></MemoryRouter>);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    if (captured.pack?.kind !== "local-workspace") throw new Error("workspace controller not installed");
    wasm.auto_pick.mockReturnValue(view({ pool: [candidate], current_pack: [] }));

    await act(async () => { await captured.pack!.autoPickCard(); });

    expect(useDraftStore.getState().workspaceState?.placements["three-drop"])
      .toMatchObject({ zone: "deck", column: 3, row: 0 });
  });

  it("uses_the_frozen_full_width_shell_fragment", () => {
    render(<MemoryRouter><DraftPage /></MemoryRouter>);
    expect(captured.menuShell).toMatchObject({ layout: "stacked", contentWidthClass: "max-w-none" });
    const source = readFileSync(join(process.cwd(), "src/pages/DraftPage.tsx"), "utf8");
    expect(source).toContain(`        {/* Keep the shell's responsive padding while allowing card-heavy draft
          phases to use all available width. Narrow setup phases retain their
          own local max-widths. */}
        <MenuShell
          layout="stacked"
          contentWidthClass="max-w-none"
          compactTopPadding={
            (phoneLayout && (phase === "drafting" || phase === "deckbuilding"))
            || tabletDeckbuilding
          }
        >`);
  });

  it("repairs_and_persists_scale_through_the_quick_page_setter", async () => {
    wasm.start_quick_draft.mockReturnValue(view({ current_pack: [card("c1")] }));
    await act(async () => useDraftStore.getState().startDraft("pool", "TST", "Test", 2));
    render(<MemoryRouter><DraftPage /></MemoryRouter>);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    const setItem = vi.spyOn(Storage.prototype, "setItem");

    for (const [raw, repaired] of [[1.11, 1.11], [0.734, 0.73], [0, 0.4], [3, 2.9], [Number.NaN, 1.65]] as const) {
      setItem.mockClear();
      await act(async () => captured.presentation!.setPackScale(raw));
      expect(captured.presentation!.packScale).toBe(repaired);
      expect(JSON.parse(localStorage.getItem(DRAFT_WORKSPACE_PREFERENCES_KEY) ?? "null").packScale).toBe(repaired);
      if (Number.isFinite(raw) && raw !== repaired) {
        expect(setItem.mock.calls.map(([, value]) => JSON.parse(value).packScale)).not.toContain(raw);
      }
    }
  });

  it("preserves the workspace across the sealed opening to deckbuilding transition", async () => {
    wasm.start_sealed_draft.mockReturnValue(view({ kind: "Sealed", status: "Deckbuilding" }));
    await act(async () => {
      await useDraftStore.getState().startSealedDraft("pool", "TST", "Test", 2);
    });
    expect(useDraftStore.getState().phase).toBe("opening");
    const openingWorkspace = useDraftStore.getState().workspaceState;
    expect(openingWorkspace).not.toBeNull();

    render(<MemoryRouter><DraftPage /></MemoryRouter>);
    expect(screen.getByTestId("complete-opening")).toBeInTheDocument();
    expect(screen.queryByTestId("limited-deck-builder")).toBeNull();

    fireEvent.click(screen.getByTestId("complete-opening"));

    expect(useDraftStore.getState().phase).toBe("deckbuilding");
    // The opening phase performs no workspace mutation: the exact instance
    // survives into deckbuilding.
    expect(useDraftStore.getState().workspaceState).toBe(openingWorkspace);
    await waitFor(() => expect(screen.getByTestId("limited-deck-builder")).toBeInTheDocument());
    expect(captured.local!.workspace).toBe(openingWorkspace);
  });
});
