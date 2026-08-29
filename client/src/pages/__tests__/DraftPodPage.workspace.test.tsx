// @vitest-environment happy-dom

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { WorkspaceDeckBuilderController } from "../../components/draft/LimitedDeckBuilder";
import type { PackDisplayController, PackDisplayPresentation } from "../../components/draft/PackDisplay";
import type { DraftShellPhoneAction } from "../../components/chrome/ShellContext";
import { DRAFT_WORKSPACE_PREFERENCES_KEY } from "../../constants/storage";
import type { DraftWorkspaceProps } from "../../components/draft/workspace/DraftWorkspace";
import type { ResponsiveDraftLayout } from "../../components/draft/workspace/workspacePreferences";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { DraftPodPage } from "../DraftPodPage";

const captured = vi.hoisted(() => ({
  pack: null as PackDisplayController | null,
  workspace: null as DraftWorkspaceProps | null,
  deckbuilder: null as WorkspaceDeckBuilderController | null,
  builderLayout: null as ResponsiveDraftLayout | null,
  previews: [] as Array<{ mode?: string; hoverDelayMs?: number }>,
  menuShell: null as { layout?: string; contentWidthClass?: string; compactTopPadding?: boolean } | null,
  presentation: null as PackDisplayPresentation | null,
  packLayout: null as ResponsiveDraftLayout | null,
  phoneToolbarPinned: null as boolean | null,
  mobileWorkspaceOpen: null as boolean | null,
  shellMode: null as string | null,
  phoneAction: undefined as DraftShellPhoneAction | undefined,
  progressVariant: null as string | null,
  showProgress: null as boolean | null,
  hostPresentations: [] as string[],
}));

const store = vi.hoisted(() => {
  const cards = [
    { instance_id: "copy-z", name: "Shared", set_code: "TST", collector_number: "9", rarity: "common", colors: [], cmc: 1, type_line: "Card" },
    { instance_id: "copy-a", name: "Shared", set_code: "TST", collector_number: "1", rarity: "mythic", colors: [], cmc: 1, type_line: "Card" },
  ];
  const view = {
    status: "Drafting", kind: "Premier", pool: cards, current_pack: cards, draft_effects: [],
    pool_groups: {
      color_groups: [], type_groups: [], cmc_groups: [], rarity_groups: [],
      type_filter_options: [], color_filter_options: [],
      color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
      workspace_capabilities: { rarity_group_order: null },
      workspace_row_classification: { creature_instance_ids: [], noncreature_instance_ids: [] },
    },
    seats: [], current_pack_number: 1, pick_number: 1, pass_direction: "Left",
    cards_per_pack: 2, pack_count: 3, min_deck_size: 1, addable_cards: [],
    timer_remaining_ms: null, standings: [], current_round: 0,
    tournament_format: "Swiss", pod_policy: "Casual", pairings: [], match_config: { match_type: "Bo1" },
  };
  const state = {
    phase: "drafting",
    view,
    workspaceState: {
      schemaVersion: 1,
      placements: {
        "copy-z": { zone: "deck", row: 0, column: 0, order: 0 },
        "copy-a": { zone: "sideboard", row: 0, column: 0, order: 0 },
      },
      virtualBasics: [],
    },
    selectedCard: null,
    pendingPickIntent: null,
    interactionGeneration: 7,
    pickInteractionLocked: false,
    paused: false,
    pauseReason: null,
    sideboardPrompt: null,
    playDrawPrompt: null,
    sideboardSubmitted: false,
    intergameWorkspaceState: null,
    standings: [],
    pairings: [],
    error: null,
    selectCard: vi.fn(),
    submitPick: vi.fn(async () => ({ status: "acknowledged" as const })),
    submitPickStep: vi.fn(async () => ({ status: "acknowledged" as const })),
    confirmPick: vi.fn(async () => ({ status: "acknowledged" as const })),
    submitPickWithDraftEffect: vi.fn(async () => ({ status: "acknowledged" as const })),
    autoPickCard: vi.fn(async () => ({ status: "acknowledged" as const })),
    setWorkspaceState: vi.fn(),
    addBasicLand: vi.fn(),
    removeBasicLand: vi.fn(),
    submitDeck: vi.fn(),
    leave: vi.fn(),
    resumeDraft: vi.fn(async () => "absent" as const),
  };
  return { state };
});

vi.mock("../../stores/multiplayerDraftStore", () => {
  const hook = Object.assign(
    (selector: (state: typeof store.state) => unknown) => selector(store.state),
    {
      getState: () => store.state,
      subscribe: () => vi.fn(),
    },
  );
  return {
    useMultiplayerDraftStore: hook,
    draftPodScreen: (state: typeof store.state) => state.phase,
    intergamePromptKey: () => null,
  };
});

