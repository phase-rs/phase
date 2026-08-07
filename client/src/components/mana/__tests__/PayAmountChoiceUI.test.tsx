import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import type { LoopCollapseAxis, WaitingFor } from "../../../adapter/types";
import {
  buildGameState,
  buildPayAmountChoiceWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { useGameStore } from "../../../stores/gameStore";
import { PayAmountChoiceUI } from "../PayAmountChoiceUI.tsx";

// CR 732.2a: the LoopCollapse prompt must name the axis the loop collapses. The
// counter/life labels are iteration-framed (×N), never a raw token count.
function loopCollapseWaitingFor(axis: LoopCollapseAxis, min = 0): WaitingFor {
  return buildPayAmountChoiceWaitingFor({
    data: {
      player: 0,
      resource: { type: "LoopCollapse", data: { axis } },
      // The box initializes `raw` to `String(min)`, so `min` pins the rendered count.
      min,
      max: 1000,
      source_id: 0,
    },
  });
}

// player 0 == local PLAYER_ID, and turn_decision_controller/active_player are the
// local seat, so `useCanActForWaitingState` returns true and the prompt renders
// (else it renders null and every text query passes VACUOUSLY).
function renderWithAxis(axis: LoopCollapseAxis, min = 0) {
  const waitingFor = loopCollapseWaitingFor(axis, min);
  setGameStoreForTest({
    gameState: buildGameState({
      waiting_for: waitingFor,
      active_player: 0,
      turn_decision_controller: 0,
    }),
    waitingFor,
  });
  return render(<PayAmountChoiceUI />);
}

describe("PayAmountChoiceUI — LoopCollapse axis label", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  afterEach(() => {
    cleanup();
  });

  it("labels a Counters loop with iteration framing, never a raw token count", () => {
    renderWithAxis("Counters");
    // Positive reach-guard: the prompt actually rendered (canAct true) — the commit
    // button is iteration-framed ("Add counters N×"), not a raw resource count.
    expect(
      screen.getByRole("button", { name: /add counters/i }),
    ).toBeInTheDocument();
    // Negative (the pre-fix bug): a counter loop must NOT say "tokens".
    expect(screen.queryByText(/tokens/i)).not.toBeInTheDocument();
  });

  it("labels a Tokens loop with token framing", () => {
    renderWithAxis("Tokens");
    expect(
      screen.getByRole("button", { name: /create .* tokens/i }),
    ).toBeInTheDocument();
  });

  // LOW-5 (i18next plural): the Tokens label must agree in number with the count. The old
  // hardcoded "Create {{value}} tokens" rendered "Create 1 tokens" at value=1; the `_one`/`_other`
  // keys keyed on `count` fix the singular.
  it("renders the SINGULAR token label at value=1", () => {
    renderWithAxis("Tokens", 1);
    // Revert-flip: restore the hardcoded plural key ⇒ "Create 1 tokens" ⇒ this /token$/ fails.
    expect(
      screen.getByRole("button", { name: /create 1 token$/i }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /create 1 tokens/i }),
    ).not.toBeInTheDocument();
  });

  it("renders the PLURAL token label at value=2 (matched-pair reach-guard)", () => {
    renderWithAxis("Tokens", 2);
    expect(
      screen.getByRole("button", { name: /create 2 tokens$/i }),
    ).toBeInTheDocument();
  });
});

const BOX = "Choose amount to pay";

/**
 * Seed exactly as `renderWithAxis` does — player 0 == the local seat, with
 * `active_player`/`turn_decision_controller` on that seat — so
 * `useCanActForWaitingState` is true and no query below passes VACUOUSLY.
 */
function renderPrompt(waitingFor: WaitingFor) {
  const seeded = setGameStoreForTest({
    gameState: buildGameState({
      waiting_for: waitingFor,
      active_player: 0,
      turn_decision_controller: 0,
    }),
    waitingFor,
  });
  return { ...seeded, ...render(<PayAmountChoiceUI />) };
}

function type(value: string) {
  fireEvent.change(screen.getByLabelText(BOX), { target: { value } });
}

