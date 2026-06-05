import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { DialogHost } from "../DialogHost.tsx";
import { DialogShell } from "../DialogShell.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import type { WaitingFor } from "../../../adapter/types.ts";

function setWaitingFor(waitingFor: WaitingFor | null) {
  useGameStore.setState({ waitingFor });
}

// Minimal gameState stub: `useCanActForWaitingState` short-circuits to false
// when `gameState` is null, so every test that expects the local player to
// be the actor needs a non-null state. The hook only reads
// `turn_decision_controller` and `active_player`, both 0 here so player 0
// (default PLAYER_ID) qualifies as the local actor.
const stubGameState = {
  turn_decision_controller: 0,
  active_player: 0,
} as never;

describe("DialogHost", () => {
  beforeEach(() => {
    useGameStore.setState({ gameState: stubGameState });
  });
  afterEach(() => {
    cleanup();
    setWaitingFor(null);
    useGameStore.setState({ gameState: null });
    useUiStore.setState({ pendingAbilityChoice: null, enchantmentsDialogPlayer: null });
  });

  it("hides the peek-restore tab while the dialog is visible (un-peeked)", () => {
    setWaitingFor({ type: "ModeChoice", data: { player: 0 } } as never);
    render(
      <DialogHost>
        <DialogShell title="t">
          <div />
        </DialogShell>
      </DialogHost>,
    );
    expect(screen.queryByLabelText("Restore dialog")).not.toBeInTheDocument();
  });

  it("toggles peek when the shell's peek button is clicked", () => {
    setWaitingFor({ type: "ModeChoice", data: { player: 0 } } as never);
    render(
      <DialogHost>
        <DialogShell title="t">
          <div />
        </DialogShell>
      </DialogHost>,
    );

    fireEvent.click(screen.getByLabelText("Move dialog out of the way"));
    expect(screen.getByLabelText("Restore dialog")).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("Restore dialog"));
    expect(screen.queryByLabelText("Restore dialog")).not.toBeInTheDocument();
  });

  it("does not render the peek tab for non-dialog WaitingFor types", () => {
    setWaitingFor({ type: "Priority", data: { player: 0 } } as never);
    render(
      <DialogHost>
        <DialogShell title="t">
          <div />
        </DialogShell>
      </DialogHost>,
    );
    fireEvent.click(screen.getByLabelText("Move dialog out of the way"));
    expect(screen.queryByLabelText("Restore dialog")).not.toBeInTheDocument();
  });

  it("does not establish a viewport-blocking wrapper when the opponent is the waiting player (regression)", () => {
    // Opponent (player 1) is searching their library; local player is 0.
    // The host MUST NOT wrap children in `fixed inset-0 z-40`, otherwise
    // the empty overlay swallows pointer events and the local viewer can't
    // hover/zoom cards while spectating.
    setWaitingFor({ type: "SearchLibrary", data: { player: 1 } } as never);
    const { container } = render(
      <DialogHost>
        <div data-testid="child" />
      </DialogHost>,
    );
    const wrapper = container.firstElementChild as HTMLElement | null;
    expect(wrapper?.className ?? "").not.toMatch(/fixed/);
  });

  it("anchors convoke mana payment at z-40 but stays click-through via pointer-events (regression)", () => {
    // CR 702.51a / CR 702.126a: a convoke/improvise spell enters `ManaPayment`
    // with `convoke_mode` set, and the caster taps creatures/artifacts on the
    // battlefield to pay. The host MUST anchor the panel in its `fixed inset-0
    // z-40` stacking context (otherwise framer-motion's transform demotes an
    // un-anchored host to a z-auto context that paints BELOW the board's z-10
    // grid, burying the pay panel behind the HUD/hand). Click-through is
    // achieved with `pointer-events: none` so board taps still reach the
    // creatures/artifacts; the panel's own controls re-enable events.
    setWaitingFor({
      type: "ManaPayment",
      data: { player: 0, convoke_mode: "Convoke" },
    } as never);
    const { container } = render(
      <DialogHost>
        <div data-testid="child" />
      </DialogHost>,
    );
    const wrapper = container.firstElementChild as HTMLElement | null;
    expect(wrapper?.className ?? "").toMatch(/fixed/);
    expect(wrapper?.className ?? "").toMatch(/z-40/);
    expect(wrapper?.style.pointerEvents).toBe("none");
  });

  it("restores host pointer events during convoke when a UI modal is open (#1532)", () => {
    // Issue #1532: convoke payment is click-through so board taps reach creatures,
    // but an ability picker opened mid-payment (mana dork + convoke-eligible) must
    // stay interactive — not inherit pointer-events:none from the host.
    setWaitingFor({
      type: "ManaPayment",
      data: { player: 0, convoke_mode: "Convoke" },
    } as never);
    useUiStore.setState({
      pendingAbilityChoice: {
        objectId: 41,
        actions: [{ type: "TapForConvoke", data: { object_id: 41, mana_type: "Green" } }],
      },
    });
    const { container } = render(
      <DialogHost>
        <div data-testid="child" />
      </DialogHost>,
    );
    const wrapper = container.firstElementChild as HTMLElement | null;
    expect(wrapper?.className ?? "").toMatch(/fixed/);
    expect(wrapper?.className ?? "").toMatch(/z-40/);
    expect(wrapper?.style.pointerEvents).not.toBe("none");
  });

  it("keeps the viewport wrapper for plain mana payment without convoke", () => {
    // Plain payment is committed via the panel's Pay button (no board taps), so
    // the host wraps normally — only `convoke_mode` payments go click-through.
    setWaitingFor({
      type: "ManaPayment",
      data: { player: 0 },
    } as never);
    const { container } = render(
      <DialogHost>
        <div data-testid="child" />
      </DialogHost>,
    );
    const wrapper = container.firstElementChild as HTMLElement | null;
    expect(wrapper?.className ?? "").toMatch(/fixed/);
  });

  it("resets peek to false when WaitingFor changes (regression)", () => {
    setWaitingFor({ type: "ModeChoice", data: { player: 0 } } as never);
    render(
      <DialogHost>
        <DialogShell title="t">
          <div />
        </DialogShell>
      </DialogHost>,
    );

    fireEvent.click(screen.getByLabelText("Move dialog out of the way"));
    expect(screen.getByLabelText("Restore dialog")).toBeInTheDocument();

    act(() => {
      setWaitingFor({ type: "ReplacementChoice", data: { player: 0 } } as never);
    });
    expect(screen.queryByLabelText("Restore dialog")).not.toBeInTheDocument();
  });
});