vi.mock("../../stores/draftPodStore", () => ({
  useDraftPodStore: (selector: (state: { config: { podSize: number }; reset: () => void; resumeHostedPod: () => void; enterKind: () => void }) => unknown) => selector({
    config: { podSize: 8 }, reset: vi.fn(), resumeHostedPod: vi.fn(), enterKind: vi.fn(),
  }),
}));
vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/chrome/ShellContext", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../components/chrome/ShellContext")>(),
  useDraftShellChrome: (mode: string, phoneAction?: DraftShellPhoneAction, progressVariant?: string, showProgress?: boolean) => {
    captured.shellMode = mode;
    captured.phoneAction = phoneAction;
    captured.progressVariant = progressVariant ?? "quick";
    captured.showProgress = showProgress ?? true;
  },
}));
vi.mock("../../components/menu/MenuShell", () => ({ MenuShell: (props: { children: ReactNode; layout?: string; contentWidthClass?: string; compactTopPadding?: boolean }) => {
  captured.menuShell = props;
  return <>{props.children}</>;
} }));
vi.mock("../../components/draft/HostControls", () => ({ HostControls: ({ presentation = "floating" }: { presentation?: string }) => {
  captured.hostPresentations.push(presentation);
  return <div data-testid={`host-controls-${presentation}`} />;
} }));
vi.mock("../../components/draft/SeatStatusRing", () => ({ SeatStatusRing: () => <div data-testid="seat-status-ring" /> }));
vi.mock("../../components/draft/PickTimer", () => ({ PickTimer: () => null }));
vi.mock("../../components/draft/DraftProgress", () => ({ DraftProgress: () => null }));
vi.mock("../../components/modal/DialogShell", () => ({
  DialogShell: ({ title, children, onClose }: { title: ReactNode; children: ReactNode; onClose(): void }) => (
    <div role="dialog" aria-label={String(title)}>
      {children}
      <button type="button" onClick={onClose}>Close pod status</button>
    </div>
  ),
}));
vi.mock("../../components/card/HoverCardPreview", () => ({
  HoverCardPreview: (props: { mode?: string; hoverDelayMs?: number }) => {
    captured.previews.push(props);
    return null;
  },
}));
vi.mock("../../components/draft/DraftIntro", () => ({
  DraftIntro: ({ onContinue }: { onContinue(): void }) => <button onClick={onContinue}>Continue</button>,
}));
vi.mock("../../components/draft/PackDisplay", () => ({
  PackDisplay: ({ controller, presentation, responsiveLayout, phoneToolbarPinned, mobileWorkspaceOpen }: { controller: PackDisplayController; presentation: PackDisplayPresentation; responsiveLayout?: ResponsiveDraftLayout; phoneToolbarPinned?: boolean; mobileWorkspaceOpen?: boolean }) => {
    captured.pack = controller;
    captured.presentation = presentation;
    captured.packLayout = responsiveLayout ?? null;
    captured.phoneToolbarPinned = phoneToolbarPinned ?? false;
    captured.mobileWorkspaceOpen = mobileWorkspaceOpen ?? false;
    return <div data-testid="pack">{controller.view?.current_pack?.map((card) => card.instance_id).join(",")}</div>;
  },
}));
vi.mock("../../components/draft/workspace/DraftWorkspace", () => ({
  DraftWorkspace: (props: DraftWorkspaceProps) => {
    captured.workspace = props;
    const phoneLayout = props.responsiveLayout === "phone-portrait" || props.responsiveLayout === "phone-landscape";
    return <div data-testid="workspace">{phoneLayout ? props.mobileSummaryAccessory : null}</div>;
  },
}));
vi.mock("../../components/draft/LimitedDeckBuilder", () => ({
  LimitedDeckBuilder: ({ local, responsiveLayout }: { local?: WorkspaceDeckBuilderController; responsiveLayout?: ResponsiveDraftLayout }) => {
    captured.deckbuilder = local ?? null;
    captured.builderLayout = responsiveLayout ?? null;
    return <div data-testid="deckbuilder" />;
  },
}));

