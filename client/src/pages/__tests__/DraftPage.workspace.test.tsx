// @vitest-environment happy-dom

import type { ReactNode } from "react";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DraftCardInstance, DraftPlayerView } from "../../adapter/draft-adapter";
import { DRAFT_WORKSPACE_PREFERENCES_KEY } from "../../constants/storage";
import { createDefaultDraftWorkspacePreferences } from "../../components/draft/workspace/workspacePreferences";
import type { LocalDeckBuilderController } from "../../components/draft/LimitedDeckBuilder";
import type { PackDisplayController, PackDisplayPresentation } from "../../components/draft/PackDisplay";
import { projectWorkspaceLandCounts } from "../../components/draft/workspace/workspaceProjection";
import { ShellProvider } from "../../components/chrome/ShellContext";

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
  get_bot_deck: vi.fn(() => ({ main_deck: ["Opponent"], lands: {} })),
  booster_pack_pool_for_game: vi.fn(() => []),
  export_draft_session: vi.fn(() => "session"),
  import_draft_session: vi.fn(),
}));

const persistence = vi.hoisted(() => ({
  cleanupQuickDraftLifecycle: vi.fn(async () => undefined),
  drainQuickDraftPersistence: vi.fn(async () => undefined),
  inspectActiveQuickDraftLifecycle: vi.fn<() => Promise<unknown>>(async () => null),
  loadDraftRun: vi.fn<() => Promise<unknown>>(async () => null),
  loadQuickDraftSession: vi.fn<() => Promise<unknown>>(async () => null),
  persistQuickDraftSnapshot: vi.fn(async () => undefined),
  publishInitialDraftMatch: vi.fn(async () => undefined),
  publishStagedDraftMatch: vi.fn<() => Promise<void>>(async () => undefined),
  recordDraftMatchResult: vi.fn(async () => null),
  runLimits: vi.fn(() => ({ maxWins: 1, maxLosses: 1 })),
}));

const formatGate = vi.hoisted(() => ({
  evaluate: vi.fn(async (_request: unknown) => ({ compatible: true, reasons: [] as string[] })),
}));

// Captures the controller DraftPage hands the deckbuilder so the wiring back
// to the real store can be exercised without rendering the full board.
const captured = vi.hoisted(() => ({
  local: null as LocalDeckBuilderController | null,
  preview: null as { mode?: string; hoverDelayMs?: number } | null,
  menuShell: null as { layout?: string; contentWidthClass?: string; compactTopPadding?: boolean; fillEmbeddedHeight?: boolean } | null,
  pack: null as PackDisplayController | null,
  presentation: null as PackDisplayPresentation | null,
  phoneToolbarPinned: null as boolean | null,
  shellMode: null as string | null,
  showProgress: null as boolean | null,
  steps: null as { phase?: string; compact?: boolean; arrowSeparators?: boolean } | null,
  intro: null as DraftIntroCapture | null,
}));

const arrivingPreferences = vi.hoisted(() => vi.fn());

