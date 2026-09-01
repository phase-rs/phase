/**
 * Host administration controls read the POD SESSION phase, not the local
 * client's screen.
 *
 * `HostControls` administers the pod; `draftPodScreen` is a local, per-viewer
 * rendering concern (a viewer can hide the Bo3 intergame overlay without the pod
 * changing at all). Gating a host control on whether the host personally happens
 * to be sideboarding — still less on whether they have hidden that screen — would
 * be the same layering error the derived-screen change removes, inverted.
 *
 * Every fixture below carries a LIVE `sideboardPrompt` alongside
 * `phase: "matchInProgress"`, so the two authorities disagree
 * (`draftPodScreen(fixture) === "betweenGames"`). Without that, swapping
 * `HostControls`'s `s.phase` selector to `draftPodScreen` — exactly the silent
 * move this file exists to catch — would leave every row green.
 */

import { cleanup, fireEvent, render, renderHook, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DraftShellTopAction } from "../../chrome/ShellContext";
import { HostControls, useHostDraftTopActions } from "../HostControls";

const { draftState } = vi.hoisted(() => ({
  draftState: {
    role: "host" as string | null,
    phase: "matchInProgress",
    paused: false,
    sideboardPrompt: {
      matchId: "m1",
      gameNumber: 2,
      score: { p0_wins: 1, p1_wins: 0, draws: 0 },
      loserSeat: 1,
      timerMs: 0,
    } as unknown,
    playDrawPrompt: null as unknown,
    view: {
      pod_policy: "Casual",
      seats: [
        { seat_index: 1, display_name: "Alice", is_bot: false, connected: true },
      ],
    } as unknown,
    pairings: [
      {
        round: 1,
        table: 1,
        seat_a: 0,
        name_a: "Host",
        seat_b: 1,
        name_b: "Alice",
        match_id: "m1",
        status: "InProgress",
        winner_seat: null,
        score_a: 1,
        score_b: 0,
      },
    ],
    advanceRound: vi.fn(),
    requestPause: vi.fn(),
    requestResume: vi.fn(),
    overrideMatchResult: vi.fn(),
    replaceSeatWithBot: vi.fn(),
    leave: vi.fn(),
  },
}));

// The spread keeps the real `draftPodScreen` / `intergamePromptKey` exports
// resolvable, so a future edit importing one into `HostControls` runs against
// the shipped rule rather than throwing on a missing export.
vi.mock("../../../stores/multiplayerDraftStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../stores/multiplayerDraftStore")>()),
  useMultiplayerDraftStore: (selector: (state: typeof draftState) => unknown) => selector(draftState),
}));

const defaultEndDraftAction: DraftShellTopAction = {
  id: "end-draft",
  label: "End Draft",
  tone: "danger",
  onClick: vi.fn(),
};

function makeEndDraftAction(
  overrides: Partial<DraftShellTopAction> = {},
): DraftShellTopAction {
  return {
    ...defaultEndDraftAction,
    onClick: vi.fn(),
    ...overrides,
  };
}

function HostControlsHarness({
  endDraftAction = defaultEndDraftAction,
}: {
  endDraftAction?: DraftShellTopAction;
}) {
  const draftTopActions = useHostDraftTopActions({
    enabled: draftState.phase === "drafting",
    endDraftAction,
  });
  return <HostControls
    draftTopActions={draftTopActions}
    endDraftAction={endDraftAction}
  />;
}

