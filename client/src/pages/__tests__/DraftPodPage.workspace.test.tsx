// @vitest-environment happy-dom

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { WorkspaceDeckBuilderController } from "../../components/draft/LimitedDeckBuilder";
import type { PackDisplayController, PackDisplayPresentation } from "../../components/draft/PackDisplay";
import type { DraftShellPhoneAction, DraftShellTopAction } from "../../components/chrome/ShellContext";
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
  topActions: [] as readonly DraftShellTopAction[],
  hostActionsEnabled: [] as boolean[],
  hostEndActions: [] as DraftShellTopAction[],
  floatingActions: [] as readonly DraftShellTopAction[],
  floatingEndAction: undefined as DraftShellTopAction | undefined,
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
    role: "host",
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

const podStore = vi.hoisted(() => ({
  reset: vi.fn(),
  resumeHostedPod: vi.fn(),
  enterKind: vi.fn(),
}));

const router = vi.hoisted(() => ({ navigate: vi.fn() }));

vi.mock("../../stores/multiplayerDraftStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../stores/multiplayerDraftStore")>();
  const hook = Object.assign(
    (selector: (state: typeof store.state) => unknown) => selector(store.state),
    {
      getState: () => store.state,
      subscribe: () => vi.fn(),
    },
  );
  return {
    ...actual,
    useMultiplayerDraftStore: hook,
    draftPodScreen: (state: typeof store.state) => state.phase,
    intergamePromptKey: () => null,
  };
});

