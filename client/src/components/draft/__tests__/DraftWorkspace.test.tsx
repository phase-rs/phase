import { useState } from "react";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { DraftCardInstance, DraftPoolGroups } from "../../../adapter/draft-adapter";
import { useMultiplayerDraftStore } from "../../../stores/multiplayerDraftStore";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import { CompactSideboard } from "../workspace/CompactSideboard";
import { DraftWorkspace, shouldShowDraftWorkspaceDeck } from "../workspace/DraftWorkspace";
import {
  useDraftWorkspaceDrag,
  type DraftWorkspaceDragController,
  type WorkspaceDragSource,
} from "../workspace/useDraftWorkspaceDrag";
import type { DraftWorkspaceState } from "../workspace/types";
import {
  createDefaultDraftWorkspacePreferences,
  DRAFT_WORKSPACE_COLLAPSED_SIDEBOARD_CARD_WIDTH_PX,
  DRAFT_WORKSPACE_PACK_SCALE_MAX,
  type DraftWorkspacePreferences,
} from "../workspace/workspacePreferences";

const motionState = vi.hoisted(() => ({ reduced: false }));
const previewProps = vi.hoisted(() => ({
  values: [] as Array<{ mode?: string; hoverDelayMs?: number }>,
}));

vi.mock("framer-motion", async (importOriginal) => {
  const actual = await importOriginal<typeof import("framer-motion")>();
  return { ...actual, useReducedMotion: () => motionState.reduced };
});

vi.mock("../../../hooks/useCardImage", () => ({
  useCardImage: () => ({ src: "/card.png", isLoading: false }),
}));

vi.mock("../../card/HoverCardPreview", () => ({
  HoverCardPreview: (props: { card: { name: string } | null; mode?: string; hoverDelayMs?: number }) => {
    previewProps.values.push(props);
    return <div data-testid="workspace-preview">{props.card?.name}</div>;
  },
}));

let restoreBrowserHarness: (() => void) | null = null;

afterEach(() => {
  cleanup();
  restoreBrowserHarness?.();
  restoreBrowserHarness = null;
  motionState.reduced = false;
  previewProps.values = [];
  useMultiplayerDraftStore.getState().reset();
  vi.restoreAllMocks();
});

function installBrowserHarness({
  width = 1024,
  innerHeight = 900,
  viewportWidth = undefined as number | undefined,
  viewportHeight = 768,
  viewportOffsetLeft = 0,
  viewportOffsetTop = 137,
  inset = "24px",
  visualViewportPresent = true,
  reducedMotion = false,
} = {}) {
  motionState.reduced = reducedMotion;
  const originalWidth = Object.getOwnPropertyDescriptor(window, "innerWidth");
  const originalHeight = Object.getOwnPropertyDescriptor(window, "innerHeight");
  const originalViewport = Object.getOwnPropertyDescriptor(window, "visualViewport");
  const originalMatchMedia = Object.getOwnPropertyDescriptor(window, "matchMedia");
  const originalGetComputedStyle = window.getComputedStyle.bind(window);
  let resolvedInset = inset;

  Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
  Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: innerHeight });

  const viewportTarget = new EventTarget();
  const visualViewport = Object.assign(viewportTarget, {
    height: viewportHeight,
    offsetTop: viewportOffsetTop,
    width: viewportWidth ?? width,
    offsetLeft: viewportOffsetLeft,
    pageLeft: viewportOffsetLeft,
    pageTop: viewportOffsetTop,
    scale: 1,
    onresize: null,
    onscroll: null,
  }) as unknown as VisualViewport;
  const viewportAdd = vi.spyOn(visualViewport, "addEventListener");
  const viewportRemove = vi.spyOn(visualViewport, "removeEventListener");
  Object.defineProperty(window, "visualViewport", {
    configurable: true,
    value: visualViewportPresent ? visualViewport : undefined,
  });

  const mediaQueries = new Map<string, MediaQueryList & {
    addEventListener: ReturnType<typeof vi.fn>;
    removeEventListener: ReturnType<typeof vi.fn>;
  }>();
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    value: vi.fn((query: string) => {
      const existing = mediaQueries.get(query);
      if (existing) return existing;
      const target = new EventTarget();
      const addEventListener = vi.fn(target.addEventListener.bind(target));
      const removeEventListener = vi.fn(target.removeEventListener.bind(target));
      const result = Object.assign(target, {
        media: query,
        matches: query === "(prefers-reduced-motion: reduce)" && reducedMotion,
        onchange: null,
        addEventListener,
        removeEventListener,
        addListener: (listener: EventListenerOrEventListenerObject) => target.addEventListener("change", listener),
        removeListener: (listener: EventListenerOrEventListenerObject) => target.removeEventListener("change", listener),
        dispatchEvent: target.dispatchEvent.bind(target),
      }) as unknown as MediaQueryList & {
        addEventListener: ReturnType<typeof vi.fn>;
        removeEventListener: ReturnType<typeof vi.fn>;
      };
      mediaQueries.set(query, result);
      return result;
    }),
  });
  vi.spyOn(window, "getComputedStyle").mockImplementation((element) => (
    element instanceof HTMLElement && element.dataset.safeAreaProbe === "true"
      ? { paddingBottom: resolvedInset } as CSSStyleDeclaration
      : originalGetComputedStyle(element)
  ));

  restoreBrowserHarness = () => {
    if (originalWidth) Object.defineProperty(window, "innerWidth", originalWidth);
    if (originalHeight) Object.defineProperty(window, "innerHeight", originalHeight);
    if (originalViewport) Object.defineProperty(window, "visualViewport", originalViewport);
    else delete (window as { visualViewport?: VisualViewport }).visualViewport;
    if (originalMatchMedia) Object.defineProperty(window, "matchMedia", originalMatchMedia);
    else delete (window as { matchMedia?: typeof window.matchMedia }).matchMedia;
  };

  return {
    visualViewport,
    viewportAdd,
    viewportRemove,
    mediaQueries,
    setInset: (value: string) => { resolvedInset = value; },
  };
}

function card(
  instanceId: string,
  name = instanceId,
  setCode = "TST",
  collectorNumber = instanceId,
  cmc = 2,
): DraftCardInstance {
  return {
    instance_id: instanceId,
    name,
    set_code: setCode,
    collector_number: collectorNumber,
    rarity: "common",
    colors: ["U"],
    cmc,
    type_line: "Creature",
  };
}

function cubeGroups(cards: readonly DraftCardInstance[]): DraftPoolGroups {
  const base = groups(cards);
  const entriesFor = (cmc: number) => cards
    .filter((entry) => entry.cmc === cmc)
    .map((entry) => ({ card: entry, count: 1, instance_ids: [entry.instance_id] }));
  const manaValueOne = entriesFor(1);
  const manaValueThree = entriesFor(3);
  return {
    ...base,
    cmc_groups: [
      { kind: "mana_value1", total: manaValueOne.length, cards: manaValueOne },
      { kind: "mana_value3", total: manaValueThree.length, cards: manaValueThree },
    ],
    rarity_groups: [],
    workspace_capabilities: { rarity_group_order: null },
  };
}

function groups(cards: readonly DraftCardInstance[]): DraftPoolGroups {
  const entries = cards.map((entry) => ({ card: entry, count: 1, instance_ids: [entry.instance_id] }));
  return {
    color_groups: [{ kind: "blue", total: cards.length, cards: entries }],
    type_groups: [{ kind: "creature", total: cards.length, cards: entries }],
    cmc_groups: [{ kind: "mana_value2", total: cards.length, cards: entries }],
    rarity_groups: [{ kind: "common", total: cards.length, cards: entries }],
    type_filter_options: ["creature"],
    color_filter_options: ["blue"],
    color_counts: { white: 0, blue: cards.length, black: 0, red: 0, green: 0 },
    workspace_capabilities: { rarity_group_order: ["common"] },
    workspace_row_classification: {
      creature_instance_ids: cards.map((entry) => entry.instance_id),
      noncreature_instance_ids: [],
    },
  };
}

function state(
  placements: DraftWorkspaceState["placements"],
  virtualBasics: DraftWorkspaceState["virtualBasics"] = [],
): DraftWorkspaceState {
  return { schemaVersion: 1, placements, virtualBasics };
}

function preferences(
  overrides: Partial<DraftWorkspacePreferences> = {},
): DraftWorkspacePreferences {
  const defaults = createDefaultDraftWorkspacePreferences();
  return {
    ...defaults,
    explicitView: "board",
    sideboardCollapsed: false,
    deck: { ...defaults.deck, columnCount: 4, rows: "one" },
    sideboard: { ...defaults.sideboard, columnCount: 2, rows: "one" },
    ...overrides,
  };
}

function lastWorkspaceChange(mock: ReturnType<typeof vi.fn>): DraftWorkspaceState {
  return mock.mock.calls[mock.mock.calls.length - 1][0] as DraftWorkspaceState;
}

function StatefulWorkspace({
  cards,
  initialWorkspace,
  initialPreferences,
  workspaceChanges = vi.fn(),
  preferenceChanges = vi.fn(),
  onCardHover,
  poolGroups,
  interactionLocked = false,
  dragController,
  responsiveLayout,
  responsiveContext,
}: {
  cards: readonly DraftCardInstance[];
  initialWorkspace: DraftWorkspaceState;
  initialPreferences: DraftWorkspacePreferences;
  workspaceChanges?: (next: DraftWorkspaceState) => void;
  preferenceChanges?: (next: DraftWorkspacePreferences) => void;
  onCardHover?: (info: { name: string; sourcePrinting?: { setCode: string; collectorNumber: string } } | null) => void;
  poolGroups?: DraftPoolGroups;
  interactionLocked?: boolean;
  dragController?: DraftWorkspaceDragController;
  responsiveLayout?: "phone-portrait" | "phone-landscape" | "tablet-portrait" | "tablet-landscape" | "desktop";
  responsiveContext?: "draft" | "builder";
}) {
  const [workspace, setWorkspace] = useState(initialWorkspace);
  const [workspacePreferences, setPreferences] = useState(initialPreferences);
  return (
    <DraftWorkspace
      pool={cards}
      poolGroups={poolGroups ?? groups(cards)}
      workspace={workspace}
      preferences={workspacePreferences}
      interactionLocked={interactionLocked}
      dragController={dragController}
      responsiveLayout={responsiveLayout}
      responsiveContext={responsiveContext}
      onWorkspaceChange={(next) => {
        workspaceChanges(next);
        setWorkspace(next);
      }}
      onPreferencesChange={(next) => {
        preferenceChanges(next);
        setPreferences(next);
      }}
      onCardHover={onCardHover}
    />
  );
}

function TouchDragWorkspace({
  cards,
  initialWorkspace,
}: {
  cards: readonly DraftCardInstance[];
  initialWorkspace: DraftWorkspaceState;
}) {
  const [workspace, setWorkspace] = useState(initialWorkspace);
  const [workspacePreferences, setPreferences] = useState(() => preferences({ sideboardCollapsed: true }));
  const interaction = { interactionGeneration: 1, pickInteractionLocked: false, pendingPickIntent: null } as const;
  const dragController = useDraftWorkspaceDrag({
    enabled: true,
    readPickInteraction: () => interaction,
    subscribePickInteraction: () => () => undefined,
    onDrop: () => { throw new Error("Workspace drags do not dispatch picks."); },
    resolveCollapsedSideboardColumn: () => 0,
  });

  return (
    <>
      <DraftWorkspace
        pool={cards}
        poolGroups={groups(cards)}
        workspace={workspace}
        preferences={workspacePreferences}
        dragController={dragController}
        responsiveLayout="phone-portrait"
        onWorkspaceChange={setWorkspace}
        onPreferencesChange={setPreferences}
      />
      <output data-testid="deck-card-zone">{workspace.placements.deck?.zone}</output>
    </>
  );
}