describe("HostControls", () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  beforeEach(() => {
    draftState.role = "host";
    draftState.phase = "matchInProgress";
    draftState.sideboardPrompt = {
      matchId: "m1",
      gameNumber: 2,
      score: { p0_wins: 1, p1_wins: 0, draws: 0 },
      loserSeat: 1,
      timerMs: 0,
    };
    draftState.paused = false;
    draftState.requestPause.mockClear();
    draftState.requestResume.mockClear();
    draftState.leave.mockClear();
    defaultEndDraftAction.onClick = vi.fn();
  });

  // H1a — pinning row (not red-first: the fixture states `phase` directly). It
  // records the DECISION that this gate reads the pod-session phase.
  it("keeps Override match result available during the Bo3 intergame window", () => {
    render(<HostControlsHarness />);

    // REVERT-FAILING: narrow `showOverride` to exclude `matchInProgress`, or swap
    // the component's `s.phase` selector to `draftPodScreen`.
    expect(screen.getByText("Override Result")).toBeInTheDocument();
  });

  // H1b — its own row: one row asserting both predicates could not say which moved.
  it("keeps Kick / replace seat available during the Bo3 intergame window", () => {
    render(<HostControlsHarness />);

    // REVERT-FAILING: narrow `showKickReplace` to exclude `matchInProgress`, or
    // swap the component's `s.phase` selector to `draftPodScreen`.
    expect(screen.getByText("Kick + Replace")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Replace Alice with Bot" })).toBeInTheDocument();
  });

  // H2 — sibling/negative. The Pause control is the paired positive, so the two
  // absences are real absences rather than a component that returned `null` for
  // an unrelated reason (the `role !== "host"` early return, or the all-false one).
  it("hides both controls outside a match, while still rendering the drafting control", () => {
    draftState.phase = "drafting";
    draftState.sideboardPrompt = null;
    render(<HostControlsHarness />);

    expect(screen.getByRole("button", { name: "Pause Draft" })).toBeInTheDocument();
    expect(screen.queryByText("Override Result")).toBeNull();
    expect(screen.queryByText("Kick + Replace")).toBeNull();
  });

  // H3 — the role gate. Reach-guard: the same fixture rendered Override in H1a.
  it("renders nothing for a guest", () => {
    draftState.role = "guest";
    const { container } = render(<HostControlsHarness />);

    expect(container).toBeEmptyDOMElement();
  });

  // H4 — the host never loses its pod-level release on this screen. This is what
  // makes "the overlay holds until the host acts or the pod session moves" true
  // rather than "holds indefinitely".
  it("keeps End Draft available on the intergame screen and while drafting", () => {
    render(<HostControlsHarness />);
    // REVERT-FAILING: add `"matchInProgress"` to `showEndDraft`'s exclusion list.
    expect(screen.getByRole("button", { name: "End Draft" })).toBeInTheDocument();

    cleanup();
    draftState.phase = "drafting";
    draftState.sideboardPrompt = null;
    render(<HostControlsHarness />);
    expect(screen.getByRole("button", { name: "Pause Draft" })).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "End Draft" })).toHaveLength(1);
  });

  it("memoizes responsive pause/resume around the injected page end action", () => {
    draftState.phase = "drafting";
    draftState.sideboardPrompt = null;
    const endDraftAction = makeEndDraftAction();
    const { result, rerender } = renderHook(
      ({ enabled, action }) => useHostDraftTopActions({
        enabled,
        endDraftAction: action,
      }),
      { initialProps: { enabled: true, action: endDraftAction } },
    );

    expect(result.current.map(({ id, label, tone }) => ({ id, label, tone })))
      .toEqual([
        { id: "pause-resume", label: "Pause Draft", tone: "neutral" },
        { id: "end-draft", label: "End Draft", tone: "danger" },
      ]);
    expect(result.current[1]).toBe(endDraftAction);
    const first = result.current;
    rerender({ enabled: true, action: endDraftAction });
    expect(result.current).toBe(first);
    result.current[0].onClick();
    expect(draftState.requestPause).toHaveBeenCalledOnce();

    draftState.paused = true;
    rerender({ enabled: true, action: endDraftAction });
    expect(result.current[0]).toMatchObject({
      id: "pause-resume",
      label: "Resume Draft",
      tone: "emerald",
    });
    result.current[0].onClick();
    expect(draftState.requestResume).toHaveBeenCalledOnce();
  });

  it("returns no responsive draft actions when disabled, non-drafting, or guest", () => {
    draftState.phase = "drafting";
    const endDraftAction = makeEndDraftAction();
    const { result, rerender } = renderHook(
      ({ enabled }) => useHostDraftTopActions({ enabled, endDraftAction }),
      { initialProps: { enabled: false } },
    );
    expect(result.current).toEqual([]);

    rerender({ enabled: true });
    expect(result.current).toHaveLength(2);
    draftState.role = "guest";
    rerender({ enabled: true });
    expect(result.current).toEqual([]);
    draftState.role = "host";
    draftState.phase = "deckbuilding";
    rerender({ enabled: true });
    expect(result.current).toEqual([]);
  });

  it("forwards the supplied page action through responsive and floating controls", () => {
    draftState.phase = "drafting";
    draftState.sideboardPrompt = null;
    const endDraftAction = makeEndDraftAction();
    const { result } = renderHook(() => useHostDraftTopActions({
      enabled: true,
      endDraftAction,
    }));

    expect(result.current[1]).toBe(endDraftAction);
    render(
      <HostControls
        draftTopActions={result.current}
        endDraftAction={endDraftAction}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "End Draft" }));
    expect(endDraftAction.onClick).toHaveBeenCalledOnce();
    expect(draftState.leave).not.toHaveBeenCalled();

    cleanup();
    draftState.phase = "matchInProgress";
    render(<HostControls draftTopActions={[]} endDraftAction={endDraftAction} />);
    fireEvent.click(screen.getByRole("button", { name: "End Draft" }));
    expect(endDraftAction.onClick).toHaveBeenCalledTimes(2);
    expect(draftState.leave).not.toHaveBeenCalled();
  });

  it("forwards the page action disabled state in both presentations", () => {
    draftState.phase = "drafting";
    draftState.sideboardPrompt = null;
    const endDraftAction = makeEndDraftAction({ disabled: true });
    const { result } = renderHook(() => useHostDraftTopActions({
      enabled: true,
      endDraftAction,
    }));

    expect(result.current[1]).toBe(endDraftAction);
    render(
      <HostControls
        draftTopActions={result.current}
        endDraftAction={endDraftAction}
      />,
    );
    expect(screen.getByRole("button", { name: "End Draft" })).toBeDisabled();

    cleanup();
    draftState.phase = "matchInProgress";
    render(<HostControls draftTopActions={[]} endDraftAction={endDraftAction} />);
    expect(screen.getByRole("button", { name: "End Draft" })).toBeDisabled();
  });
});