vi.mock("@wasm/draft", () => wasm);
vi.mock("../../services/quickDraftPersistence", () => persistence);
vi.mock("../../adapter/wasm-adapter", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../adapter/wasm-adapter")>(),
  getSharedAdapter: () => ({ evaluateDeckFormatGate: formatGate.evaluate }),
}));
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
  useDraftShellChrome: (mode: string, _phoneAction?: unknown, _progressVariant?: string, showProgress?: boolean) => {
    captured.shellMode = mode;
    captured.showProgress = showProgress ?? true;
  },
}));
vi.mock("../../components/menu/MenuShell", () => ({ MenuShell: (props: { children: ReactNode; layout?: string; contentWidthClass?: string; compactTopPadding?: boolean; fillEmbeddedHeight?: boolean }) => {
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
// Spied on the module the page imports it from. `setArrivingCardBoardPreferences`
// lives in `workspacePreferences` rather than in either store, because both
// draft stores read the published value. Everything else in the module is the
// real implementation, including the `loadDraftWorkspacePreferences` /
// `saveDraftWorkspacePreferences` pair these tests already exercise.
vi.mock("../../components/draft/workspace/workspacePreferences", async (importOriginal) => {
  const actual = await importOriginal<
    typeof import("../../components/draft/workspace/workspacePreferences")
  >();
  // FORWARDS to the real setter rather than swallowing the call. The stores read
  // the published value back through `getArrivingCardBoardPreferences` in this
  // same module, so a bare `vi.fn()` would sever the page-to-store seam and any
  // later placement assertion here would silently measure the module's seeded
  // default instead of what the page published.
  arrivingPreferences.mockImplementation(actual.setArrivingCardBoardPreferences);
  return { ...actual, setArrivingCardBoardPreferences: arrivingPreferences };
});

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
    status: "Drafting", kind: "Quick", launch_capability: "None", commanders_required: 0, pool: [], current_pack: [], draft_effects: [],
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
    formatGate.evaluate.mockResolvedValue({ compatible: true, reasons: [] });
    captured.local = null;
    captured.preview = null;
    captured.menuShell = null;
    captured.pack = null;
    captured.presentation = null;
    captured.phoneToolbarPinned = null;
    captured.shellMode = null;
    captured.showProgress = null;
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

  it("shows a strict Bo3 launch reason, keeps the choice, and allows a later legal retry", async () => {
    wasm.start_quick_draft.mockReturnValue(view({
      pool: [card("forest", "Forest")],
      match_config: { match_type: "Bo3" },
    }));
    await act(async () => useDraftStore.getState().startDraft("pool", "TST", "Test", 2));
    act(() => useDraftStore.setState({ phase: "launching" }));
    formatGate.evaluate.mockImplementation(async (request: unknown) => ({
      compatible: (request as { selected_match_type: string }).selected_match_type !== "Bo3",
      reasons: (request as { selected_match_type: string }).selected_match_type === "Bo3" ? ["BO3 requires a sideboard"] : [],
    }));
    render(<MemoryRouter><DraftPage /></MemoryRouter>);

    fireEvent.click(screen.getByRole("button", { name: /Best of Three/ }));
    expect(useDraftStore.getState().runFormat).toBe("bo3");
    fireEvent.click(screen.getByRole("button", { name: "Start Match" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("BO3 requires a sideboard");
    expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    expect(persistence.publishInitialDraftMatch).not.toHaveBeenCalled();
    expect(useDraftStore.getState().runFormat).toBe("bo3");
    expect(screen.getByRole("button", { name: "Start Match" })).toBeEnabled();

    fireEvent.click(screen.getByRole("button", { name: /Full Run/ }));
    fireEvent.click(screen.getByRole("button", { name: "Start Match" }));
    await waitFor(() => expect(persistence.publishInitialDraftMatch).toHaveBeenCalledOnce());
    expect(formatGate.evaluate).toHaveBeenLastCalledWith(expect.objectContaining({ selected_match_type: "Bo1" }));
  });

  it("disables the start action and format picker while a strict launch gate is pending", async () => {
    wasm.start_quick_draft.mockReturnValue(view({
      pool: [card("forest", "Forest")],
      match_config: { match_type: "Bo3" },
    }));
    await act(async () => useDraftStore.getState().startDraft("pool", "TST", "Test", 2));
    act(() => useDraftStore.setState({ phase: "launching" }));
    let release!: (result: { compatible: boolean; reasons: string[] }) => void;
    formatGate.evaluate.mockImplementationOnce(() => new Promise((resolve) => { release = resolve; }));
    render(<MemoryRouter><DraftPage /></MemoryRouter>);
    fireEvent.click(screen.getByRole("button", { name: "Start Match" }));
    await waitFor(() => expect(formatGate.evaluate).toHaveBeenCalledTimes(2));
    expect(screen.getByRole("button", { name: "Start Match…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: /Best of Three/ })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: /Best of Three/ }));
    useDraftStore.getState().setRunFormat("bo3");
    expect(useDraftStore.getState().runFormat).toBe("run");
    release({ compatible: false, reasons: ["temporarily rejected"] });
    expect(await screen.findByRole("alert")).toHaveTextContent("temporarily rejected");
    expect(screen.getByRole("button", { name: "Start Match" })).toBeEnabled();
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
    expect(captured.showProgress).toBe(false);
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
    expect(captured.showProgress).toBe(false);
    expect(captured.steps).toBeNull();
    expect(container.querySelector("[data-draft-steps-spacing]")).not.toBeInTheDocument();
    expect(captured.menuShell).toMatchObject({ compactTopPadding: true });
    expect(screen.getByTestId("limited-deck-builder")).toBeInTheDocument();
  });

  it("forwards embedded fill only for responsive workspace phases", async () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 768 });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 1024 });
    wasm.start_quick_draft.mockReturnValue(view({ current_pack: [card("c1")] }));
    await act(async () => useDraftStore.getState().startDraft("pool", "TST", "Test", 2));

    const { rerender } = render(
      <ShellProvider value>
        <MemoryRouter><DraftPage /></MemoryRouter>
      </ShellProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(captured.menuShell).toMatchObject({ fillEmbeddedHeight: true });

    act(() => { useDraftStore.setState({ phase: "deckbuilding" }); });
    rerender(
      <ShellProvider value>
        <MemoryRouter><DraftPage /></MemoryRouter>
      </ShellProvider>,
    );
    expect(captured.menuShell).toMatchObject({ fillEmbeddedHeight: true });
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

  it("publishes_the_stored_board_columns_on_mount", () => {
    // The arriving-card placement in `draftStore.installWorkspace` reads the
    // published value, and a `kind: "state"` install can land before the page
    // has changed anything — a resumed draft, a fresh Sealed pool. Without this
    // publication those cards lay out against the module's seeded default
    // rather than the columns this player actually chose.
    localStorage.setItem(DRAFT_WORKSPACE_PREFERENCES_KEY, JSON.stringify({
      ...createDefaultDraftWorkspacePreferences(),
      deck: { sort: "color", columnCount: 5, rows: "one", showHeaders: true },
    }));
    arrivingPreferences.mockClear();

    render(<MemoryRouter><DraftPage /></MemoryRouter>);

    expect(arrivingPreferences).toHaveBeenCalledWith(
      expect.objectContaining({ sort: "color", columnCount: 5 }),
    );
  });

  it("publishes_board_columns_during_the_preference_change_not_after_a_commit", async () => {
    // The solo twin of `DraftPodPage.winston.test.tsx`'s "publishes board
    // columns during the preference change, not after a commit". Deliberately
    // NOT wrapped in `act`: were the publication moved into an effect, nothing
    // would have flushed it by the time this assertion runs.
    localStorage.setItem(DRAFT_WORKSPACE_PREFERENCES_KEY, JSON.stringify({
      ...createDefaultDraftWorkspacePreferences(),
      deck: { sort: "rarity", columnCount: 4, rows: "one", showHeaders: true },
    }));
    wasm.start_quick_draft.mockReturnValue(view({ current_pack: [card("c1")] }));
    await act(async () => useDraftStore.getState().startDraft("pool", "TST", "Test", 2));
    render(<MemoryRouter><DraftPage /></MemoryRouter>);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    arrivingPreferences.mockClear();

    // `setPackScale` routes through `handleWorkspacePreferencesChange`, the one
    // path that publishes, spreading the rest of the preferences unchanged.
    captured.presentation!.setPackScale(1.2);

    expect(arrivingPreferences).toHaveBeenCalledTimes(1);
    expect(arrivingPreferences).toHaveBeenCalledWith({
      sort: "rarity", columnCount: 4, rows: "one", showHeaders: true,
    });
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
          fillEmbeddedHeight={fillEmbeddedHeight}
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

  it.each(["playing", "complete"] as const)("keeps %s visible and retries End Run with the same ID after cleanup rejects", async (phase) => {
    const run = { format: "run" as const, results: phase === "complete"
      ? [{ gameId: "finished", result: "win" as const }] : [],
      playerDeck: ["Player"], opponentDeck: ["Opponent"], usedBotSeats: [1], booster_pack_pool: [] };
    useDraftStore.setState({ draftId: "retained-run", phase, runState: run, runFormat: "run" });
    persistence.cleanupQuickDraftLifecycle.mockRejectedValueOnce(new Error("meta delete failed"));
    render(<MemoryRouter><DraftPage /></MemoryRouter>);
    fireEvent.click(screen.getByRole("button", { name: "End Run" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("meta delete failed");
    expect(useDraftStore.getState().draftId).toBe("retained-run");
    expect(useDraftStore.getState().phase).toBe(phase);
    fireEvent.click(screen.getByRole("button", { name: "Retry End Run" }));
    await waitFor(() => expect(persistence.cleanupQuickDraftLifecycle).toHaveBeenCalledTimes(2));
    expect(persistence.cleanupQuickDraftLifecycle.mock.calls).toEqual([["retained-run"], ["retained-run"]]);
    await waitFor(() => expect(useDraftStore.getState().draftId).toBeNull());
  });

  it("shows the exact draft start error only for the matching Resume run and clears it on a launch attempt", async () => {
    const run = { format: "run" as const, results: [], playerDeck: ["Player"],
      opponentDeck: ["Opponent"], usedBotSeats: [1], booster_pack_pool: [],
      activeMatch: { draftId: "matching-run", gameId: "game", format: "run" as const,
        resultCountAtLaunch: 0, botSeat: 1, opponentDeck: ["Opponent"] } };
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue({
      id: "matching-run", setCode: "custom-cube", difficulty: 2, kind: "Quick", phase: "playing",
    });
    persistence.loadDraftRun.mockResolvedValue(run);
    persistence.loadQuickDraftSession.mockResolvedValue(null);
    render(<MemoryRouter initialEntries={[{ pathname: "/draft/quick", search: "?resume=1",
      state: { draftId: "matching-run", draftStartError: "Contract from Below requires ante" } }]}>
      <DraftPage />
    </MemoryRouter>);
    expect(await screen.findByRole("alert")).toHaveTextContent("Contract from Below requires ante");
    fireEvent.click(screen.getByRole("button", { name: "Next Match" }));
    await waitFor(() => expect(screen.queryByText("Contract from Below requires ante")).toBeNull());
  });

  it("offers End Run for a legacy submitted session without a durable run and retries cleanup", async () => {
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue({
      id: "legacy-run", setCode: "TST", difficulty: 2, kind: "Quick", phase: "launching",
    });
    persistence.loadDraftRun.mockResolvedValue(null);
    persistence.loadQuickDraftSession.mockResolvedValue({
      sessionJson: "saved Pairing session", mainDeck: ["Player"], landCounts: {},
      poolSortMode: "name", poolPanelOpen: false, workspace: null,
    });
    wasm.import_draft_session.mockReturnValue(view({ status: "Pairing" }));
    let rejectCleanup!: (error: Error) => void;
    persistence.cleanupQuickDraftLifecycle.mockReturnValueOnce(new Promise<undefined>((_resolve, reject) => {
      rejectCleanup = reject;
    }));
    render(<MemoryRouter initialEntries={[{ pathname: "/draft/quick", search: "?resume=1",
      state: { draftId: "legacy-run", draftStartError: "exact ante error" } }]}><Routes>
      <Route path="/draft/quick" element={<DraftPage />} />
      <Route path="/draft" element={<div data-testid="draft-home" />} />
    </Routes></MemoryRouter>);
    expect(await screen.findByRole("button", { name: "Start Match" })).toBeEnabled();
    expect(wasm.import_draft_session).toHaveBeenCalledWith("saved Pairing session", 2);
    expect(useDraftStore.getState().phase).toBe("launching");
    expect(screen.getByRole("alert")).toHaveTextContent("exact ante error");
    fireEvent.click(screen.getByRole("button", { name: "End Run" }));
    await waitFor(() => expect(screen.getByRole("button", { name: /Start Match/ })).toBeDisabled());
    expect(screen.getByRole("button", { name: "End Run" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "End Run" }));
    expect(persistence.cleanupQuickDraftLifecycle).toHaveBeenCalledTimes(1);
    await act(async () => { rejectCleanup(new Error("cleanup failed")); });
    expect(await screen.findByRole("alert")).toHaveTextContent("cleanup failed");
    expect(screen.queryByTestId("draft-home")).toBeNull();
    expect(useDraftStore.getState().draftId).toBe("legacy-run");
    fireEvent.click(screen.getByRole("button", { name: "Retry End Run" }));
    expect(await screen.findByTestId("draft-home")).toBeInTheDocument();
    expect(persistence.cleanupQuickDraftLifecycle.mock.calls).toEqual([["legacy-run"], ["legacy-run"]]);
  });

  it("offers Retry Resume and End Run after a run read rejects, then recovers the same ID", async () => {
    const run = { format: "run" as const, results: [], playerDeck: ["Player"],
      opponentDeck: ["Opponent"], usedBotSeats: [1] };
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue({
      id: "read-again", setCode: "TST", difficulty: 2, kind: "Quick", phase: "playing",
    });
    persistence.loadDraftRun.mockRejectedValueOnce(new Error("Storage read failed")).mockResolvedValue(run);
    persistence.loadQuickDraftSession.mockResolvedValue(null);
    render(<MemoryRouter initialEntries={["/draft/quick?resume=1"]}><DraftPage /></MemoryRouter>);
    expect(await screen.findByRole("alert")).toHaveTextContent("Storage read failed");
    expect(screen.getByRole("button", { name: "Retry Resume" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "End Run" })).toBeEnabled();
    expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Retry Resume" }));
    expect(await screen.findByRole("button", { name: "Next Match" })).toBeEnabled();
    expect(useDraftStore.getState().draftId).toBe("read-again");
    expect(screen.queryByText("Storage read failed")).toBeNull();
  });

  it.each([
    { routeId: "route-run", endRun: true },
    { routeId: undefined, endRun: false },
  ])("shows Retry Resume after metadata inspection rejects with route ID $routeId", async ({ routeId, endRun }) => {
    const run = { format: "run" as const, results: [], playerDeck: ["Player"],
      opponentDeck: ["Opponent"], usedBotSeats: [1] };
    persistence.inspectActiveQuickDraftLifecycle
      .mockRejectedValueOnce(new Error("Metadata read failed"))
      .mockResolvedValue({ id: "route-run", setCode: "TST", difficulty: 2, kind: "Quick", phase: "playing" });
    persistence.loadDraftRun.mockResolvedValue(run);
    persistence.loadQuickDraftSession.mockResolvedValue(null);
    render(<MemoryRouter initialEntries={[{ pathname: "/draft/quick", search: "?resume=1",
      state: routeId ? { draftId: routeId } : null }]}><DraftPage /></MemoryRouter>);
    expect(await screen.findByRole("alert")).toHaveTextContent("Metadata read failed");
    expect(screen.getByRole("button", { name: "Retry Resume" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "End Run" }) !== null).toBe(endRun);
    expect(useDraftStore.getState().draftId).toBeNull();
    expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();
    if (endRun) {
      fireEvent.click(screen.getByRole("button", { name: "End Run" }));
      await waitFor(() => expect(persistence.cleanupQuickDraftLifecycle).toHaveBeenCalledWith("route-run"));
    } else {
      fireEvent.click(screen.getByRole("button", { name: "Retry Resume" }));
      expect(await screen.findByRole("button", { name: "Next Match" })).toBeEnabled();
      expect(persistence.loadDraftRun).toHaveBeenCalledWith("route-run");
      expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();
    }
  });

  it("excludes double-clicked Next Match while publication waits and retries after rejection", async () => {
    const run = { format: "run" as const, results: [], playerDeck: ["Player"],
      opponentDeck: ["Opponent"], usedBotSeats: [1],
      activeMatch: { draftId: "next-run", gameId: "next-game", format: "run" as const,
        resultCountAtLaunch: 0, botSeat: 1, opponentDeck: ["Opponent"] } };
    useDraftStore.setState({ draftId: "next-run", selectedSet: "TST", difficulty: 2,
      phase: "playing", runFormat: "run", runState: run });
    persistence.loadDraftRun.mockResolvedValue(run);
    let rejectWrite!: (reason: Error) => void;
    persistence.publishStagedDraftMatch.mockReturnValueOnce(new Promise<void>((_resolve, reject) => {
      rejectWrite = reject;
    }));
    render(<MemoryRouter initialEntries={["/draft/quick"]}><Routes>
      <Route path="/draft/quick" element={<DraftPage />} />
      <Route path="/game/:id" element={<div data-testid="launched-game" />} />
    </Routes></MemoryRouter>);
    fireEvent.click(screen.getByRole("button", { name: "Next Match" }));
    await waitFor(() => expect(persistence.publishStagedDraftMatch).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByRole("button", { name: /Next Match/ }));
    expect(persistence.publishStagedDraftMatch).toHaveBeenCalledOnce();
    expect(screen.queryByTestId("launched-game")).toBeNull();
    rejectWrite(new Error("handoff write failed"));
    expect(await screen.findByRole("alert")).toHaveTextContent("handoff write failed");
    fireEvent.click(screen.getByRole("button", { name: "Next Match" }));
    await waitFor(() => expect(persistence.publishStagedDraftMatch).toHaveBeenCalledTimes(2));
    expect(await screen.findByTestId("launched-game")).toBeInTheDocument();
  });

  it.each([
    { search: "?resume=1", routeId: "other-run" },
    { search: "", routeId: "matching-run" },
  ])("ignores a draft error for a different run or a non-Resume route: $search/$routeId", async ({ search, routeId }) => {
    const run = { format: "run" as const, results: [], playerDeck: ["Player"],
      opponentDeck: ["Opponent"], usedBotSeats: [1] };
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue({
      id: "matching-run", setCode: "TST", difficulty: 2, kind: "Quick", phase: "playing",
    });
    persistence.loadDraftRun.mockResolvedValue(run);
    persistence.loadQuickDraftSession.mockResolvedValue(null);
    render(<MemoryRouter initialEntries={[{ pathname: "/draft/quick", search,
      state: { draftId: routeId, draftStartError: "wrong run error" } }]}><DraftPage /></MemoryRouter>);
    if (search) expect(await screen.findByRole("button", { name: "Next Match" })).toBeEnabled();
    expect(screen.queryByText("wrong run error")).toBeNull();
  });
});