describe("PayAmountChoiceUI — sanitized amount entry", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  afterEach(() => {
    cleanup();
  });

  it("T1/reach: a valid entry enables commit and dispatches exactly what was typed", () => {
    const { dispatch } = renderPrompt(loopCollapseWaitingFor("Tokens"));

    // Positive control for every negative below: the harness reaches a rendered,
    // enabled, dispatch-firing control.
    expect(screen.getByLabelText(BOX)).toBeInTheDocument();
    type("377");

    const commit = screen.getByRole("button", { name: /create 377 tokens/i });
    expect(commit).not.toBeDisabled();
    fireEvent.click(commit);
    expect(dispatch).toHaveBeenCalledWith({
      type: "SubmitPayAmount",
      data: { amount: 377 },
    });
  });

  it("T2/above-max: an entry over the engine max cannot be committed", () => {
    const { dispatch } = renderPrompt(loopCollapseWaitingFor("Tokens"));
    type("1001");

    expect(screen.getByLabelText(BOX)).toHaveValue("1001");
    // Name pinned to the neutral verb: "Create 0 tokens" here would state an amount the player
    // never typed, and `/create/i` would match it.
    expect(screen.getByRole("button", { name: "Confirm" })).toBeDisabled();
    // DOMINATED by the assertion above — labelled, per this file's convention, so it is not
    // mistaken for a probe of `handleCommit`'s null-guard. MEASURED: deleting
    // `if (amount === null) return;` reds ONLY T6b, and adding a click here does not change
    // that, because React does not dispatch `onClick` to a disabled `button` at all. That
    // suppression is React-level, not environment-level (this suite runs happy-dom), so
    // switching the test environment would not change the calculus. Enter (T6b) bypasses
    // `disabled` and is the only route on which that guard is observable.
    expect(dispatch).not.toHaveBeenCalled();
  });

  it("T3/below-min: an entry under the engine min cannot be committed", () => {
    // Provenance honesty: `min > 0` is NOT currently reachable in production — every
    // PayAmountChoice mint site hardcodes `min: 0`. This is a prop-contract test on the
    // wire type, not a live-state repro.
    const { dispatch } = renderPrompt(
      buildPayAmountChoiceWaitingFor({
        data: {
          player: 0,
          resource: { type: "Counters" },
          min: 5,
          max: 1000,
          source_id: 0,
        },
      }),
    );
    type("3");

    expect(screen.getByRole("button", { name: "Confirm" })).toBeDisabled();
    expect(
      screen.getByText("Enter a whole number between 5 and 1000"),
    ).toBeInTheDocument();
    // DOMINATED — see T2. The discriminating row for the null-guard is T6b.
    expect(dispatch).not.toHaveBeenCalled();
  });

  // `min` is PINNED at 0 here: at min=5 the "+2" member becomes DOMINATED by the range
  // conjunct and stops discriminating the digit guard.
  it.each(["", " 7 ", "1.5", "1e3", "+2", "0x10"])(
    "T4/non-numeric: %j never reaches the engine",
    (raw) => {
      const { dispatch } = renderPrompt(loopCollapseWaitingFor("Tokens"));
      type(raw);

      expect(screen.getByRole("button", { name: "Confirm" })).toBeDisabled();
      // DOMINATED — see T2. The discriminating row for the null-guard is T6b.
      expect(dispatch).not.toHaveBeenCalled();
    },
  );

  it("T5/cleared: clearing a valid entry cannot submit the 0 the player never typed", () => {
    const { dispatch } = renderPrompt(loopCollapseWaitingFor("Tokens"));
    type("377");
    type("");

    expect(screen.getByRole("button", { name: "Confirm" })).toBeDisabled();
    // The click that used to sit here was inert — React does not dispatch `onClick` to a
    // disabled `button` — and implied coverage this row does not have. DOMINATED; T6b
    // discriminates. See T2.
    expect(dispatch).not.toHaveBeenCalled();
  });

  // Reset is keyed on prompt IDENTITY, not just its window. Keyed on [min, max] alone the effect
  // would not fire for a successor prompt with the same bounds, and the previous decision would
  // carry into it. `min`/`max` are held CONSTANT here so `source_id` is the only changing dep —
  // the one shape in which the identity dependency is observable.
  it("T10/successor: a same-window prompt from a DIFFERENT source resets the entry", () => {
    const promptFrom = (sourceId: number) =>
      buildPayAmountChoiceWaitingFor({
        data: {
          player: 0,
          resource: { type: "Counters" },
          min: 0,
          max: 10,
          source_id: sourceId,
        },
      });

    const { rerender } = renderPrompt(promptFrom(1));
    type("7");
    expect(screen.getByLabelText(BOX)).toHaveValue("7");

    const successor = promptFrom(2);
    setGameStoreForTest({
      gameState: buildGameState({
        waiting_for: successor,
        active_player: 0,
        turn_decision_controller: 0,
      }),
      waitingFor: successor,
    });
    rerender(<PayAmountChoiceUI />);

    expect(screen.getByLabelText(BOX)).toHaveValue("0");
  });

  // The seat axis, which `source_id` alone does NOT cover. The reachable case is the LoopCollapse
  // arm in `game/turns.rs` — the same prompt `loopCollapseWaitingFor` models here — which mints one
  // prompt per controller in APNAP order with `min: 0`, `accumulated: 0`, `source_id: ObjectId(0)`
  // and `pending_mana_ability: None` written as LITERALS. Only `max` and the collapse axis derive
  // from state, so two controllers with equal counts on the same axis differ in `player` alone.
  // (An earlier version cited an equal-life case on `effects/pay.rs` instead. That test does show
  // consecutive prompts with a constant source and `player` 0 then 1 — but `max` and `accumulated`
  // move there too, so `[min, max]` alone would already have fired and it proves nothing about
  // `source_id` being insufficient. Same correction as in the component.)
  it("T11/successor-seat: a same-source prompt for a DIFFERENT seat resets the entry", () => {
    const promptFor = (player: number) =>
      buildPayAmountChoiceWaitingFor({
        data: {
          player,
          resource: { type: "Counters" },
          min: 0,
          max: 10,
          source_id: 9,
        },
      });

    // CR 723.5 ("while controlling another player, a player makes all choices and decisions the
    // controlled player is allowed to make") + CR 723.3 ("a player who's being controlled during
    // their turn is still the active player") — which together are why the local seat (0) stays
    // `turn_decision_controller` while the prompt's semantic player moves to seat 1. That is what
    // keeps `useCanActForWaitingState` (usePlayerId.ts:95) true, so ONE client answers both
    // prompts in a row. Setting the controller to 1 as well hides the panel and makes this row
    // vacuous — it fails that way, which is how the shape was pinned rather than guessed.
    const seat = (waitingFor: WaitingFor, player: number) => {
      setGameStoreForTest({
        gameState: buildGameState({
          waiting_for: waitingFor,
          active_player: player,
          turn_decision_controller: 0,
        }),
        waitingFor,
      });
    };

    const first = promptFor(0);
    seat(first, 0);
    const { rerender } = render(<PayAmountChoiceUI />);
    type("7");
    expect(screen.getByLabelText(BOX)).toHaveValue("7");

    const second = promptFor(1);
    seat(second, 1);
    rerender(<PayAmountChoiceUI />);

    expect(screen.getByLabelText(BOX)).toHaveValue("0");
  });

  it("T6a/enter-valid: Enter submits a valid entry", () => {
    const { dispatch } = renderPrompt(loopCollapseWaitingFor("Tokens"));
    type("42");
    fireEvent.keyDown(screen.getByLabelText(BOX), { key: "Enter" });

    expect(dispatch).toHaveBeenCalledWith({
      type: "SubmitPayAmount",
      data: { amount: 42 },
    });
  });

  it("T6b/enter-invalid: Enter does NOT coerce an out-of-range entry", () => {
    const { dispatch } = renderPrompt(loopCollapseWaitingFor("Tokens"));
    type("1001");
    fireEvent.keyDown(screen.getByLabelText(BOX), { key: "Enter" });

    expect(dispatch).not.toHaveBeenCalled();
  });

  it("T7/engine-bound: the window comes from the engine, not a client constant", () => {
    // Hostile multi-authority fixture: max=7, not the LoopCollapse 1000. Runnable proof
    // that no client-side ceiling exists.
    const { dispatch } = renderPrompt(
      buildPayAmountChoiceWaitingFor({
        data: {
          player: 0,
          resource: { type: "LoopCollapse", data: { axis: "Tokens" } },
          min: 0,
          max: 7,
          source_id: 0,
        },
      }),
    );

    type("8");
    expect(screen.getByRole("button", { name: "Confirm" })).toBeDisabled();

    type("7");
    const commit = screen.getByRole("button", { name: /create 7 tokens/i });
    expect(commit).not.toBeDisabled();
    fireEvent.click(commit);
    expect(dispatch).toHaveBeenCalledWith({
      type: "SubmitPayAmount",
      data: { amount: 7 },
    });
  });

  it("T8/seat-gate: the prompt renders only for the seat the engine addressed", () => {
    // Rendering the component directly bypasses GamePage's outer gate, so the component's
    // own `!data || !canAct` guard is genuinely the expression under test.
    const { container } = renderPrompt(
      buildPayAmountChoiceWaitingFor({
        data: {
          player: 1,
          resource: { type: "LoopCollapse", data: { axis: "Tokens" } },
          min: 0,
          max: 1000,
          source_id: 0,
        },
      }),
    );
    expect(container).toBeEmptyDOMElement();

    // PINNED POSITIVE, same test: the emptiness above is satisfiable by a constant
    // `return null`, so the identical prompt on the LOCAL seat must render the box.
    cleanup();
    renderPrompt(loopCollapseWaitingFor("Tokens"));
    expect(screen.getByLabelText(BOX)).toBeInTheDocument();
  });

  it("T9/zero: 0 is a legal amount and commits (the truthiness-bug class)", () => {
    const { dispatch } = renderPrompt(loopCollapseWaitingFor("Tokens"));

    // `raw` is "0" at mount (min = 0), so this asserts the mounted state directly.
    expect(screen.getByLabelText(BOX)).toHaveValue("0");
    const commit = screen.getByRole("button", { name: /create 0 tokens/i });
    expect(commit).not.toBeDisabled();
    fireEvent.click(commit);
    expect(dispatch).toHaveBeenCalledWith({
      type: "SubmitPayAmount",
      data: { amount: 0 },
    });
  });
});