describe("draft workspace shell", () => {
  it.each([
    ["draft phone", "phone-landscape", "draft", "phoneDeckVisualColumnCaps", "landscape"],
    ["draft tablet", "tablet-portrait", "draft", "phoneDeckVisualColumnCaps", "portrait"],
    ["builder phone", "phone-portrait", "builder", "phoneDeckVisualColumnCaps", "portrait"],
    ["builder tablet", "tablet-landscape", "builder", "tabletDeckVisualColumnCaps", "landscape"],
  ] as const)("caps %s visual builder controls at ten columns", (
    _label,
    responsiveLayout,
    responsiveContext,
    target,
    orientation,
  ) => {
    const cards = [card("deck-card")];
    const preferenceChanges = vi.fn();
    const initialPreferences = preferences();
    initialPreferences[target][orientation] = 9;
    render(
      <StatefulWorkspace
        cards={cards}
        initialWorkspace={state({ "deck-card": { zone: "deck", row: 0, column: 0, order: 0 } })}
        initialPreferences={initialPreferences}
        preferenceChanges={preferenceChanges}
        responsiveLayout={responsiveLayout}
        responsiveContext={responsiveContext}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Layout" }));
    const increase = screen.getByRole("button", { name: "Increase max columns per row" });
    const decrease = screen.getByRole("button", { name: "Decrease max columns per row" });
    expect(increase).toHaveClass("h-11", "w-11");
    expect(decrease).toHaveClass("h-11", "w-11");
    fireEvent.click(increase);

    expect(screen.getByRole("group", { name: "Max columns per row" })).toHaveTextContent("10");
    expect(preferenceChanges).toHaveBeenLastCalledWith(expect.objectContaining({
      [target]: expect.objectContaining({ [orientation]: 10 }),
    }));
    expect(increase).toBeDisabled();
  });

  it("uses the draft visual preview setting without showing a workspace control", () => {
    const cards = [card("deck-card"), card("side-card")];
    const preferenceChanges = vi.fn();
    const initialPreferences = preferences({ cardPreviewMode: "none" });
    usePreferencesStore.setState({ draftCardPreviewMode: "side" });

    render(
      <StatefulWorkspace
        cards={cards}
        initialWorkspace={state({
          "deck-card": { zone: "deck", row: 0, column: 0, order: 0 },
          "side-card": { zone: "sideboard", row: 0, column: 0, order: 0 },
        })}
        initialPreferences={initialPreferences}
        preferenceChanges={preferenceChanges}
      />,
    );

    expect(screen.queryByRole("group", { name: "Card preview" })).not.toBeInTheDocument();
    expect(preferenceChanges).not.toHaveBeenCalled();
    expect(previewProps.values.slice(-2)).toEqual([
      expect.objectContaining({ mode: "side", hoverDelayMs: 0 }),
      expect.objectContaining({ mode: "side", hoverDelayMs: 0 }),
    ]);
  });

  it("uses_phone_only_board_presentation_without_rewriting_preferences", () => {
    const cards = [card("deck"), card("side")];
    const preferenceChanges = vi.fn();
    const initialPreferences = preferences({
      deck: { sort: "cmc", columnCount: 7, rows: "two", showHeaders: false },
      sideboardCollapsed: true,
    });
    const { container } = render(
      <DraftWorkspace
        pool={cards}
        poolGroups={groups(cards)}
        workspace={state({
          deck: { zone: "deck", row: 0, column: 6, order: 0 },
          side: { zone: "sideboard", row: 0, column: 0, order: 0 },
        })}
        preferences={initialPreferences}
        responsiveLayout="phone-portrait"
        mobileOverlay
        mobileWorkspaceOpen={false}
        onMobileWorkspaceOpenChange={vi.fn()}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={preferenceChanges}
      />,
    );

    const deck = container.querySelector<HTMLElement>('[data-zone="deck"]')!;
    expect(within(deck).queryByRole("combobox", { name: "Sort board" })).not.toBeInTheDocument();
    expect(within(deck).queryByRole("group", { name: "Board rows" })).not.toBeInTheDocument();
    expect(within(deck).queryByRole("checkbox", { name: "Show headers" })).not.toBeInTheDocument();
    const layoutTrigger = within(deck).getByRole("button", { name: "Layout" });
    expect(layoutTrigger).toHaveAttribute("aria-haspopup", "dialog");
    fireEvent.click(layoutTrigger);
    expect(layoutTrigger).toHaveAttribute("aria-expanded", "true");
    const layoutDialog = screen.getByRole("dialog", { name: "Layout" });
    expect(within(layoutDialog).getByRole("heading", { name: "Columns" })).toBeInTheDocument();
    expect(within(layoutDialog).getByText("Max columns per row")).toBeInTheDocument();
    expect(within(layoutDialog).getByRole("group", { name: "Max columns per row" })).toBeInTheDocument();
    expect(layoutDialog.querySelector("[data-layout-sort-options]")).toHaveClass("grid", "grid-cols-2");
    expect(within(layoutDialog).getByRole("button", { name: "Mana value" })).toHaveAttribute("aria-pressed", "true");
    const colorSort = within(layoutDialog).getByRole("button", { name: "Color" });
    expect(colorSort).toHaveAttribute("aria-pressed", "false");
    fireEvent.click(colorSort);
    expect(preferenceChanges).toHaveBeenCalledWith(expect.objectContaining({
      deck: expect.objectContaining({ sort: "color" }),
    }));
    expect(layoutTrigger).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByRole("dialog", { name: "Layout" })).not.toBeInTheDocument();
    expect(within(deck).queryByRole("group", { name: "Board columns" })).not.toBeInTheDocument();
    expect(deck.querySelectorAll("[data-board-column-group]")).toHaveLength(3);
    expect(deck.querySelectorAll("header[aria-label^='Column ']")).toHaveLength(7);
    expect(within(deck).queryByRole("heading", { name: "Deck (1 card)", level: 2 })).not.toBeInTheDocument();
    expect(preferenceChanges).toHaveBeenCalledTimes(1);

    const summary = container.querySelector<HTMLElement>("[data-mobile-workspace-summary]")!;
    expect(summary).toHaveClass("overflow-x-auto", "whitespace-nowrap");
    expect(within(summary).getByRole("button", { name: "Show Deck workspace" }))
      .toHaveClass("shrink-0", "whitespace-nowrap");
  });

  it("opens_the_phone_workspace_overlay_as_a_board_without_persisting_the_default_view", () => {
    const cards = [card("deck"), card("side")];
    const preferenceChanges = vi.fn();
    const initialPreferences = preferences({
      explicitView: null,
      sideboardCollapsed: true,
    });
    function PhoneWorkspaceOverlay() {
      const [mobileWorkspaceOpen, setMobileWorkspaceOpen] = useState(false);
      return (
        <DraftWorkspace
          pool={cards}
          poolGroups={groups(cards)}
          workspace={state({
            deck: { zone: "deck", row: 0, column: 0, order: 0 },
            side: { zone: "sideboard", row: 0, column: 0, order: 0 },
          })}
          preferences={initialPreferences}
          responsiveLayout="phone-portrait"
          mobileOverlay
          mobileWorkspaceOpen={mobileWorkspaceOpen}
          onMobileWorkspaceOpenChange={setMobileWorkspaceOpen}
          onWorkspaceChange={vi.fn()}
          onPreferencesChange={preferenceChanges}
        />
      );
    }

    const { container } = render(<PhoneWorkspaceOverlay />);
    const deck = container.querySelector<HTMLElement>('[data-zone="deck"]')!;
    expect(deck.querySelectorAll("header[aria-label^='Column ']")).toHaveLength(0);

    fireEvent.click(screen.getByRole("button", { name: "Show Deck workspace" }));

    expect(within(deck).getByRole("button", { name: "Layout" })).toBeInTheDocument();
    expect(deck).toHaveClass("overflow-y-auto", "overscroll-contain");
    expect(within(deck).queryByRole("group", { name: "Board columns" })).not.toBeInTheDocument();
    expect(deck.querySelectorAll("header[aria-label^='Column ']")).toHaveLength(4);
    expect(container.querySelector("[data-mobile-workspace-scrim]")).toHaveClass(
      "fixed",
      "bg-slate-950",
      "overscroll-contain",
    );
    const workspace = container.querySelector<HTMLElement>('[data-responsive-workspace-layout="phone-portrait"]')!;
    expect(workspace).toHaveClass("bg-slate-950", "overscroll-contain");
    expect(preferenceChanges).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Hide deck workspace" }));
    expect(deck.querySelectorAll("header[aria-label^='Column ']")).toHaveLength(0);
  });

  it("uses_builder_phone_sideboard_preferences_for_exact_portrait_and_landscape_toggles", () => {
    const cards = [card("side-a"), card("side-b")];
    function BuilderWorkspace({ responsiveLayout }: { responsiveLayout: "phone-portrait" | "phone-landscape" }) {
      const [workspacePreferences, setPreferences] = useState(() => preferences({
        builderPhoneSideboardCollapsed: true,
      }));
      return (
        <DraftWorkspace
          pool={cards}
          poolGroups={groups(cards)}
          workspace={state({
            "side-a": { zone: "sideboard", row: 0, column: 0, order: 0 },
            "side-b": { zone: "sideboard", row: 0, column: 0, order: 1 },
          })}
          preferences={workspacePreferences}
          responsiveLayout={responsiveLayout}
          responsiveContext="builder"
          onWorkspaceChange={vi.fn()}
          onPreferencesChange={setPreferences}
        />
      );
    }

    const { container, rerender } = render(<BuilderWorkspace responsiveLayout="phone-portrait" />);
    const deck = container.querySelector<HTMLElement>('[data-zone="deck"]')!;
    const layout = within(deck).getByRole("button", { name: "Layout" });
    expect(layout).toBeInTheDocument();
    expect(within(deck).queryByRole("button", { name: "Sort board" })).not.toBeInTheDocument();
    expect(deck).not.toHaveClass("overflow-y-auto");
    expect(container.querySelector('[data-responsive-workspace-layout="phone-portrait"]'))
      .toHaveClass("overflow-y-auto", "overscroll-contain");
    fireEvent.click(layout);
    const layoutDialog = screen.getByRole("dialog", { name: "Layout" });
    expect(within(layoutDialog).getByRole("group", { name: "Max columns per row" })).toBeInTheDocument();
    expect(within(layoutDialog).getByRole("button", { name: "Decrease max columns per row" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Layout" }));
    const portrait = screen.getByRole("region", { name: "Compact sideboard" });
    const portraitOpen = within(portrait).getByRole("button", { name: "Show sideboard (2 cards)" });
    expect(portraitOpen).toHaveTextContent("▲");
    expect(portrait.querySelector("[data-sideboard-body]")).not.toBeInTheDocument();
    fireEvent.click(portraitOpen);
    const portraitClose = within(portrait).getByRole("button", { name: "Hide sideboard" });
    expect(portraitClose).toHaveTextContent("▼");
    expect(portrait.querySelector("[data-sideboard-body]")).toHaveClass("overflow-y-auto", "touch-pan-y");

    rerender(<BuilderWorkspace key="landscape" responsiveLayout="phone-landscape" />);
    const landscape = screen.getByRole("region", { name: "Compact sideboard" });
    const landscapeOpen = within(landscape).getByRole("button", { name: "Show sideboard (2 cards)" });
    expect(landscape).toHaveAttribute("data-sideboard-collapsed", "true");
    expect(within(landscape).getByRole("heading", { name: "Sideboard" })).toHaveClass("[writing-mode:vertical-rl]");
    expect(landscapeOpen).toHaveTextContent("▲");
    expect(landscapeOpen.querySelector("span")).toHaveClass("-rotate-90");
    const collapsedHeader = landscape.querySelector("header")!;
    expect(collapsedHeader.children[0]).toBe(landscapeOpen);
    expect(collapsedHeader.children[1]).toHaveAttribute("data-sideboard-count");
    expect(collapsedHeader.children[1]).toHaveTextContent("2");
    expect(collapsedHeader.children[2]).toHaveTextContent("Sideboard");
    fireEvent.click(landscapeOpen);
    expect(within(landscape).getByRole("button", { name: "Hide sideboard" })).toHaveTextContent("▼");
    expect(landscape.querySelector("[data-sideboard-body]")).toHaveClass("overflow-y-auto", "touch-pan-y");
    expect(landscape.querySelector("[data-card-stack]")).toHaveClass("relative");
    expect(landscape.querySelector("[data-card-stack]")).toHaveAttribute("data-sideboard-column-count", "1");
    expect(landscape.querySelector<HTMLElement>("[data-sideboard-row='1'][data-sideboard-column='0'] [data-instance-id]")!.style.top)
      .toBe("32px");
  });

  it("moves_cards_between_the_phone_board_and_compact_sideboard_with_touch_drags", () => {
    const cards = [card("deck"), card("side")];
    const { container } = render(
      <TouchDragWorkspace
        cards={cards}
        initialWorkspace={state({
          deck: { zone: "deck", row: 0, column: 0, order: 0 },
          side: { zone: "sideboard", row: 0, column: 0, order: 0 },
        })}
      />,
    );
    const deck = container.querySelector<HTMLElement>('[data-zone="deck"]')!;
    const board = deck.querySelector<HTMLElement>("div[data-drop-state]")!;
    const column = deck.querySelector<HTMLElement>('[data-board-column="0"]')!;
    const row = column.querySelector<HTMLElement>('[data-board-row="0"]')!;
    const compactSideboard = container.querySelector<HTMLElement>('[data-drop-target="collapsed-sideboard"]')!;
    const rect = (left: number, top: number, right: number, bottom: number) => ({
      left, top, right, bottom, width: right - left, height: bottom - top,
      x: left, y: top, toJSON: () => ({}),
    }) as DOMRect;
    board.getBoundingClientRect = () => rect(0, 0, 160, 160);
    column.getBoundingClientRect = () => rect(0, 0, 160, 160);
    row.getBoundingClientRect = () => rect(0, 0, 160, 160);
    compactSideboard.getBoundingClientRect = () => rect(200, 0, 360, 160);

    const boardCard = within(deck).getByRole("button", { name: "Inspect deck" });
    boardCard.getBoundingClientRect = () => rect(0, 0, 100, 140);
    boardCard.setPointerCapture = vi.fn();
    boardCard.releasePointerCapture = vi.fn();
    expect(boardCard).toHaveClass("touch-none");
    fireEvent.pointerDown(boardCard, {
      button: 0, clientX: 20, clientY: 20, isPrimary: true, pointerId: 1, pointerType: "touch",
    });
    fireEvent.pointerMove(boardCard, { clientX: 240, clientY: 40, pointerId: 1, pointerType: "touch" });
    fireEvent.pointerUp(boardCard, { clientX: 240, clientY: 40, pointerId: 1, pointerType: "touch" });
    expect(screen.getByTestId("deck-card-zone")).toHaveTextContent("sideboard");

    fireEvent.click(within(compactSideboard).getByRole("button", { name: "Show sideboard (2 cards)" }));
    const compactCard = within(compactSideboard).getByRole("button", { name: "Inspect deck" });
    compactCard.getBoundingClientRect = () => rect(200, 0, 300, 140);
    compactCard.setPointerCapture = vi.fn();
    compactCard.releasePointerCapture = vi.fn();
    expect(compactCard).toHaveClass("touch-pan-y");
    fireEvent.pointerDown(compactCard, {
      button: 0, clientX: 240, clientY: 40, isPrimary: true, pointerId: 2, pointerType: "touch",
    });
    fireEvent.pointerMove(compactCard, { clientX: 40, clientY: 40, pointerId: 2, pointerType: "touch" });
    fireEvent.pointerUp(compactCard, { clientX: 40, clientY: 40, pointerId: 2, pointerType: "touch" });
    expect(screen.getByTestId("deck-card-zone")).toHaveTextContent("deck");
  });

  it("scopes_phone_touch_ownership_and_preserves_responsive_sideboard_layouts", () => {
    const cards = [card("deck"), card("side")];
    const dragController: DraftWorkspaceDragController = {
      announcement: "",
      activeTarget: null,
      dragPreview: null,
      handlePointerDown: vi.fn(),
      handleWorkspacePointerDown: vi.fn(),
      handlePointerMove: vi.fn(),
      handlePointerUp: vi.fn(),
      handlePointerCancel: vi.fn(),
      handleLostPointerCapture: vi.fn(),
      consumeCompatibilityActivation: vi.fn(() => false),
      registerBoard: vi.fn(() => vi.fn()),
      registerColumn: vi.fn(() => vi.fn()),
      registerCollapsedSideboard: vi.fn(),
      dropState: vi.fn(() => ({ zoneActive: false, column: null, row: null })),
      invalidateGeometry: vi.fn(),
      dispose: vi.fn(),
    };
    const baseProps = {
      pool: cards,
      poolGroups: groups(cards),
      workspace: state({
        deck: { zone: "deck" as const, row: 0 as const, column: 0, order: 0 },
        side: { zone: "sideboard" as const, row: 0 as const, column: 0, order: 0 },
      }),
      preferences: preferences({ sideboardCollapsed: false }),
      dragController,
      onWorkspaceChange: vi.fn(),
      onPreferencesChange: vi.fn(),
    };
    const { container, rerender } = render(
      <DraftWorkspace {...baseProps} responsiveLayout="phone-portrait" />,
    );

    const deckCard = () => within(container.querySelector<HTMLElement>('[data-zone="deck"]')!)
      .getByRole("button", { name: "Inspect deck" });
    const sideboard = () => screen.getByRole("region", { name: "Compact sideboard" });
    expect(deckCard()).toHaveClass("touch-none");
    expect(within(sideboard()).getByRole("button", { name: "Inspect side" })).toHaveClass("touch-pan-y");
    expect(sideboard().querySelector("[data-card-stack]")).toHaveClass("relative");
    expect(within(sideboard()).getByRole("heading", { name: "Sideboard" })).toBeInTheDocument();
    expect(within(sideboard()).queryByText("Sideboard (1 card)")).not.toBeInTheDocument();

    rerender(<DraftWorkspace {...baseProps} responsiveLayout="phone-landscape" />);
    expect(sideboard()).toHaveAttribute("data-sideboard-collapsed", "false");
    expect(within(sideboard()).getByRole("heading", { name: "Sideboard (1 card)" })).toBeInTheDocument();

    rerender(<DraftWorkspace {...baseProps} responsiveLayout="tablet-landscape" />);
    expect(deckCard()).toHaveClass("touch-pan-y");
    expect(sideboard().querySelector("[data-card-stack]")).toHaveClass("relative");
    expect(sideboard().querySelector("[data-card-stack]")).not.toHaveClass("grid-cols-2");
    expect(within(sideboard()).getByRole("heading", { name: /Sideboard/ })).toBeInTheDocument();
    const expandedToggle = within(sideboard()).getByRole("button", { name: "Hide sideboard" });
    expect(expandedToggle).toHaveTextContent("▲");
    expect(expandedToggle.querySelector("span")).not.toHaveClass("rotate-90", "-rotate-90");

    rerender(<DraftWorkspace {...baseProps} preferences={preferences({ sideboardCollapsed: true })} responsiveLayout="tablet-landscape" />);
    const collapsedToggle = within(sideboard()).getByRole("button", { name: "Show sideboard (1 card)" });
    expect(collapsedToggle).toHaveTextContent("▲");
    expect(collapsedToggle.querySelector("span")).toHaveClass("-rotate-90");

    rerender(<DraftWorkspace {...baseProps} responsiveLayout="tablet-portrait" />);
    expect(sideboard().querySelector("[data-card-stack]")).toHaveClass("relative");

    rerender(<DraftWorkspace {...baseProps} responsiveLayout="phone-portrait" responsiveContext="builder" />);
    expect(deckCard()).toHaveClass("touch-pan-y");
  });

  it("lays_out_the_draft_sideboard_in_ordered_overlapping_responsive_columns", () => {
    const cards = Array.from({ length: 7 }, (_, index) => card(`side-${index}`));
    const workspace = state(Object.fromEntries(cards.map((entry, index) => [
      entry.instance_id,
      { zone: "sideboard" as const, row: 0 as const, column: 0, order: index },
    ])));
    const props = {
      pool: cards,
      poolGroups: groups(cards),
      workspace,
      preferences: preferences({ sideboardCollapsed: false }),
      onWorkspaceChange: vi.fn(),
      onPreferencesChange: vi.fn(),
    };
    const { rerender } = render(<DraftWorkspace {...props} responsiveLayout="phone-portrait" />);
    const stack = () => screen.getByRole("region", { name: "Compact sideboard" })
      .querySelector<HTMLElement>("[data-card-stack]")!;
    const positions = () => [...stack().querySelectorAll<HTMLElement>("[data-sideboard-column]")]
      .map((node) => [node.dataset.sideboardColumn, node.dataset.sideboardRow]);
    const cardAt = (row: number, column: number) => stack()
      .querySelector<HTMLElement>(`[data-sideboard-row="${row}"][data-sideboard-column="${column}"] [data-instance-id]`)!;

    expect(stack()).toHaveAttribute("data-sideboard-column-count", "2");
    expect(positions().slice(0, 4)).toEqual([["0", "0"], ["1", "0"], ["0", "1"], ["1", "1"]]);
    expect(cardAt(1, 0).style.top).toBe("56px");
    expect(cardAt(0, 1).style.left).toBe("calc(50% + 4px)");
    expect(stack().querySelector<HTMLElement>("[data-sideboard-stack-spacer]")!.style.marginBottom).toBe("168px");

    rerender(<DraftWorkspace {...props} responsiveLayout="phone-landscape" />);
    expect(stack()).toHaveAttribute("data-sideboard-column-count", "1");
    expect(positions().slice(0, 3)).toEqual([["0", "0"], ["0", "1"], ["0", "2"]]);
    expect(cardAt(1, 0).style.top).toBe("32px");
    expect(stack().querySelector<HTMLElement>("[data-sideboard-stack-spacer]")!.style.marginBottom).toBe("192px");

    rerender(<DraftWorkspace {...props} responsiveLayout="tablet-portrait" />);
    expect(stack()).toHaveAttribute("data-sideboard-column-count", "3");
    expect(positions().slice(0, 4)).toEqual([["0", "0"], ["1", "0"], ["2", "0"], ["0", "1"]]);
    expect(cardAt(1, 0).style.top).toBe("72px");
    expect(stack().querySelector<HTMLElement>("[data-sideboard-stack-spacer]")!.style.marginBottom).toBe("144px");

    rerender(<DraftWorkspace {...props} responsiveLayout="tablet-landscape" />);
    expect(stack()).toHaveAttribute("data-sideboard-column-count", "1");
    expect(positions().slice(0, 3)).toEqual([["0", "0"], ["0", "1"], ["0", "2"]]);
    expect(cardAt(1, 0).style.top).toBe("40px");
    expect(stack().querySelector<HTMLElement>("[data-sideboard-stack-spacer]")!.style.marginBottom).toBe("240px");
  });

  it("renders_one_fixed_ordered_pointer_following_overlay", () => {
    const cards = [card("effect-a"), card("effect-b")];
    const source = {
      kind: "draft-effect" as const,
      authorityId: "effect",
      sourceInstanceId: "effect-a",
      instanceIds: ["effect-a", "effect-b"] as const,
      cards,
      sourceIndices: [0, 1],
      interactionGeneration: 1,
      previewWidth: 146,
      previewHeight: 204,
      onAdmission: vi.fn(),
      onSettled: vi.fn(),
    };
    const dragController = {
      announcement: "Dragging effect-a, effect-b.",
      activeTarget: null,
      dragPreview: { source, clientX: 100, clientY: 120 },
      handlePointerDown: vi.fn(), handleWorkspacePointerDown: vi.fn(),
      handlePointerMove: vi.fn(), handlePointerUp: vi.fn(),
      handlePointerCancel: vi.fn(), handleLostPointerCapture: vi.fn(),
      consumeCompatibilityActivation: vi.fn(() => false),
      registerBoard: vi.fn(() => vi.fn()), registerColumn: vi.fn(() => vi.fn()),
      registerCollapsedSideboard: vi.fn(), dropState: vi.fn(() => ({ zoneActive: false, column: null, row: null })),
      invalidateGeometry: vi.fn(), dispose: vi.fn(),
    } satisfies DraftWorkspaceDragController;

    render(
      <DraftWorkspace
        pool={cards}
        poolGroups={groups(cards)}
        workspace={state({
          "effect-a": { zone: "deck", row: 0, column: 0, order: 0 },
          "effect-b": { zone: "deck", row: 0, column: 0, order: 1 },
        })}
        preferences={preferences()}
        dragController={dragController}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    const overlay = screen.getByTestId("draft-drag-preview");
    expect(overlay).toHaveClass("fixed", "opacity-70");
    expect(overlay).toHaveStyle({ left: "112px", top: "132px", pointerEvents: "none" });
    const previews = [...overlay.querySelectorAll<HTMLElement>("[data-drag-instance-id]")];
    expect(previews.map((element) => element.dataset.dragInstanceId)).toEqual(["effect-a", "effect-b"]);
    expect(Number.parseFloat(previews[0].style.width)).toBeLessThan(source.previewWidth);
    expect(Number.parseFloat(previews[0].style.height)).toBeLessThan(source.previewHeight);
  });

  it("clamps_the_complete_overlay_to_desktop_window_edges", () => {
    installBrowserHarness({ width: 800, innerHeight: 600, visualViewportPresent: false });
    const cards = [card("effect-a"), card("effect-b")];
    const source = {
      kind: "draft-effect" as const,
      authorityId: "effect",
      sourceInstanceId: "effect-a",
      instanceIds: ["effect-a", "effect-b"] as const,
      cards,
      sourceIndices: [0, 1],
      interactionGeneration: 1,
      previewWidth: 146,
      previewHeight: 204,
      onAdmission: vi.fn(),
      onSettled: vi.fn(),
    };
    const dragController = {
      announcement: "Dragging effect-a, effect-b.", activeTarget: null,
      dragPreview: { source, clientX: 796, clientY: 596 },
      handlePointerDown: vi.fn(), handleWorkspacePointerDown: vi.fn(),
      handlePointerMove: vi.fn(), handlePointerUp: vi.fn(),
      handlePointerCancel: vi.fn(), handleLostPointerCapture: vi.fn(),
      consumeCompatibilityActivation: vi.fn(() => false),
      registerBoard: vi.fn(() => vi.fn()), registerColumn: vi.fn(() => vi.fn()),
      registerCollapsedSideboard: vi.fn(), dropState: vi.fn(() => ({ zoneActive: false, column: null, row: null })),
      invalidateGeometry: vi.fn(), dispose: vi.fn(),
    } satisfies DraftWorkspaceDragController;

    render(
      <DraftWorkspace
        pool={cards}
        poolGroups={groups(cards)}
        workspace={state({
          "effect-a": { zone: "deck", row: 0, column: 0, order: 0 },
          "effect-b": { zone: "deck", row: 0, column: 0, order: 1 },
        })}
        preferences={preferences()}
        dragController={dragController}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    expect(screen.getByTestId("draft-drag-preview")).toHaveStyle({
      left: "635.4px",
      top: "487.8px",
    });
  });

  it("clamps_the_complete_overlay_inside_an_offset_mobile_visual_viewport", () => {
    installBrowserHarness({
      width: 430,
      innerHeight: 760,
      viewportWidth: 390,
      viewportHeight: 640,
      viewportOffsetLeft: 20,
      viewportOffsetTop: 80,
    });
    const cards = [card("single")];
    const source = {
      kind: "pick" as const,
      authorityId: "single",
      sourceInstanceId: "single",
      instanceIds: ["single"] as const,
      cards,
      sourceIndices: [0],
      interactionGeneration: 1,
      previewWidth: 146,
      previewHeight: 204,
      onAdmission: vi.fn(),
      onSettled: vi.fn(),
    };
    const dragController = {
      announcement: "Dragging single.", activeTarget: null,
      dragPreview: { source, clientX: 425, clientY: 755 },
      handlePointerDown: vi.fn(), handleWorkspacePointerDown: vi.fn(),
      handlePointerMove: vi.fn(), handlePointerUp: vi.fn(),
      handlePointerCancel: vi.fn(), handleLostPointerCapture: vi.fn(),
      consumeCompatibilityActivation: vi.fn(() => false),
      registerBoard: vi.fn(() => vi.fn()), registerColumn: vi.fn(() => vi.fn()),
      registerCollapsedSideboard: vi.fn(), dropState: vi.fn(() => ({ zoneActive: false, column: null, row: null })),
      invalidateGeometry: vi.fn(), dispose: vi.fn(),
    } satisfies DraftWorkspaceDragController;

    render(
      <DraftWorkspace
        pool={cards}
        poolGroups={groups(cards)}
        workspace={state({ single: { zone: "deck", row: 0, column: 0, order: 0 } })}
        preferences={preferences()}
        dragController={dragController}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    expect(screen.getByTestId("draft-drag-preview")).toHaveStyle({
      left: "329.7px",
      top: "607.8px",
    });
  });

  it("fits_a_max_scale_two_card_overlay_inside_a_narrow_offset_visual_viewport", () => {
    const viewportLeft = 23;
    const viewportTop = 71;
    const viewportWidth = 320;
    const viewportHeight = 240;
    installBrowserHarness({
      width: 430,
      innerHeight: 760,
      viewportWidth,
      viewportHeight,
      viewportOffsetLeft: viewportLeft,
      viewportOffsetTop: viewportTop,
    });
    const cards = [card("effect-a"), card("effect-b")];
    const source = {
      kind: "draft-effect" as const,
      authorityId: "effect",
      sourceInstanceId: "effect-a",
      instanceIds: ["effect-a", "effect-b"] as const,
      cards,
      sourceIndices: [0, 1],
      interactionGeneration: 1,
      previewWidth: 146 * DRAFT_WORKSPACE_PACK_SCALE_MAX,
      previewHeight: 204 * DRAFT_WORKSPACE_PACK_SCALE_MAX,
      onAdmission: vi.fn(),
      onSettled: vi.fn(),
    };
    const dragController = {
      announcement: "Dragging effect-a, effect-b.", activeTarget: null,
      dragPreview: { source, clientX: 500, clientY: 500 },
      handlePointerDown: vi.fn(), handleWorkspacePointerDown: vi.fn(),
      handlePointerMove: vi.fn(), handlePointerUp: vi.fn(),
      handlePointerCancel: vi.fn(), handleLostPointerCapture: vi.fn(),
      consumeCompatibilityActivation: vi.fn(() => false),
      registerBoard: vi.fn(() => vi.fn()), registerColumn: vi.fn(() => vi.fn()),
      registerCollapsedSideboard: vi.fn(), dropState: vi.fn(() => ({ zoneActive: false, column: null, row: null })),
      invalidateGeometry: vi.fn(), dispose: vi.fn(),
    } satisfies DraftWorkspaceDragController;

    const renderWorkspace = () => (
      <DraftWorkspace
        pool={cards}
        poolGroups={groups(cards)}
        workspace={state({
          "effect-a": { zone: "deck", row: 0, column: 0, order: 0 },
          "effect-b": { zone: "deck", row: 0, column: 0, order: 1 },
        })}
        preferences={preferences({ packScale: DRAFT_WORKSPACE_PACK_SCALE_MAX })}
        dragController={dragController}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />
    );
    const { rerender } = render(renderWorkspace());

    const overlay = screen.getByTestId("draft-drag-preview");
    const previews = [...overlay.querySelectorAll<HTMLElement>("[data-drag-instance-id]")];
    const left = Number.parseFloat(overlay.style.left);
    const top = Number.parseFloat(overlay.style.top);
    const cardWidths = previews.map((preview) => Number.parseFloat(preview.style.width));
    const cardHeights = previews.map((preview) => Number.parseFloat(preview.style.height));
    const groupWidth = cardWidths.reduce((total, width) => total + width, 0) + 4;
    const groupHeight = Math.max(...cardHeights);

    expect(previews.map((preview) => preview.dataset.dragInstanceId)).toEqual(["effect-a", "effect-b"]);
    for (const [index, cardWidth] of cardWidths.entries()) {
      const cardLeft = left + index * (cardWidth + 4);
      expect(cardWidth).toBeGreaterThanOrEqual(0);
      expect(cardHeights[index]).toBeGreaterThanOrEqual(0);
      expect(cardLeft).toBeGreaterThanOrEqual(viewportLeft);
      expect(cardLeft + cardWidth).toBeLessThanOrEqual(viewportLeft + viewportWidth);
      expect(top + cardHeights[index]).toBeLessThanOrEqual(viewportTop + viewportHeight);
    }
    expect(groupWidth).toBeLessThanOrEqual(viewportWidth);
    expect(groupHeight).toBeLessThanOrEqual(viewportHeight);
    expect(left).toBeGreaterThanOrEqual(viewportLeft);
    expect(top).toBeGreaterThanOrEqual(viewportTop);
    expect(left + groupWidth).toBeLessThanOrEqual(viewportLeft + viewportWidth);
    expect(top + groupHeight).toBeLessThanOrEqual(viewportTop + viewportHeight);

    Object.assign(window.visualViewport!, { width: 2, height: 1 });
    rerender(renderWorkspace());
    const tinyOverlay = screen.getByTestId("draft-drag-preview");
    const tinyPreviews = [...tinyOverlay.querySelectorAll<HTMLElement>("[data-drag-instance-id]")];
    const tinyCardWidth = Number.parseFloat(tinyPreviews[0].style.width);
    const tinyCardHeight = Number.parseFloat(tinyPreviews[0].style.height);
    const tinyGap = Number.parseFloat(tinyOverlay.style.columnGap);
    const tinyGeometry = [
      Number.parseFloat(tinyOverlay.style.left),
      Number.parseFloat(tinyOverlay.style.top),
      tinyCardWidth,
      tinyCardHeight,
      tinyGap,
    ];
    expect(tinyGeometry.every((value) => Number.isFinite(value) && value >= 0)).toBe(true);
    expect(tinyCardWidth * tinyPreviews.length + tinyGap).toBeLessThanOrEqual(2);
    expect(tinyCardHeight).toBeLessThanOrEqual(1);
  });

  it("keeps_the_desktop_board_visible_across_width_changes_and_cleans_them_up", async () => {
    const harness = installBrowserHarness({ width: 1023 });
    const windowAdd = vi.spyOn(window, "addEventListener");
    const windowRemove = vi.spyOn(window, "removeEventListener");
    const { container, unmount } = render(
      <DraftWorkspace
        pool={[]}
        poolGroups={groups([])}
        workspace={state({})}
        preferences={preferences({ explicitView: null, sideboardCollapsed: null })}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    expect(screen.queryByRole("group", { name: "Workspace view" })).not.toBeInTheDocument();
    expect(screen.queryByText("Build your deck")).not.toBeInTheDocument();
    expect(screen.getAllByRole("toolbar", { name: "Board layout" })).toHaveLength(1);
    expect(screen.getByRole("button", { name: "Show sideboard (0 cards)" })).toBeInTheDocument();
    expect(container.querySelector('[data-safe-area-probe="true"]')).not.toBeInTheDocument();
    expect(screen.queryByRole("separator")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Pin workspace|Unpin workspace/ })).not.toBeInTheDocument();
    expect(harness.mediaQueries).not.toHaveProperty("(min-height: 640px)");
    expect(harness.viewportAdd).not.toHaveBeenCalled();

    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1024 });
    harness.mediaQueries.get("(min-width: 1024px)")!.dispatchEvent(new Event("change"));
    await waitFor(() => expect(screen.getAllByRole("toolbar", { name: "Board layout" })).toHaveLength(1));
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1023 });
    harness.mediaQueries.get("(min-width: 1024px)")!.dispatchEvent(new Event("change"));
    await waitFor(() => expect(screen.getAllByRole("toolbar", { name: "Board layout" })).toHaveLength(1));

    const resizeRegistration = windowAdd.mock.calls.find(([type]) => type === "resize")!;
    unmount();
    expect(windowRemove).toHaveBeenCalledWith(...resizeRegistration);
    for (const query of ["(min-width: 1024px)"]) {
      const media = harness.mediaQueries.get(query)!;
      expect(media.removeEventListener).toHaveBeenCalledWith(...media.addEventListener.mock.calls[0]);
    }
  });

  it("commits_repaired_geometry_through_onWorkspaceChange_exactly_once", async () => {
    const cards = [card("deck-card"), card("side-card")];
    const workspaceChanges = vi.fn();
    const { rerender } = render(
      <DraftWorkspace
        pool={cards}
        poolGroups={groups(cards)}
        workspace={state({
          "deck-card": { zone: "deck", row: 8, column: 8, order: 7 },
          "side-card": { zone: "sideboard", row: 9, column: 9, order: 4 },
          stale: { zone: "deck", row: 0, column: 0, order: 0 },
        }, [{ instanceId: "basic", name: "Island" }])}
        preferences={preferences()}
        onWorkspaceChange={workspaceChanges}
        onPreferencesChange={vi.fn()}
      />,
    );

    await waitFor(() => expect(workspaceChanges).toHaveBeenCalledTimes(1));
    const repaired = workspaceChanges.mock.calls[0][0] as DraftWorkspaceState;
    expect(repaired.placements).toMatchObject({
      "deck-card": { zone: "deck", row: 0, column: 3, order: 0 },
      "side-card": { zone: "sideboard", row: 0, column: 1, order: 0 },
      basic: { zone: "deck", row: 0, column: 0, order: 0 },
    });
    expect(repaired.placements).not.toHaveProperty("stale");
    expect(Object.keys(repaired.placements).sort()).toEqual(["basic", "deck-card", "side-card"]);

    rerender(
      <DraftWorkspace
        pool={cards}
        poolGroups={groups(cards)}
        workspace={repaired}
        preferences={preferences()}
        onWorkspaceChange={workspaceChanges}
        onPreferencesChange={vi.fn()}
      />,
    );
    await waitFor(() => expect(workspaceChanges).toHaveBeenCalledTimes(1));
  });

  it("does_not_call_onWorkspaceChange_for_already_normalized_input", async () => {
    const cards = [
      card("shared-a", "Shared Name", "AAA", "1"),
      card("shared-b", "Shared Name", "BBB", "2"),
    ];
    const workspaceChanges = vi.fn();
    render(
      <DraftWorkspace
        pool={cards}
        poolGroups={groups(cards)}
        workspace={state({
          "shared-a": { zone: "deck", row: 0, column: 0, order: 0 },
          "shared-b": { zone: "sideboard", row: 0, column: 0, order: 0 },
          basic: { zone: "deck", row: 0, column: 0, order: 1 },
        }, [{ instanceId: "basic", name: "Island" }])}
        preferences={preferences()}
        onWorkspaceChange={workspaceChanges}
        onPreferencesChange={vi.fn()}
      />,
    );

    await waitFor(() => expect(workspaceChanges).not.toHaveBeenCalled());
  });

  it("renders_each_live_identity_exactly_once_after_restored_out_of_range_placement_normalization", () => {
    const cards = [card("one"), card("two")];
    const { container } = render(
      <DraftWorkspace
        pool={cards}
        poolGroups={groups(cards)}
        workspace={state({
          one: { zone: "deck", row: 4, column: 40, order: 8 },
          two: { zone: "sideboard", row: 4, column: 40, order: 8 },
          stale: { zone: "deck", row: 0, column: 0, order: 0 },
        }, [{ instanceId: "basic", name: "Island" }])}
        preferences={preferences()}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    expect([...container.querySelectorAll("[data-instance-id]")]
      .map((element) => element.getAttribute("data-instance-id"))
      .sort()).toEqual(["basic", "one", "two"]);
  });

  it("updates_deck_sideboard_and_collapsed_sideboard_counts_after_semantic_movement", () => {
    const cards = [card("shared-a", "Shared Name"), card("shared-b", "Shared Name")];
    const workspaceChanges = vi.fn();
    const { container } = render(
      <StatefulWorkspace
        cards={cards}
        initialWorkspace={state({
          "shared-a": { zone: "deck", row: 0, column: 0, order: 0 },
          "shared-b": { zone: "deck", row: 0, column: 0, order: 1 },
        })}
        initialPreferences={preferences({ explicitView: "compact", sideboardCollapsed: true })}
        workspaceChanges={workspaceChanges}
      />,
    );

    const first = container.querySelector<HTMLElement>('[data-instance-id="shared-a"]')!;
    fireEvent.keyDown(within(first).getByRole("button", { name: "Inspect Shared Name" }), {
      key: "ArrowDown",
      ctrlKey: true,
      shiftKey: true,
    });
    expect(screen.getByText("Deck (1 card)")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Show sideboard (1 card)" })).toBeInTheDocument();
    expect(lastWorkspaceChange(workspaceChanges).placements["shared-a"].zone).toBe("sideboard");
    expect(lastWorkspaceChange(workspaceChanges).placements["shared-b"].zone).toBe("deck");
  });

  it("keeps_empty_deck_and_sideboard_as_labeled_move_destinations", () => {
    const { container } = render(
      <DraftWorkspace
        pool={[]}
        poolGroups={groups([])}
        workspace={state({})}
        preferences={preferences()}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    expect(container.querySelector('[data-zone="deck"]')).toHaveAccessibleName("Deck");
    expect(container.querySelector('[data-zone="sideboard"]')).toHaveAccessibleName("Sideboard");
    const deckHeading = screen.getByRole("heading", { name: "Deck (0 cards)", level: 2 });
    const deckToolbar = within(container.querySelector<HTMLElement>('[data-zone="deck"]')!)
      .getByRole("toolbar", { name: "Board layout" });
    expect(deckToolbar.firstElementChild).toBe(deckHeading);
    expect(deckToolbar).toHaveClass("py-1.5");
    expect(deckToolbar).not.toHaveClass("py-2.5");
    expect(deckToolbar).toContainElement(within(deckToolbar).getByRole("combobox", { name: "Sort board" }));
    expect(screen.getByText("Sideboard (0 cards)")).toBeInTheDocument();
  });

  it("shows_live_deck_only_creature_and_land_counts_after_the_header_toggle", () => {
    const cards = [
      { ...card("creature"), type_line: "Creature — Human" },
      { ...card("creature-land"), type_line: "Land Creature — Dryad" },
      { ...card("land"), type_line: "Basic Land — Forest" },
      { ...card("side-creature"), type_line: "Artifact Creature" },
      { ...card("side-land"), type_line: "Land" },
    ];
    const workspace = state({
      creature: { zone: "deck", row: 0, column: 0, order: 0 },
      "creature-land": { zone: "deck", row: 0, column: 0, order: 1 },
      land: { zone: "deck", row: 0, column: 1, order: 0 },
      "side-creature": { zone: "sideboard", row: 0, column: 0, order: 0 },
      "side-land": { zone: "sideboard", row: 0, column: 1, order: 0 },
      "deck-basic": { zone: "deck", row: 0, column: 1, order: 1 },
      "side-basic": { zone: "sideboard", row: 0, column: 1, order: 1 },
    }, [
      { instanceId: "deck-basic", name: "Island" },
      { instanceId: "side-basic", name: "Mountain" },
    ]);
    const { container, rerender } = render(
      <DraftWorkspace
        pool={cards}
        poolGroups={groups(cards)}
        workspace={workspace}
        preferences={preferences()}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    const deck = container.querySelector<HTMLElement>('[data-zone="deck"]')!;
    const showHeaders = within(deck).getByRole("checkbox", { name: "Show headers" });
    const summary = deck.querySelector<HTMLOutputElement>("[data-deck-type-counts]")!;
    expect(showHeaders.parentElement?.nextElementSibling).toBe(summary);
    expect(within(summary).getByLabelText("2 Creature")).toBeInTheDocument();
    expect(within(summary).getByLabelText("3 Land")).toBeInTheDocument();
    expect(container.querySelectorAll("[data-deck-type-counts]")).toHaveLength(1);

    rerender(
      <DraftWorkspace
        pool={cards}
        poolGroups={groups(cards)}
        workspace={{
          ...workspace,
          placements: {
            ...workspace.placements,
            creature: { zone: "sideboard", row: 0, column: 0, order: 0 },
          },
        }}
        preferences={preferences()}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );
    expect(within(summary).getByLabelText("1 Creature")).toBeInTheDocument();
  });

  it("keeps_the_desktop_board_visible_when_a_compact_preference_is_restored", () => {
    const cards = [card("one")];
    const workspaceChanges = vi.fn();
    const workspace = state({ one: { zone: "deck", row: 0, column: 0, order: 0 } });
    const props = {
      pool: cards,
      poolGroups: groups(cards),
      workspace,
      onWorkspaceChange: workspaceChanges,
      onPreferencesChange: vi.fn(),
    };
    const { rerender } = render(
      <DraftWorkspace
        {...props}
        preferences={preferences({ explicitView: "compact" })}
      />,
    );

    expect(screen.queryByRole("group", { name: "Workspace view" })).not.toBeInTheDocument();
    expect(screen.getAllByRole("toolbar", { name: "Board layout" })).toHaveLength(2);
    rerender(<DraftWorkspace {...props} preferences={preferences({ explicitView: "board" })} />);
    expect(screen.getAllByRole("toolbar", { name: "Board layout" })).toHaveLength(2);
    expect(workspaceChanges).not.toHaveBeenCalled();
  });

  it("switches_builder_phone_compact_and_visual_views_without_a_compact_landscape_sideboard", () => {
    const preferenceChanges = vi.fn();
    const props = {
      pool: [card("one")],
      poolGroups: groups([card("one")]),
      workspace: state({ one: { zone: "deck", row: 0, column: 0, order: 0 } }),
      onWorkspaceChange: vi.fn(),
      onPreferencesChange: preferenceChanges,
      compactDeckControls: <button type="button">Add Lands</button>,
      responsiveLayout: "phone-landscape" as const,
      responsiveContext: "builder" as const,
    };
    const { container, rerender } = render(
      <DraftWorkspace {...props} preferences={preferences({ explicitView: null })} />,
    );

    expect(screen.getByRole("button", { name: "Visual builder" })).toHaveClass("min-h-11");
    const phonePrimary = container.querySelector<HTMLElement>("[data-compact-pool-primary-controls]")!;
    expect(within(phonePrimary).getAllByRole("button").map((button) => button.textContent))
      .toEqual(["Group", "Add Lands", "Visual builder"]);
    expect(phonePrimary.children[2]).toHaveAttribute("data-deck-type-counts");
    expect(container.querySelector("[data-compact-pool-trailing-controls]")).toHaveClass("ml-auto");
    expect(container.querySelector("[data-deck-type-counts]")).toHaveClass("text-[0.65625rem]");
    expect(screen.queryByRole("region", { name: "Compact sideboard" })).not.toBeInTheDocument();
    expect(container.querySelector<HTMLElement>("[data-zone='deck']")).toHaveClass("overflow-hidden");
    fireEvent.click(screen.getByRole("button", { name: "Visual builder" }));
    expect(preferenceChanges).toHaveBeenLastCalledWith(expect.objectContaining({ explicitView: "board" }));

    rerender(<DraftWorkspace {...props} preferences={preferences({ explicitView: "board" })} />);
    expect(screen.getByRole("button", { name: "Text builder" })).toHaveClass("ml-auto", "min-h-11");
    expect(screen.getByRole("region", { name: "Compact sideboard" })).toBeInTheDocument();
    expect(container.querySelector<HTMLElement>("[data-zone='deck']")).toHaveClass("overflow-y-auto", "overscroll-contain");
    fireEvent.click(screen.getByRole("button", { name: "Text builder" }));
    expect(preferenceChanges).toHaveBeenLastCalledWith(expect.objectContaining({ explicitView: "compact" }));

    rerender(<DraftWorkspace
      {...props}
      responsiveLayout="desktop"
      preferences={preferences({ explicitView: "compact" })}
    />);
    expect(screen.queryByRole("button", { name: "Visual builder" })).not.toBeInTheDocument();
    expect(screen.getAllByRole("toolbar", { name: "Board layout" })).toHaveLength(2);

    rerender(<DraftWorkspace
      {...props}
      responsiveLayout="phone-portrait"
      preferences={preferences({ explicitView: "compact" })}
    />);
    expect(screen.queryByRole("region", { name: "Compact sideboard" })).not.toBeInTheDocument();

    rerender(<DraftWorkspace
      {...props}
      responsiveLayout="phone-portrait"
      preferences={preferences({ explicitView: "board" })}
    />);
    expect(screen.getByRole("region", { name: "Compact sideboard" })).toBeInTheDocument();
    expect(screen.getByRole("toolbar", { name: "Board layout" })).toHaveClass("px-2");
    expect(container.querySelector("[data-deck-type-counts]")).toHaveClass("text-[0.65625rem]");

    rerender(<DraftWorkspace
      {...props}
      responsiveLayout="tablet-portrait"
      preferences={preferences({ explicitView: "compact" })}
    />);
    const tabletPrimary = container.querySelector<HTMLElement>("[data-compact-pool-primary-controls]")!;
    expect(within(tabletPrimary).getAllByRole("button").map((button) => button.textContent))
      .toEqual(["Group", "Add Lands", "Visual builder"]);
    expect(within(tabletPrimary).getByRole("button", { name: "Visual builder" })).toHaveClass("min-h-11");
    expect(container.querySelector("[data-deck-type-counts]")).toHaveClass("text-sm");
    expect(container.querySelector<HTMLElement>("[data-zone='deck']")).toHaveClass("overflow-hidden");
    expect(container.querySelector("[data-workspace-composition='collapsed']"))
      .toHaveClass("flex", "h-full", "min-h-0", "flex-col", "overflow-hidden");
    expect(screen.queryByRole("heading", { name: "Deck (1 card)", level: 2 })).not.toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Compact sideboard" })).not.toBeInTheDocument();
    fireEvent.click(within(tabletPrimary).getByRole("button", { name: "Group" }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: "Color" }));
    expect(within(tabletPrimary).getByRole("button", { name: "Group" })).toBeInTheDocument();

    rerender(<DraftWorkspace
      {...props}
      responsiveLayout="tablet-portrait"
      preferences={preferences({ explicitView: "board" })}
    />);
    expect(screen.getByRole("button", { name: "Text builder" })).toHaveClass("min-h-11");
    expect(screen.getByRole("toolbar", { name: "Board layout" })).toHaveClass("px-4");
    expect(container.querySelector("[data-deck-type-counts]")).toHaveClass("text-sm");

    rerender(<DraftWorkspace
      {...props}
      responsiveLayout="tablet-landscape"
      preferences={preferences({ explicitView: "board" })}
    />);
    expect(screen.getByRole("button", { name: "Text builder" })).toHaveClass("min-h-11");
  });

  it.each(["tablet-portrait", "tablet-landscape"] as const)(
    "keeps builder compact content reachable after collapsing the %s visual deck",
    (responsiveLayout) => {
      const preferenceChanges = vi.fn();
      const props = {
        pool: [card("one")],
        poolGroups: groups([card("one")]),
        workspace: state({ one: { zone: "deck", row: 0, column: 0, order: 0 } }),
        onWorkspaceChange: vi.fn(),
        onPreferencesChange: preferenceChanges,
        compactDeckControls: <button type="button">Add Lands</button>,
        responsiveLayout,
        responsiveContext: "builder" as const,
      };
      expect(shouldShowDraftWorkspaceDeck(true, true)).toBe(true);
      const { rerender } = render(
        <DraftWorkspace {...props} preferences={preferences({ explicitView: "board" })} />,
      );

      fireEvent.click(screen.getByRole("button", { name: "Text builder" }));
      expect(preferenceChanges).toHaveBeenLastCalledWith(expect.objectContaining({ explicitView: "compact" }));

      rerender(<DraftWorkspace {...props} preferences={preferences({ explicitView: "compact" })} />);
      expect(screen.getByRole("button", { name: "Group" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Add Lands" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Visual builder" })).toBeInTheDocument();
      expect(screen.getByText("one")).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Visual builder" }));
      expect(preferenceChanges).toHaveBeenLastCalledWith(expect.objectContaining({ explicitView: "board" }));
    },
  );

  it("uses_a_responsive_collapsed_sideboard_width_only_for_desktop_draft", () => {
    const props = {
      pool: [] as DraftCardInstance[],
      poolGroups: groups([]),
      workspace: state({}),
      preferences: preferences({ explicitView: "compact", sideboardCollapsed: true }),
      onWorkspaceChange: vi.fn(),
      onPreferencesChange: vi.fn(),
      responsiveLayout: "desktop" as const,
    };
    const { container, rerender } = render(<DraftWorkspace {...props} />);
    expect(container.querySelector<HTMLElement>("[data-workspace-composition='collapsed']")!.style
      .getPropertyValue("--collapsed-sideboard-card-width")).toBe(
        `clamp(166.4px, 16vw, ${DRAFT_WORKSPACE_COLLAPSED_SIDEBOARD_CARD_WIDTH_PX * 0.8}px)`,
      );

    rerender(<DraftWorkspace {...props} responsiveContext="builder" />);
    expect(container.querySelector<HTMLElement>("[data-workspace-composition='collapsed']")!.style
      .getPropertyValue("--collapsed-sideboard-card-width")).toBe("240.89999999999998px");
  });

  it("renders_collapsed_sideboard_cards_with_preview_empty_state_and_move_to_deck", () => {
    const cards = [card("side", "Side Card", "CUB", "42")];
    const workspaceChanges = vi.fn();
    const preferenceChanges = vi.fn();
    const onCardHover = vi.fn();
    const { container } = render(
      <StatefulWorkspace
        cards={cards}
        initialWorkspace={state({ side: { zone: "sideboard", row: 0, column: 0, order: 0 } })}
        initialPreferences={preferences({ explicitView: "compact" })}
        workspaceChanges={workspaceChanges}
        preferenceChanges={preferenceChanges}
        onCardHover={onCardHover}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Hide sideboard" }));
    expect(preferenceChanges).toHaveBeenLastCalledWith(expect.objectContaining({ sideboardCollapsed: true }));
    expect(screen.getByRole("button", { name: "Show sideboard (1 card)" })).toHaveAttribute("aria-expanded", "false");
    const compactSideboard = screen.getByRole("region", { name: "Compact sideboard" });
    const compactCard = container.querySelector<HTMLElement>(
      '[aria-label="Compact sideboard"] [data-instance-id="side"]',
    )!;
    fireEvent.mouseEnter(within(compactCard).getByRole("button", { name: "Inspect Side Card" }));
    expect(onCardHover).toHaveBeenLastCalledWith({
      name: "Side Card",
      sourcePrinting: { setCode: "CUB", collectorNumber: "42" },
    });

    fireEvent.click(within(compactCard).getByRole("button", { name: "Inspect Side Card" }));
    expect(lastWorkspaceChange(workspaceChanges).placements.side.zone).toBe("deck");
    expect(within(compactSideboard).getByText("No cards in the sideboard")).toBeInTheDocument();
  });

  it("removes_virtual_basics_clicked_in_the_compact_sideboard", () => {
    const workspaceChanges = vi.fn();
    render(
      <StatefulWorkspace
        cards={[]}
        initialWorkspace={state(
          { basic: { zone: "sideboard", row: 0, column: 0, order: 0 } },
          [{ instanceId: "basic", name: "Island" }],
        )}
        initialPreferences={preferences({ explicitView: "compact", sideboardCollapsed: false })}
        workspaceChanges={workspaceChanges}
        responsiveLayout="phone-landscape"
        preferenceChanges={vi.fn()}
      />,
    );

    const compactSideboard = screen.getByRole("region", { name: "Compact sideboard" });
    expect(within(compactSideboard).getByRole("button", { name: "Remove Island" }))
      .toBeInTheDocument();
    fireEvent.click(within(compactSideboard).getByRole("button", { name: "Inspect Island" }));

    expect(lastWorkspaceChange(workspaceChanges)).toMatchObject({
      placements: {},
      virtualBasics: [],
    });
    expect(within(compactSideboard).getByText("No cards in the sideboard")).toBeInTheDocument();
  });

  it("resolves_restored_rarity_to_cmc_for_cube_board_ui_order_and_mutations", () => {
    const cards = [
      card("low", "Low Cost", "CUB", "1", 1),
      card("high", "High Cost", "CUB", "2", 3),
    ];
    const preferenceChanges = vi.fn();
    const workspaceChanges = vi.fn();
    const { container } = render(
      <StatefulWorkspace
        cards={cards}
        poolGroups={cubeGroups(cards)}
        initialWorkspace={state({
          low: { zone: "deck", row: 0, column: 0, order: 0 },
          high: { zone: "deck", row: 0, column: 1, order: 0 },
        })}
        initialPreferences={preferences({
          deck: { sort: "rarity", columnCount: 2, rows: "one", showHeaders: true },
        })}
        workspaceChanges={workspaceChanges}
        preferenceChanges={preferenceChanges}
      />,
    );

    const deck = container.querySelector<HTMLElement>('[data-zone="deck"]')!;
    expect([...deck.querySelectorAll("[data-instance-id]")]
      .map((element) => element.getAttribute("data-instance-id"))).toEqual(["low", "high"]);
    const headers = within(deck).getAllByRole("banner");
    expect(headers).toHaveLength(2);
    expect(headers[0].querySelector("[data-sort-designation]")).toHaveAttribute("title", "Mana value 1");
    expect(headers[1].querySelector("[data-sort-designation]")).toHaveAttribute("title", "Mana value 3");

    const sortSelect = within(deck).getByRole("combobox", { name: "Sort board" });
    expect(sortSelect).toHaveValue("cmc");
    expect(preferenceChanges).not.toHaveBeenCalled();
    sortSelect.focus();
    fireEvent.change(sortSelect, { target: { value: "color" } });
    expect(sortSelect).toHaveFocus();
    expect(preferenceChanges).toHaveBeenLastCalledWith(expect.objectContaining({
      deck: expect.objectContaining({ sort: "color" }),
    }));
    expect(screen.queryByRole("option", { name: "Rarity" })).not.toBeInTheDocument();

    const twoRows = within(deck).getByRole("button", { name: "Two rows" });
    const callsAfterSort = preferenceChanges.mock.calls.length;
    fireEvent.blur(sortSelect);
    twoRows.focus();
    expect(twoRows).toHaveFocus();
    expect(preferenceChanges).toHaveBeenCalledTimes(callsAfterSort);

    fireEvent.click(twoRows);
    expect(preferenceChanges).toHaveBeenLastCalledWith(expect.objectContaining({
      deck: expect.objectContaining({ sort: "color", rows: "two" }),
    }));
    expect(lastWorkspaceChange(workspaceChanges).placements.low.zone).toBe("deck");
    expect(lastWorkspaceChange(workspaceChanges).placements.high.zone).toBe("deck");
  });

  it("does_not_move_focus_to_the_workspace_when_pick_interaction_locks", () => {
    const cards = [card("focused")];
    const workspace = state({
      focused: { zone: "deck", row: 0, column: 0, order: 0 },
    });
    const props = {
      pool: cards,
      poolGroups: groups(cards),
      workspace,
      preferences: preferences(),
      onWorkspaceChange: vi.fn(),
      onPreferencesChange: vi.fn(),
    };
    const pickControl = document.createElement("button");
    document.body.append(pickControl);
    pickControl.focus();

    const { container, rerender } = render(<DraftWorkspace {...props} interactionLocked={false} />);
    expect(pickControl).toHaveFocus();

    rerender(<DraftWorkspace {...props} interactionLocked />);

    expect(pickControl).toHaveFocus();
    expect(container.querySelector('[aria-label="Deck workspace"] > [role="status"]'))
      .toHaveTextContent("Pick in progress.");
  });

  it("moves_deck_to_sideboard_with_destination_columns_rows_position_and_announcement", () => {
    const cards = [card("moving")];
    const workspaceChanges = vi.fn();
    const twoRowDeck = preferences({
      deck: { sort: "cmc", columnCount: 4, rows: "two", showHeaders: true },
      sideboard: { sort: "color", columnCount: 2, rows: "one", showHeaders: true },
    });
    render(
      <StatefulWorkspace
        cards={cards}
        initialWorkspace={state({ moving: { zone: "deck", row: 0, column: 3, order: 0 } })}
        initialPreferences={twoRowDeck}
        workspaceChanges={workspaceChanges}
      />,
    );

    fireEvent.keyDown(screen.getByRole("button", { name: "Inspect moving" }), {
      key: "ArrowDown",
      ctrlKey: true,
      shiftKey: true,
    });
    expect(lastWorkspaceChange(workspaceChanges).placements.moving).toEqual({
      zone: "sideboard", row: 0, column: 1, order: 0,
    });
    expect(screen.getByText("Moved moving to Sideboard, column 2, position 1.")).toBeInTheDocument();
  });

  it("moves_sideboard_to_deck_with_destination_columns_rows_position_and_announcement", () => {
    const cards = [card("moving")];
    const workspaceChanges = vi.fn();
    const asymmetric = preferences({
      deck: { sort: "cmc", columnCount: 2, rows: "two", showHeaders: true },
      sideboard: { sort: "color", columnCount: 4, rows: "one", showHeaders: true },
    });
    render(
      <StatefulWorkspace
        cards={cards}
        initialWorkspace={state({ moving: { zone: "sideboard", row: 0, column: 3, order: 0 } })}
        initialPreferences={asymmetric}
        workspaceChanges={workspaceChanges}
      />,
    );

    fireEvent.keyDown(screen.getByRole("button", { name: "Inspect moving" }), {
      key: "ArrowUp",
      ctrlKey: true,
      shiftKey: true,
    });
    expect(lastWorkspaceChange(workspaceChanges).placements.moving).toEqual({
      zone: "deck", row: 0, column: 1, order: 0,
    });
    expect(screen.getByText("Moved moving to Deck, column 2, position 1.")).toBeInTheDocument();
  });

  it("removes_actions_while_preserving_compact_movement_surfaces", () => {
    const cards = [card("moving")];
    const workspaceChanges = vi.fn();
    const { container } = render(
      <StatefulWorkspace
        cards={cards}
        initialWorkspace={state({ moving: { zone: "deck", row: 0, column: 0, order: 0 } })}
        initialPreferences={preferences({ explicitView: "compact", sideboardCollapsed: false })}
        workspaceChanges={workspaceChanges}
        responsiveLayout="phone-landscape"
      />,
    );

    expect(screen.queryByRole("button", { name: /Actions/ })).not.toBeInTheDocument();

    const initialPoolCard = container.querySelector<HTMLElement>('[aria-label="Card pool"] [data-instance-id="moving"]')!;
    fireEvent.click(within(initialPoolCard).getByRole("button", { name: "Move moving to Sideboard" }));
    expect(lastWorkspaceChange(workspaceChanges).placements.moving.zone).toBe("sideboard");

    const compactSideboard = screen.getByRole("region", { name: "Compact sideboard" });
    fireEvent.click(within(compactSideboard).getByRole("button", { name: "Move moving to Deck" }));
    expect(lastWorkspaceChange(workspaceChanges).placements.moving.zone).toBe("deck");

    const poolCard = container.querySelector<HTMLElement>('[aria-label="Card pool"] [data-instance-id="moving"]')!;
    fireEvent.click(within(poolCard).getByRole("button", { name: "Move moving to Sideboard" }));
    expect(lastWorkspaceChange(workspaceChanges).placements.moving.zone).toBe("sideboard");
  });

  it("compact_sideboard_previews_exact_printing_on_hover_and_focus_and_clears_on_leave_and_blur", () => {
    const cards = [
      card("shared-a", "Shared Name", "AAA", "1"),
      card("shared-b", "Shared Name", "BBB", "2"),
    ];
    const onCardHover = vi.fn();
    const { container } = render(
      <CompactSideboard
        pool={cards}
        poolGroups={groups(cards)}
        workspace={state({
          "shared-a": { zone: "sideboard", row: 0, column: 0, order: 0 },
          "shared-b": { zone: "sideboard", row: 0, column: 0, order: 1 },
        })}
        preferences={{ deck: preferences().deck, sideboard: preferences().sideboard }}
        onToggle={vi.fn()}
        onWorkspaceChange={vi.fn()}
        onCardHover={onCardHover}
      />,
    );
    const first = container.querySelector<HTMLElement>('[data-instance-id="shared-a"]')!;
    const second = container.querySelector<HTMLElement>('[data-instance-id="shared-b"]')!;

    fireEvent.mouseEnter(within(first).getByRole("button", { name: "Inspect Shared Name" }));
    expect(onCardHover).toHaveBeenLastCalledWith({
      name: "Shared Name",
      sourcePrinting: { setCode: "AAA", collectorNumber: "1" },
    });
    fireEvent.mouseLeave(within(first).getByRole("button", { name: "Inspect Shared Name" }));
    expect(onCardHover).toHaveBeenLastCalledWith(null);
    fireEvent.focus(within(second).getByRole("button", { name: "Inspect Shared Name" }));
    expect(onCardHover).toHaveBeenLastCalledWith({
      name: "Shared Name",
      sourcePrinting: { setCode: "BBB", collectorNumber: "2" },
    });
    fireEvent.blur(within(second).getByRole("button", { name: "Inspect Shared Name" }));
    expect(onCardHover).toHaveBeenLastCalledWith(null);
  });

  it("places_the_count_and_accessible_down_toggle_in_the_collapsed_slot_header", () => {
    const cards = [card("side", "Side Card")];
    const props = {
      pool: cards,
      poolGroups: groups(cards),
      workspace: state({ side: { zone: "sideboard", row: 0, column: 0, order: 0 } }),
      preferences: preferences({ sideboardCollapsed: true }),
      onWorkspaceChange: vi.fn(),
      onPreferencesChange: vi.fn(),
    };
    const { rerender } = render(<DraftWorkspace {...props} />);
    const collapsed = screen.getByRole("region", { name: "Compact sideboard" });
    const count = within(collapsed).getByRole("heading", { name: "Sideboard (1 card)" });
    const toggle = within(collapsed).getByRole("button", { name: "Show sideboard (1 card)" });
    expect(count.closest("header")).toBe(toggle.closest("header"));
    expect(toggle).toHaveAttribute("title", "Show sideboard (1 card)");
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(toggle.querySelector("[aria-hidden='true']")).toBeInTheDocument();
    expect(toggle).toBeEnabled();
    expect(within(collapsed).queryByRole("combobox", { name: "Sort board" })).not.toBeInTheDocument();
    expect(within(collapsed).queryByRole("group", { name: "Board rows" })).not.toBeInTheDocument();
    expect(within(collapsed).queryByRole("group", { name: "Board columns" })).not.toBeInTheDocument();
    expect(within(collapsed).queryByRole("checkbox", { name: "Show headers" })).not.toBeInTheDocument();
    expect(within(collapsed).queryByRole("toolbar", { name: "Board layout" })).not.toBeInTheDocument();
    expect(collapsed.querySelector("[data-row-headers], header[aria-label^='Column ']"))
      .not.toBeInTheDocument();

    rerender(<DraftWorkspace {...props} interactionLocked />);
    expect(within(collapsed).getByRole("button", { name: "Show sideboard (1 card)" })).toBeDisabled();
    expect(within(collapsed).getByRole("button", { name: "Inspect Side Card" })).toBeDisabled();
    expect(within(collapsed).getByRole("button", { name: "Move Side Card to Deck" })).toBeDisabled();

    rerender(<DraftWorkspace {...props} preferences={preferences({ sideboardCollapsed: false })} />);
    expect(screen.queryByRole("region", { name: "Compact sideboard" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Show sideboard (1 card)" })).not.toBeInTheDocument();
  });

  it("preserves_collapsed_sideboard_registration_drop_attributes_and_active_styling", () => {
    const registerCollapsedSideboard = vi.fn();
    const registerSideboard = vi.fn();
    let activeTarget: DraftWorkspaceDragController["activeTarget"] = {
      zone: "sideboard",
      column: 0,
      row: null,
    };
    const controller: DraftWorkspaceDragController = {
      announcement: "", get activeTarget() { return activeTarget; },
      dragPreview: null,
      handlePointerDown: vi.fn(), handleWorkspacePointerDown: vi.fn(),
      handlePointerMove: vi.fn(), handlePointerUp: vi.fn(),
      handlePointerCancel: vi.fn(), handleLostPointerCapture: vi.fn(),
      consumeCompatibilityActivation: vi.fn(() => false),
      registerBoard: vi.fn((zone: "deck" | "sideboard") => (
        zone === "sideboard" ? registerSideboard : vi.fn()
      )),
      registerColumn: vi.fn(() => vi.fn()), registerCollapsedSideboard,
      dropState: vi.fn(() => ({ zoneActive: false, column: null, row: null })),
      invalidateGeometry: vi.fn(), dispose: vi.fn(),
    };
    const props = {
      pool: [] as DraftCardInstance[], poolGroups: groups([]), workspace: state({}),
      preferences: preferences({ sideboardCollapsed: true }), dragController: controller,
      onWorkspaceChange: vi.fn(), onPreferencesChange: vi.fn(),
    };
    const { container, rerender } = render(<DraftWorkspace {...props} />);
    const target = container.querySelector<HTMLElement>('[data-drop-target="collapsed-sideboard"]')!;
    expect(registerCollapsedSideboard).toHaveBeenCalledWith(target);
    expect(target).toHaveAttribute("data-drop-state", "active");
    // The highlight must not alter the target's own box, or the drop test flickers.
    expect(target).not.toHaveClass("border", "border-dashed", "border-amber-300");
    const highlight = target.querySelector<HTMLElement>('[data-drop-highlight="active"]')!;
    expect(highlight).toBeInTheDocument();
    expect(highlight).toHaveClass("pointer-events-none", "absolute", "inset-0");
    // The highlight frames the card slot only, so it must sit outside the panel header.
    expect(highlight.closest("header")).toBeNull();
    expect(highlight.parentElement).toBe(
      target.querySelector("[data-card-height-baseline]")!.parentElement,
    );

    activeTarget = null;
    rerender(<DraftWorkspace {...props} />);
    expect(target).toHaveAttribute("data-drop-state", "idle");
    expect(target.querySelector('[data-drop-highlight="active"]')).not.toBeInTheDocument();

    rerender(<DraftWorkspace {...props} preferences={preferences({ sideboardCollapsed: false })} />);
    expect(container.querySelector('[data-drop-target="collapsed-sideboard"]')).not.toBeInTheDocument();
    expect(registerCollapsedSideboard.mock.calls.some(([element]) => element === null)).toBe(true);
    expect(registerSideboard).toHaveBeenCalledWith(expect.any(HTMLElement));
  });

  it("renders_one_collapsed_sideboard_slot_without_duplicate_identities", () => {
    const cards = [
      card("shared-a", "Shared Name", "AAA", "1"),
      card("shared-b", "Shared Name", "BBB", "2"),
    ];
    const { container } = render(
      <DraftWorkspace
        pool={cards}
        poolGroups={groups(cards)}
        workspace={state({
          "shared-a": { zone: "sideboard", row: 0, column: 0, order: 0 },
          "shared-b": { zone: "sideboard", row: 0, column: 1, order: 0 },
          basic: { zone: "sideboard", row: 0, column: 1, order: 1 },
        }, [{ instanceId: "basic", name: "Island" }])}
        preferences={preferences({ sideboardCollapsed: true })}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );
    expect(container.querySelectorAll('[aria-label="Compact sideboard"]')).toHaveLength(1);
    for (const instanceId of ["shared-a", "shared-b", "basic"]) {
      expect(container.querySelectorAll(`[data-instance-id="${instanceId}"]`)).toHaveLength(1);
    }
  });

  it("preserves_collapsed_printing_preview_blur_and_truthful_drafted_and_virtual_actions", () => {
    const cards = [
      card("shared-a", "Shared Name", "AAA", "1"),
      card("shared-b", "Shared Name", "BBB", "2"),
    ];
    const onCardHover = vi.fn();
    render(
      <StatefulWorkspace
        cards={cards}
        initialWorkspace={state({
          "shared-a": { zone: "sideboard", row: 0, column: 0, order: 0 },
          "shared-b": { zone: "sideboard", row: 0, column: 0, order: 1 },
          basic: { zone: "sideboard", row: 0, column: 0, order: 2 },
        }, [{ instanceId: "basic", name: "Island" }])}
        initialPreferences={preferences({ sideboardCollapsed: true })}
        onCardHover={onCardHover}
      />,
    );
    const collapsed = screen.getByRole("region", { name: "Compact sideboard" });
    const sharedCards = within(collapsed).getAllByRole("button", { name: "Inspect Shared Name" });
    fireEvent.mouseEnter(sharedCards[0]);
    expect(onCardHover).toHaveBeenLastCalledWith({
      name: "Shared Name", sourcePrinting: { setCode: "AAA", collectorNumber: "1" },
    });
    fireEvent.blur(sharedCards[1]);
    expect(onCardHover).toHaveBeenLastCalledWith(null);
    fireEvent.focus(sharedCards[1]);
    expect(onCardHover).toHaveBeenLastCalledWith({
      name: "Shared Name", sourcePrinting: { setCode: "BBB", collectorNumber: "2" },
    });
    fireEvent.mouseLeave(sharedCards[1]);
    expect(onCardHover).toHaveBeenLastCalledWith(null);
    expect(within(collapsed).getAllByRole("button", { name: "Move Shared Name to Deck" }))
      .toHaveLength(2);
    expect(within(collapsed).getByRole("button", { name: "Remove Island" })).toBeInTheDocument();
    expect(within(collapsed).queryByRole("button", { name: "Move Island to Deck" }))
      .not.toBeInTheDocument();
  });

  it("keeps_collapsed_cards_native_keyboard_only_while_supporting_drag", () => {
    const cards = [card("side")];
    const workspaceChanges = vi.fn();
    const controller = {
      announcement: "", activeTarget: null, dragPreview: null,
      handlePointerDown: vi.fn(), handleWorkspacePointerDown: vi.fn(),
      handlePointerMove: vi.fn(), handlePointerUp: vi.fn(), handlePointerCancel: vi.fn(),
      handleLostPointerCapture: vi.fn(), consumeCompatibilityActivation: vi.fn(() => false),
      registerBoard: vi.fn(() => vi.fn()), registerColumn: vi.fn(() => vi.fn()),
      registerCollapsedSideboard: vi.fn(),
      dropState: vi.fn(() => ({ zoneActive: false, column: null, row: null })),
      invalidateGeometry: vi.fn(), dispose: vi.fn(),
    } satisfies DraftWorkspaceDragController;
    render(
      <StatefulWorkspace
        cards={cards}
        initialWorkspace={state({ side: { zone: "sideboard", row: 0, column: 0, order: 0 } })}
        initialPreferences={preferences({ sideboardCollapsed: true })}
        workspaceChanges={workspaceChanges}
        dragController={controller}
      />,
    );
    const cardButton = screen.getByRole("button", { name: "Inspect side" });
    for (const key of ["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"]) {
      fireEvent.keyDown(cardButton, { key, ctrlKey: true, shiftKey: true });
    }
    fireEvent.pointerDown(cardButton, { pointerId: 1, pointerType: "mouse" });
    expect(workspaceChanges).not.toHaveBeenCalled();
    expect(controller.handleWorkspacePointerDown).toHaveBeenCalledWith(
      expect.anything(),
      expect.objectContaining({ kind: "workspace", instanceIds: ["side"] }),
    );
    fireEvent.click(cardButton);
    expect(lastWorkspaceChange(workspaceChanges).placements.side.zone).toBe("deck");
    controller.consumeCompatibilityActivation.mockReturnValue(true);
    fireEvent.doubleClick(screen.getByRole("button", { name: "Inspect side" }), { detail: 2 });
    expect(controller.consumeCompatibilityActivation).toHaveBeenLastCalledWith(expect.objectContaining({
      kind: "double-click",
      detail: 2,
      pointerId: null,
      surface: "workspace",
      sourceInstanceId: "side",
    }));
  });

  it("returns_collapsed_sideboard_cards_to_the_sorted_deck_column", () => {
    const cards = [card("side", "side", "TST", "side", 3)];
    const workspaceChanges = vi.fn();
    render(
      <StatefulWorkspace
        cards={cards}
        initialWorkspace={state({ side: { zone: "sideboard", row: 0, column: 0, order: 0 } })}
        initialPreferences={preferences({ sideboardCollapsed: true })}
        workspaceChanges={workspaceChanges}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Move side to Deck" }));

    expect(lastWorkspaceChange(workspaceChanges).placements.side)
      .toMatchObject({ zone: "deck", column: 3 });
  });

  it("moves_collapsed_sideboard_cards_into_the_dropped_deck_column", () => {
    const cards = [card("side")];
    const workspaceChanges = vi.fn();
    const controller = {
      announcement: "", activeTarget: null, dragPreview: null,
      handlePointerDown: vi.fn(), handleWorkspacePointerDown: vi.fn(),
      handlePointerMove: vi.fn(), handlePointerUp: vi.fn(), handlePointerCancel: vi.fn(),
      handleLostPointerCapture: vi.fn(), consumeCompatibilityActivation: vi.fn(() => false),
      registerBoard: vi.fn(() => vi.fn()), registerColumn: vi.fn(() => vi.fn()),
      registerCollapsedSideboard: vi.fn(),
      dropState: vi.fn(() => ({ zoneActive: false, column: null, row: null })),
      invalidateGeometry: vi.fn(), dispose: vi.fn(),
    } satisfies DraftWorkspaceDragController;
    render(
      <StatefulWorkspace
        cards={cards}
        initialWorkspace={state({ side: { zone: "sideboard", row: 0, column: 0, order: 0 } })}
        initialPreferences={preferences({ sideboardCollapsed: true })}
        workspaceChanges={workspaceChanges}
        dragController={controller}
      />,
    );

    fireEvent.pointerDown(screen.getByRole("button", { name: "Inspect side" }), {
      pointerId: 1,
      pointerType: "mouse",
    });
    const source = controller.handleWorkspacePointerDown.mock.calls[0]?.[1] as WorkspaceDragSource;

    expect(source.onDrop({ zone: "deck", column: 1 })).toBe(true);
    expect(lastWorkspaceChange(workspaceChanges).placements.side)
      .toMatchObject({ zone: "deck", column: 1 });
  });

  it("keeps_the_complete_expanded_sideboard_toolbar_with_the_toggle_last", () => {
    const cards = [card("side")];
    const props = {
      pool: cards,
      poolGroups: groups(cards),
      workspace: state({ side: { zone: "sideboard", row: 0, column: 0, order: 0 } }),
      preferences: preferences({ sideboardCollapsed: false }),
      onWorkspaceChange: vi.fn(),
      onPreferencesChange: vi.fn(),
    };
    const { container, rerender } = render(<DraftWorkspace {...props} />);
    const sideboard = container.querySelector<HTMLElement>('section[aria-label="Sideboard"]')!;
    const toolbar = within(sideboard).getByRole("toolbar", { name: "Board layout" });
    const toggle = within(toolbar).getByRole("button", { name: "Hide sideboard" });
    expect(toolbar.firstElementChild).toBe(
      within(toolbar).getByRole("heading", { name: "Sideboard (1 card)" }),
    );
    expect(toolbar.lastElementChild).toBe(toggle);
    expect(toggle).toHaveAttribute("title", "Hide sideboard");
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(toggle.querySelector("[aria-hidden='true']")).toBeInTheDocument();
    expect(within(toolbar).getByRole("combobox", { name: "Sort board" })).toBeInTheDocument();
    expect(within(toolbar).getByRole("group", { name: "Board rows" })).toBeInTheDocument();
    expect(within(toolbar).getByRole("group", { name: "Board columns" })).toBeInTheDocument();
    expect(within(toolbar).getByRole("checkbox", { name: "Show headers" })).toBeInTheDocument();
    const deck = container.querySelector<HTMLElement>('section[aria-label="Deck"]')!;
    expect(within(deck).queryByRole("button", { name: "Hide sideboard" })).not.toBeInTheDocument();

    rerender(<DraftWorkspace {...props} interactionLocked />);
    expect(within(sideboard).getByRole("button", { name: "Hide sideboard" })).toBeDisabled();
    expect(within(sideboard).getByRole("combobox", { name: "Sort board" })).toBeDisabled();
  });
});