describe("DraftPodPage workspace", () => {
  beforeEach(() => {
    captured.pack = null;
    captured.workspace = null;
    captured.deckbuilder = null;
    captured.builderLayout = null;
    captured.previews = [];
    captured.menuShell = null;
    captured.presentation = null;
    captured.packLayout = null;
    captured.phoneToolbarPinned = null;
    captured.mobileWorkspaceOpen = null;
    captured.shellMode = null;
    captured.phoneAction = undefined;
    captured.progressVariant = null;
    captured.showProgress = null;
    captured.hostPresentations = [];
    store.state.phase = "drafting";
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1440 });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 900 });
    usePreferencesStore.setState({ draftCardPreviewMode: "none", draftDoubleClickConfirmPick: true });
    localStorage.clear();
    vi.clearAllMocks();
  });

  afterEach(() => {
    cleanup();
    localStorage.clear();
  });

  it("pod_set_and_cube_views_use_same_workspace_and_controller_contract", async () => {
    usePreferencesStore.setState({ draftCardPreviewMode: "shift", draftDoubleClickConfirmPick: false });
    const rendered = render(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(screen.getByTestId("pack")).toHaveTextContent("copy-z,copy-a");
    expect(captured.pack?.kind).toBe("local-workspace");
    expect(captured.pack).toMatchObject({ doubleClickPick: false });
    expect(captured.workspace?.workspace.placements).toHaveProperty("copy-z");
    expect(captured.workspace?.workspace.placements).toHaveProperty("copy-a");
    expect(captured.previews[captured.previews.length - 1])
      .toMatchObject({ mode: "shift", hoverDelayMs: 0 });
    if (captured.pack?.kind !== "local-workspace") throw new Error("workspace controller not installed");
    await captured.pack.pickCard("copy-a", "sideboard", { column: 4 });
    expect(store.state.submitPick).toHaveBeenCalledWith("copy-a", "sideboard", { column: 4 });
    await captured.pack.autoPickCard();
    expect(store.state.autoPickCard).toHaveBeenCalledWith({
      "copy-z": { column: 1 },
      "copy-a": { column: 1 },
    });

    act(() => { store.state.phase = "deckbuilding"; });
    rendered.rerender(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    expect(screen.getByTestId("deckbuilder")).toBeInTheDocument();
    expect(captured.deckbuilder?.workspace).toBe(store.state.workspaceState);
    expect(captured.deckbuilder?.capabilities).toEqual({ kind: "editable-pool", suggestions: false });
  });

  it("forwards explicit preview settings in match pool review", () => {
    usePreferencesStore.setState({ draftCardPreviewMode: "follow" });
    store.state.phase = "matchInProgress";

    render(<MemoryRouter><DraftPodPage /></MemoryRouter>);

    expect(captured.previews[captured.previews.length - 1])
      .toMatchObject({ mode: "follow", hoverDelayMs: 0 });
  });

  it("uses_the_full_width_shell_and_repairs_the_pod_page_scale_setter", async () => {
    render(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    expect(captured.menuShell).toMatchObject({ layout: "stacked", contentWidthClass: "max-w-none" });
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

  it.each([
    ["phone-portrait", 430, 932, "h-[calc(100dvh_-_11rem)]"],
    ["phone-landscape", 924, 412, "h-[calc(100dvh_-_4rem)]"],
  ] as const)("uses_quick_draft_phone_contracts_on_%s", (responsiveLayout, width, height, heightClass) => {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: height });

    const rendered = render(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(captured.shellMode).toBe("phone-drafting");
    expect(captured.progressVariant).toBe("pod");
    expect(captured.showProgress).toBe(responsiveLayout === "phone-landscape");
    expect(captured.menuShell).toMatchObject({ compactTopPadding: true });
    expect(captured.packLayout).toBe(responsiveLayout);
    expect(captured.phoneToolbarPinned).toBe(true);
    expect(captured.workspace).toMatchObject({
      responsiveLayout,
      mobileOverlay: true,
      mobileWorkspaceOpen: false,
    });
    expect(document.querySelector(`[data-responsive-draft-layout="${responsiveLayout}"]`)).toHaveClass(heightClass);
    expect(screen.queryByTestId("seat-status-ring")).not.toBeInTheDocument();
    expect(captured.phoneAction?.label).toBe("Pod Draft in Progress");
    expect(captured.hostPresentations).toEqual(["integrated"]);
    expect(screen.getByTestId("host-controls-integrated")).toBeInTheDocument();
    expect(screen.queryByTestId("host-controls-floating")).not.toBeInTheDocument();

    act(() => captured.phoneAction?.onClick());
    expect(screen.getByRole("dialog", { name: "Pod Draft in Progress" })).toBeInTheDocument();
    expect(screen.getByTestId("seat-status-ring")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Close pod status" }));
    expect(screen.queryByRole("dialog", { name: "Pod Draft in Progress" })).not.toBeInTheDocument();

    act(() => captured.workspace?.onMobileWorkspaceOpenChange?.(true));
    expect(captured.phoneToolbarPinned).toBe(false);
    expect(captured.mobileWorkspaceOpen).toBe(true);

    act(() => { store.state.phase = "deckbuilding"; });
    rendered.rerender(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    expect(captured.shellMode).toBe("phone-deckbuilding");
    expect(captured.phoneAction).toBeUndefined();
    expect(captured.builderLayout).toBe(responsiveLayout);
  });

  it.each([
    ["tablet-portrait", 768, 1024],
    ["tablet-landscape", 1024, 768],
  ] as const)("uses_quick_draft_tablet_contracts_on_%s", (responsiveLayout, width, height) => {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: height });

    const rendered = render(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(captured.shellMode).toBe("tablet-drafting");
    expect(captured.progressVariant).toBe("pod");
    expect(captured.phoneAction).toBeUndefined();
    expect(captured.packLayout).toBe(responsiveLayout);
    expect(captured.workspace?.responsiveLayout).toBe(responsiveLayout);
    expect(captured.menuShell).toMatchObject({ compactTopPadding: false });
    expect(screen.getByTestId("seat-status-ring")).toBeInTheDocument();

    act(() => { store.state.phase = "deckbuilding"; });
    rendered.rerender(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    expect(captured.shellMode).toBe("tablet-deckbuilding");
    expect(captured.progressVariant).toBe("pod");
    expect(captured.showProgress).toBe(true);
    expect(captured.builderLayout).toBe(responsiveLayout);
    expect(captured.menuShell).toMatchObject({ compactTopPadding: true });
  });
  });