vi.mock("../../stores/draftPodStore", () => ({
  useDraftPodStore: (selector: (state: { config: { podSize: number }; reset: () => void; resumeHostedPod: () => void; enterKind: () => void }) => unknown) => selector({
    config: { podSize: 8 }, ...podStore,
  }),
}));
vi.mock("react-router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-router")>()),
  useNavigate: () => router.navigate,
}));
vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/chrome/ShellContext", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../components/chrome/ShellContext")>(),
  useDraftShellChrome: (mode: string, phoneAction?: DraftShellPhoneAction, progressVariant?: string, showProgress?: boolean, topActions?: readonly DraftShellTopAction[]) => {
    captured.shellMode = mode;
    captured.phoneAction = phoneAction;
    captured.progressVariant = progressVariant ?? "quick";
    captured.showProgress = showProgress ?? true;
    captured.topActions = topActions ?? [];
  },
}));
vi.mock("../../components/menu/MenuShell", () => ({ MenuShell: (props: { children: ReactNode; layout?: string; contentWidthClass?: string; compactTopPadding?: boolean }) => {
  captured.menuShell = props;
  return <>{props.children}</>;
} }));
vi.mock("../../components/draft/HostControls", () => ({
  HostControls: ({
    draftTopActions,
    endDraftAction,
  }: {
    draftTopActions: readonly DraftShellTopAction[];
    endDraftAction: DraftShellTopAction;
  }) => {
    captured.hostPresentations.push("floating");
    captured.floatingActions = draftTopActions;
    captured.floatingEndAction = endDraftAction;
    if (store.state.role !== "host") return null;
    const action = draftTopActions.find(({ id }) => id === "end-draft") ?? endDraftAction;
    return (
      <button
        data-testid="host-controls-floating"
        disabled={action.disabled}
        onClick={action.onClick}
      >
        {action.label}
      </button>
    );
  },
  useHostDraftTopActions: ({
    enabled,
    endDraftAction,
  }: {
    enabled: boolean;
    endDraftAction: DraftShellTopAction;
  }): readonly DraftShellTopAction[] => {
    captured.hostActionsEnabled.push(enabled);
    captured.hostEndActions.push(endDraftAction);
    return enabled && store.state.role === "host" ? [
      { id: "pause-resume", label: "Pause Draft", tone: "neutral", onClick: vi.fn() },
      endDraftAction,
    ] : [];
  },
}));
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
    return <div data-testid="workspace" />;
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
    captured.topActions = [];
    captured.hostActionsEnabled = [];
    captured.hostEndActions = [];
    captured.floatingActions = [];
    captured.floatingEndAction = undefined;
    captured.progressVariant = null;
    captured.showProgress = null;
    captured.hostPresentations = [];
    store.state.phase = "drafting";
    store.state.role = "host";
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1440 });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 900 });
    usePreferencesStore.setState({ draftCardPreviewMode: "none", draftDoubleClickConfirmPick: true });
    localStorage.clear();
    vi.clearAllMocks();
    store.state.leave.mockReset();
    store.state.leave.mockResolvedValue(undefined);
  });

  afterEach(() => {
    cleanup();
    localStorage.clear();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
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
    expect(captured.workspace).not.toHaveProperty("mobileSummaryAccessory");
    expect(captured.workspace).not.toHaveProperty("tabletSideboardAccessory");
    expect(document.querySelector(`[data-responsive-draft-layout="${responsiveLayout}"]`)).toHaveClass(heightClass);
    expect(screen.queryByTestId("seat-status-ring")).not.toBeInTheDocument();
    expect(captured.phoneAction?.label).toBe("Pod Draft in Progress");
    expect(captured.topActions.map((action) => action.id)).toEqual(["pause-resume", "end-draft"]);
    expect(captured.hostActionsEnabled[captured.hostActionsEnabled.length - 1]).toBe(true);
    expect(captured.hostPresentations).toEqual([]);
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
    expect(captured.hostActionsEnabled[captured.hostActionsEnabled.length - 1]).toBe(false);
    expect(captured.topActions).toEqual([]);
    expect(screen.getByTestId("host-controls-floating")).toBeInTheDocument();
  });

  it.each([
    ["tablet-portrait", 768, 1024, true],
    ["tablet-landscape", 1024, 768, true],
  ] as const)("uses_the_correct_host_controls_presentation_on_%s", (responsiveLayout, width, height, useCompactHostControls) => {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: height });

    const rendered = render(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(captured.shellMode).toBe("tablet-drafting");
    expect(captured.progressVariant).toBe("pod");
    expect(captured.phoneAction?.label).toBe("Pod Draft in Progress");
    expect(captured.packLayout).toBe(responsiveLayout);
    expect(captured.workspace?.responsiveLayout).toBe(responsiveLayout);
    expect(captured.workspace).not.toHaveProperty("mobileSummaryAccessory");
    expect(captured.workspace).not.toHaveProperty("tabletSideboardAccessory");
    expect(captured.menuShell).toMatchObject({ compactTopPadding: false });
    expect(screen.queryByTestId("seat-status-ring")).not.toBeInTheDocument();
    expect(captured.topActions.map((action) => action.id)).toEqual(
      useCompactHostControls ? ["pause-resume", "end-draft"] : [],
    );
    expect(captured.topActions[1]).toBe(captured.hostEndActions[captured.hostEndActions.length - 1]);
    expect(captured.hostActionsEnabled[captured.hostActionsEnabled.length - 1]).toBe(useCompactHostControls);
    expect(captured.hostPresentations).toEqual(useCompactHostControls ? [] : ["floating"]);
    if (useCompactHostControls) {
      expect(screen.queryByTestId("host-controls-floating")).not.toBeInTheDocument();
    } else {
      expect(screen.getByTestId("host-controls-floating")).toBeInTheDocument();
    }

    act(() => captured.phoneAction?.onClick());
    expect(screen.getByRole("dialog", { name: "Pod Draft in Progress" })).toBeInTheDocument();
    expect(screen.getByTestId("seat-status-ring")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Close pod status" }));

    act(() => { store.state.role = "guest"; });
    rendered.rerender(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    expect(captured.hostActionsEnabled[captured.hostActionsEnabled.length - 1]).toBe(useCompactHostControls);
    expect(captured.topActions).toEqual([]);
    expect(captured.phoneAction?.label).toBe("Pod Draft in Progress");
    if (useCompactHostControls) {
      expect(screen.queryByTestId("host-controls-floating")).not.toBeInTheDocument();
    } else {
      expect(screen.getByTestId("host-controls-floating")).toBeInTheDocument();
    }

    act(() => {
      store.state.role = "host";
      store.state.phase = "deckbuilding";
    });
    rendered.rerender(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    expect(captured.shellMode).toBe("tablet-deckbuilding");
    expect(captured.progressVariant).toBe("pod");
    expect(captured.showProgress).toBe(true);
    expect(captured.builderLayout).toBe(responsiveLayout);
    expect(captured.menuShell).toMatchObject({ compactTopPadding: true });
    expect(captured.hostActionsEnabled[captured.hostActionsEnabled.length - 1]).toBe(false);
    expect(captured.topActions).toEqual([]);
    expect(captured.phoneAction).toBeUndefined();
    expect(screen.queryByRole("dialog", { name: "Pod Draft in Progress" })).not.toBeInTheDocument();
    expect(screen.getByTestId("host-controls-floating")).toBeInTheDocument();
  });

  it("keeps desktop drafting controls floating while guest and non-drafting states stay gated", () => {
    const rendered = render(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(captured.shellMode).toBe("default");
    expect(captured.topActions.map(({ id }) => id)).toEqual(["pause-resume", "end-draft"]);
    expect(captured.hostActionsEnabled[captured.hostActionsEnabled.length - 1]).toBe(true);
    expect(captured.hostPresentations).toEqual(["floating"]);
    expect(captured.floatingActions[1]).toBe(captured.topActions[1]);
    expect(captured.floatingEndAction).toBe(captured.topActions[1]);
    expect(screen.getByTestId("host-controls-floating")).toBeInTheDocument();

    act(() => { store.state.role = "guest"; });
    rendered.rerender(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    expect(captured.topActions).toEqual([]);
    expect(screen.queryByTestId("host-controls-floating")).not.toBeInTheDocument();

    act(() => {
      store.state.role = "host";
      store.state.phase = "deckbuilding";
    });
    rendered.rerender(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    expect(captured.topActions).toEqual([]);
    expect(captured.floatingActions).toEqual([]);
    expect(captured.floatingEndAction).toBe(captured.hostEndActions[captured.hostEndActions.length - 1]);
    expect(screen.getByTestId("host-controls-floating")).toBeInTheDocument();
  });

  it("latches two compact end actions from one act before React rerenders", async () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 768 });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 1024 });
    vi.stubGlobal("confirm", vi.fn(() => true));
    let finishLeave: (() => void) | undefined;
    store.state.leave.mockImplementation(() => new Promise<void>((resolve) => {
      finishLeave = resolve;
    }));

    render(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    const endDraftAction = captured.topActions.find(({ id }) => id === "end-draft");
    if (!endDraftAction) throw new Error("compact end action was not installed");

    act(() => {
      endDraftAction.onClick();
      endDraftAction.onClick();
    });
    expect(window.confirm).toHaveBeenCalledOnce();
    expect(store.state.leave).toHaveBeenCalledOnce();

    finishLeave?.();
    await vi.waitFor(() => {
      expect(podStore.reset).toHaveBeenCalledOnce();
      expect(router.navigate).toHaveBeenCalledWith("/");
    });
  });

  it("keeps a pending compact end action disabled after the phase moves to floating controls", () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1024 });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 768 });
    vi.stubGlobal("confirm", vi.fn(() => true));
    store.state.leave.mockImplementation(() => new Promise<void>(() => undefined));

    const rendered = render(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    const endDraftAction = captured.topActions.find(({ id }) => id === "end-draft");
    if (!endDraftAction) throw new Error("compact end action was not installed");
    act(() => endDraftAction.onClick());
    expect(store.state.leave).toHaveBeenCalledOnce();

    act(() => { store.state.phase = "deckbuilding"; });
    rendered.rerender(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    const floatingEndAction = captured.floatingEndAction;
    expect(screen.getByTestId("host-controls-floating")).toBeDisabled();
    expect(floatingEndAction).toMatchObject({ disabled: true });
    act(() => floatingEndAction?.onClick());
    expect(store.state.leave).toHaveBeenCalledOnce();
  });

  it("re-enables a rejected end action so a new confirmed attempt can leave", async () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 768 });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 1024 });
    vi.stubGlobal("confirm", vi.fn(() => true));
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    store.state.leave
      .mockRejectedValueOnce(new Error("leave failed"))
      .mockResolvedValueOnce(undefined);

    render(<MemoryRouter><DraftPodPage /></MemoryRouter>);
    const firstEndDraftAction = captured.topActions.find(({ id }) => id === "end-draft");
    if (!firstEndDraftAction) throw new Error("compact end action was not installed");
    act(() => firstEndDraftAction.onClick());
    expect(store.state.leave).toHaveBeenCalledOnce();

    await vi.waitFor(() => {
      const retryEndDraftAction = captured.topActions.find(({ id }) => id === "end-draft");
      expect(retryEndDraftAction).toMatchObject({ disabled: false });
    });
    const retryEndDraftAction = captured.topActions.find(({ id }) => id === "end-draft");
    if (!retryEndDraftAction) throw new Error("retry end action was not installed");
    act(() => retryEndDraftAction.onClick());
    expect(store.state.leave).toHaveBeenCalledTimes(2);
    expect(window.confirm).toHaveBeenCalledTimes(2);
  });
  });
