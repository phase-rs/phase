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
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { HostControls, useHostDraftTopActions } from "../HostControls";

const { draftState, navigate, resetPod } = vi.hoisted(() => ({
  navigate: vi.fn(),
  resetPod: vi.fn(),
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

vi.mock("../../../stores/draftPodStore", () => ({
  useDraftPodStore: (selector: (state: { reset: () => void }) => unknown) => selector({ reset: resetPod }),
}));

vi.mock("react-router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-router")>()),
  useNavigate: () => navigate,
}));

function HostControlsHarness() {
  const draftTopActions = useHostDraftTopActions({
    enabled: draftState.phase === "drafting",
  });
  return <HostControls draftTopActions={draftTopActions} />;
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
    navigate.mockClear();
    resetPod.mockClear();
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
    render(<HostControls />);
    expect(screen.getByRole("button", { name: "End Draft" })).toBeInTheDocument();
  });

  it("memoizes the responsive host draft descriptors and dispatches pause or resume", () => {
    draftState.phase = "drafting";
    draftState.sideboardPrompt = null;
    const { result, rerender } = renderHook(
      ({ enabled }) => useHostDraftTopActions({ enabled }),
      { initialProps: { enabled: true } },
    );

    expect(result.current.map(({ id, label, tone }) => ({ id, label, tone })))
      .toEqual([
        { id: "pause-resume", label: "Pause Draft", tone: "neutral" },
        { id: "end-draft", label: "End Draft", tone: "danger" },
      ]);
    const first = result.current;
    rerender({ enabled: true });
    expect(result.current).toBe(first);
    result.current[0].onClick();
    expect(draftState.requestPause).toHaveBeenCalledOnce();

    draftState.paused = true;
    rerender({ enabled: true });
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
    const { result, rerender } = renderHook(
      ({ enabled }) => useHostDraftTopActions({ enabled }),
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

  it("confirms and ends the pod through the responsive end action", async () => {
    draftState.phase = "drafting";
    draftState.sideboardPrompt = null;
    vi.stubGlobal("confirm", vi.fn(() => true));
    draftState.leave.mockResolvedValue(undefined);
    const { result } = renderHook(() => useHostDraftTopActions({ enabled: true }));

    result.current[1].onClick();

    await vi.waitFor(() => expect(draftState.leave).toHaveBeenCalledWith(false));
    expect(resetPod).toHaveBeenCalledOnce();
    expect(navigate).toHaveBeenCalledWith("/");
  });

  it("keeps End Draft disabled when a pending leave survives a responsive presentation switch", async () => {
    draftState.phase = "drafting";
    draftState.sideboardPrompt = null;
    vi.stubGlobal("confirm", vi.fn(() => true));
    let finishLeave: (() => void) | undefined;
    draftState.leave.mockImplementation(() => new Promise<void>((resolve) => {
      finishLeave = resolve;
    }));

    function ResponsiveHostControlsHarness() {
      const [compact, setCompact] = useState(true);
      const draftTopActions = useHostDraftTopActions({
        enabled: draftState.phase === "drafting",
      });
      const endDraft = draftTopActions.find((action) => action.id === "end-draft");

      return (
        <>
          <button type="button" onClick={() => setCompact(false)}>Switch layout</button>
          {compact ? (
            <button
              type="button"
              disabled={endDraft?.disabled}
              onClick={endDraft?.onClick}
            >
              End Draft
            </button>
          ) : (
            <HostControls draftTopActions={draftTopActions} />
          )}
        </>
      );
    }

    render(<ResponsiveHostControlsHarness />);
    fireEvent.click(screen.getByRole("button", { name: "End Draft" }));
    await vi.waitFor(() => expect(draftState.leave).toHaveBeenCalledOnce());

    fireEvent.click(screen.getByRole("button", { name: "Switch layout" }));
    const endDraft = screen.getByRole("button", { name: "End Draft" });
    expect(endDraft).toBeDisabled();
    fireEvent.click(endDraft);
    expect(draftState.leave).toHaveBeenCalledOnce();

    finishLeave?.();
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledWith("/"));
  });
});
