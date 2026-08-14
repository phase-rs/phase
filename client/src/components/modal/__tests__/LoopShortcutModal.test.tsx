import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  InteractionId,
  InteractionResponseSpec,
  ViewerInteraction,
} from "../../../adapter/generated/interaction";
import type {
  DecisionPoint,
  GameState,
  WaitingFor,
} from "../../../adapter/types.ts";
import {
  buildGameState,
  buildLoopShortcutWaitingFor,
  buildRespondToShortcutWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { DeclareShortcutModal, RespondToShortcutModal } from "../LoopShortcutModal.tsx";

const dispatchMock = vi.fn();

type ShortcutSpec = Extract<InteractionResponseSpec, { type: "shortcut" }>["data"];

/** The engine's published shortcut response spec, delivered on `viewerInteraction` exactly as
 *  `gameStore.legalResultState` assigns it. Defaults mirror the live publisher
 *  (`game/interaction.rs`): a Fixed window and `allow_decline: true`. */
function shortcutInteraction(
  overrides: Partial<ShortcutSpec> = {},
  // The offer's identity. Defaults to the literal every existing row was already written
  // against, so parameterizing it changes no existing row; the A→B rows below pass distinct
  // ids because a rotating id is precisely what they discriminate on.
  interactionId = "session.0.1",
): ViewerInteraction {
  const spec: ShortcutSpec = {
    count: { type: "fixed", data: { min: 1, max: 5, suggested: 5 } },
    points: [],
    allowDecline: true,
    preview: null,
    confirm: "explicit",
    ...overrides,
  };
  return {
    waitingForKind: { simultaneous: null, terminal: false, code: "shortcut" },
    authorizedSubmitters: [0],
    canSubmit: true,
    autoPassRecommended: false,
    opportunities: [
      {
        interactionId: interactionId as InteractionId,
        response: {
          type: "schema",
          data: { spec: { type: "shortcut", data: spec }, candidates: [] },
        },
        surfaces: [],
        progress: { selected: 0, minimum: 1, maximum: 1, aggregate: null, confirmable: false },
      },
    ],
    attachmentFans: {},
    availability: { type: "inputRequired" },
  };
}

// A ConvokeTaps decision-point with two tappable creatures (informational — the
// engine auto-taps via select_convoke_taps; the modal renders it read-only).
const convokePoint: DecisionPoint = {
  slot: { source: { ThisObject: { source_id: 40, incarnation: null } }, index: 0 },
  kind: { ConvokeTaps: { tappable: [40, 41] } },
};

// `viewerInteraction` is ALWAYS written (null by default): `setGameStoreForTest` merges into a
// module-level store, so an unset field would leak a previous test's published spec forward.
function seed(
  waitingFor: WaitingFor,
  overrides: Partial<GameState> = {},
  viewerInteraction: ViewerInteraction | null = null,
) {
  const gameState = buildGameState({
    objects: {},
    priority_player: 0,
    waiting_for: waitingFor,
    ...overrides,
  });
  setGameStoreForTest({ gameState, waitingFor, dispatch: dispatchMock, viewerInteraction });
}

describe("LoopShortcutModal", () => {
  beforeEach(() => {
    dispatchMock.mockReset();
    dispatchMock.mockResolvedValue(undefined);
  });

  afterEach(() => {
    cleanup();
  });

  // T1: the declare modal renders directly from the engine schema/certificate —
  // win_kind, iteration_count, and the read-only ConvokeTaps count. A wrong field
  // read renders a different/absent string and fails.
  it("renders the offer summary from certificate + schema (T1)", () => {
    seed(buildLoopShortcutWaitingFor({ schema: { points: [convokePoint] } }));
    render(<DeclareShortcutModal />);

    expect(screen.getByText("This loop deals lethal damage.")).toBeInTheDocument();
    expect(screen.getByText("Repeat until the game ends.")).toBeInTheDocument();
    expect(
      screen.getByText("Auto-taps up to 2 creatures for convoke each iteration."),
    ).toBeInTheDocument();
  });

  // T2: confirm dispatches the exact declare payload, echoing the schema's
  // iteration_count (UntilLethal) with template: null.
  it("dispatches DeclareShortcut echoing UntilLethal with template null (T2)", () => {
    seed(buildLoopShortcutWaitingFor());
    render(<DeclareShortcutModal />);

    fireEvent.click(screen.getByRole("button", { name: "Take the shortcut" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DeclareShortcut",
      data: { count: "UntilLethal", template: null },
    });
  });

  // T2 echo-guard: a Fixed(1) schema must dispatch count:{Fixed:1}, proving the
  // count is echoed from the schema, not a hardcoded "UntilLethal".
  it("echoes a Fixed iteration_count into the dispatch (T2 echo-guard)", () => {
    seed(buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 1 } } }));
    render(<DeclareShortcutModal />);

    fireEvent.click(screen.getByRole("button", { name: "Take the shortcut" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DeclareShortcut",
      data: { count: { Fixed: 1 }, template: null },
    });
    // §1b (`fixedCount_one`): CR 732.2b makes a proposal an upper bound, so the modal says
    // "at most" — the ruled wording. Fails against the pre-§1b catalog ("Repeat once.").
    expect(screen.getByText("Repeat at most once.")).toBeInTheDocument();
  });

  // §1b (`fixedCount_other`, CR 732.2c): post-fix the object-growth offer seeds
  // Fixed(MAX_SHORTCUT_CYCLES), and the modal echoes it verbatim — so the ceiling must render with
  // the "at most" wording. Covers the other plural leaf and the {{count}} interpolation; the
  // pre-§1b catalog renders "Repeat 1000 times." and fails.
  it("renders the ceiling with the at-most wording (§1b)", () => {
    seed(buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 1000 } } }));
    render(<DeclareShortcutModal />);

    expect(screen.getByText("Repeat at most 1000 times.")).toBeInTheDocument();
  });

  // T3: display-only — a ConvokeTaps point renders a read-only info line and NO
  // tappable-selection control (the confirm button is the only control), and
  // confirm still dispatches template: null.
  it("shows ConvokeTaps read-only with no selection control (T3)", () => {
    seed(buildLoopShortcutWaitingFor({ schema: { points: [convokePoint] } }));
    render(<DeclareShortcutModal />);

    expect(
      screen.getByText("Auto-taps up to 2 creatures for convoke each iteration."),
    ).toBeInTheDocument();
    // The only interactive controls are confirm + decline — no per-creature tap UI.
    const buttons = screen.getAllByRole("button");
    expect(buttons).toHaveLength(2);
    expect(buttons.map((b) => b.textContent)).toEqual([
      "Take the shortcut",
      "Decline the shortcut",
    ]);

    fireEvent.click(buttons[0]);
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DeclareShortcut",
      data: { count: "UntilLethal", template: null },
    });
  });

  // T3b (CR 732.2a): the declare modal offers a Decline control that dispatches the
  // payloadless DeclineShortcut — suggesting a shortcut is optional. Distinct from the
  // opponent-side Shorten; this is the controller declining their own auto-offer.
  it("dispatches DeclineShortcut on decline (T3b)", () => {
    seed(buildLoopShortcutWaitingFor());
    render(<DeclareShortcutModal />);

    fireEvent.click(screen.getByRole("button", { name: "Decline the shortcut" }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "DeclineShortcut" });
  });

  // C5a: the picker declares the count the PLAYER picked. Discriminating by construction — the
  // pre-C5 dispatch echoed `schema.iteration_count` ({Fixed:5}), and 2 is neither that, nor the
  // engine's `suggested` (5), nor either window edge (1/5), so no hardcoded value satisfies it.
  it("declares the picked count, not the engine's suggestion (C5a)", () => {
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 5 } } }),
      {},
      shortcutInteraction(),
    );
    render(<DeclareShortcutModal />);

    // Opens on the ENGINE's suggested count — the frontend holds no default.
    const box = screen.getByRole("spinbutton");
    expect(box).toHaveValue("5");

    fireEvent.change(box, { target: { value: "2" } });
    fireEvent.click(screen.getByRole("button", { name: "Take the shortcut" }));
    // COUNT ONLY, deliberately. `template` is asserted nowhere in the C5 rows: the engine refuses
    // a `template: null` declaration on a point-carrying schema (module header), so pinning the
    // whole payload here would codify a payload the engine does not accept as the end state.
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DeclareShortcut",
      data: expect.objectContaining({ count: { Fixed: 2 } }),
    });
  });

  // C5a bounds: the window is engine-owned. The steppers stop at the published max, and an entry
  // outside [min,max] declares NOTHING. The final legal entry is the paired positive reach-guard —
  // without it "never dispatched" could pass on a modal that renders no working control at all.
  it("steps inside the engine window and refuses an entry outside it (C5a bounds)", () => {
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 3 } } }),
      {},
      shortcutInteraction({ count: { type: "fixed", data: { min: 1, max: 3, suggested: 2 } } }),
    );
    render(<DeclareShortcutModal />);

    const box = screen.getByRole("spinbutton");
    fireEvent.click(screen.getByRole("button", { name: "Increase amount" }));
    expect(box).toHaveValue("3");
    expect(screen.getByRole("button", { name: "Increase amount" })).toBeDisabled();

    fireEvent.change(box, { target: { value: "9" } });
    fireEvent.click(screen.getByRole("button", { name: "Take the shortcut" }));
    expect(dispatchMock).not.toHaveBeenCalled();

    fireEvent.change(box, { target: { value: "1" } });
    fireEvent.click(screen.getByRole("button", { name: "Take the shortcut" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DeclareShortcut",
      data: expect.objectContaining({ count: { Fixed: 1 } }),
    });
  });

  // C5a negative: a window absent from the payload renders NO picker and never invents a
  // client-chosen count — the offer's own `iteration_count` is declared verbatim. Both absent
  // shapes are covered: no interaction projection at all, and an UntilLethal offer.
  it("renders no picker without a published window (C5a negative)", () => {
    seed(buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 5 } } }));
    render(<DeclareShortcutModal />);

    expect(screen.queryByRole("spinbutton")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Take the shortcut" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DeclareShortcut",
      data: expect.objectContaining({ count: { Fixed: 5 } }),
    });
    cleanup();
    dispatchMock.mockReset();

    seed(
      buildLoopShortcutWaitingFor(),
      {},
      shortcutInteraction({ count: { type: "untilLethal" } }),
    );
    render(<DeclareShortcutModal />);

    expect(screen.queryByRole("spinbutton")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Take the shortcut" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DeclareShortcut",
      data: expect.objectContaining({ count: "UntilLethal" }),
    });
  });

  // C4/§7.5: a count typed into offer A must not survive into offer B. The body is keyed on the
  // offer's `interactionId`, which the engine re-mints on every accepted action.
  //
  // ⚠ The second render MUST be `view.rerender(...)`, never a second `render(...)`. A fresh
  // `render` builds a new tree and mounts a new `DeclareShortcutOffer`, which resets `picked` on
  // the UNFIXED code too — the row would go green against the defect and prove nothing. The rows
  // above use `cleanup()` + `render()` between shapes; that is the opposite of what these need, so
  // do not "fix" these into the house idiom.
  it("starts offer B from its own suggestion, not the count typed into offer A (C4)", () => {
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 5 } } }),
      {},
      shortcutInteraction(
        { count: { type: "fixed", data: { min: 1, max: 9, suggested: 5 } } },
        "session.0.1",
      ),
    );
    const view = render(<DeclareShortcutModal />);

    const box = screen.getByRole("spinbutton");
    // Positive reach-guard: the entry actually landed, so a later "not 2" cannot pass vacuously
    // by the picker never having accepted input. `type="text"` + `role="spinbutton"`, so the
    // compared value is a STRING.
    fireEvent.change(box, { target: { value: "2" } });
    expect(box).toHaveValue("2");

    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 7 } } }),
      {},
      shortcutInteraction(
        { count: { type: "fixed", data: { min: 1, max: 9, suggested: 7 } } },
        "session.0.2",
      ),
    );
    view.rerender(<DeclareShortcutModal />);

    expect(screen.getByRole("spinbutton")).toHaveValue("7");
  });

  // The hostile sibling, and it is what kills the plausible wrong fix: offer B publishes a
  // BYTE-IDENTICAL window to A and differs only in `interactionId`. A key built from the window —
  // or from any `waitingFor.data` field — passes the row above and fails this one.
  it("resets on a second offer carrying an identical window (C4 hostile)", () => {
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 5 } } }),
      {},
      shortcutInteraction(
        { count: { type: "fixed", data: { min: 1, max: 9, suggested: 5 } } },
        "session.0.1",
      ),
    );
    const view = render(<DeclareShortcutModal />);

    const box = screen.getByRole("spinbutton");
    fireEvent.change(box, { target: { value: "2" } });
    expect(box).toHaveValue("2");

    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 5 } } }),
      {},
      shortcutInteraction(
        { count: { type: "fixed", data: { min: 1, max: 9, suggested: 5 } } },
        "session.0.2",
      ),
    );
    view.rerender(<DeclareShortcutModal />);

    expect(screen.getByRole("spinbutton")).toHaveValue("5");
  });

  // BL-1 (CR 732.2a), BOTH arms: Decline is offered iff the engine's `allowDecline` says so. The
  // false arm asserts Confirm is still present, so "no Decline button" cannot pass by the modal
  // having failed to render.
  it("renders Decline only when the engine allows it (BL-1)", () => {
    seed(buildLoopShortcutWaitingFor(), {}, shortcutInteraction({ allowDecline: true }));
    render(<DeclareShortcutModal />);
    expect(screen.getByRole("button", { name: "Decline the shortcut" })).toBeInTheDocument();
    cleanup();

    seed(buildLoopShortcutWaitingFor(), {}, shortcutInteraction({ allowDecline: false }));
    render(<DeclareShortcutModal />);
    expect(screen.queryByRole("button", { name: "Decline the shortcut" })).toBeNull();
    expect(screen.getByRole("button", { name: "Take the shortcut" })).toBeInTheDocument();
  });

  // C4 render path: the preview's magnitudes are the ENGINE's, already multiplied and signed, and
  // headed by the count they describe. The recompute-guard is the discriminator: moving the picker
  // to 2 must leave every number untouched — a component that rescaled the preview to the picked
  // count (or that recomputed it at all) fails here.
  it("renders the engine preview verbatim and never rescales it (C4 render)", () => {
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 4 } } }),
      {},
      shortcutInteraction({
        count: { type: "fixed", data: { min: 1, max: 4, suggested: 4 } },
        preview: {
          count: 4,
          entries: [
            { family: "life", player: 1, amount: -40 },
            { family: "mana", player: null, amount: 12 },
          ],
        },
      }),
    );
    render(<DeclareShortcutModal />);

    expect(screen.getByText("Repeating 4 times produces:")).toBeInTheDocument();
    expect(screen.getByText("-40 life — P2")).toBeInTheDocument();
    expect(screen.getByText("12 mana")).toBeInTheDocument();

    fireEvent.change(screen.getByRole("spinbutton"), { target: { value: "2" } });
    expect(screen.getByText("Repeating 4 times produces:")).toBeInTheDocument();
    expect(screen.getByText("-40 life — P2")).toBeInTheDocument();
    expect(screen.getByText("12 mana")).toBeInTheDocument();
  });

  // T4: the respond window renders the proposal and Accept dispatches Accept.
  it("renders the proposal and dispatches Accept (T4)", () => {
    seed(buildRespondToShortcutWaitingFor());
    render(<RespondToShortcutModal />);

    expect(screen.getByText("This loop deals lethal damage.")).toBeInTheDocument();
    expect(screen.getByText("Repeat until the game ends.")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Accept" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "RespondToShortcut",
      data: { response: "Accept" },
    });
  });

  // T5: "Break out" dispatches the Shorten payload shape (placeholder at_iteration).
  it("dispatches Shorten on break out (T5)", () => {
    seed(buildRespondToShortcutWaitingFor());
    render(<RespondToShortcutModal />);

    fireEvent.click(screen.getByRole("button", { name: "Break out" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "RespondToShortcut",
      data: { response: { Shorten: { at_iteration: 1 } } },
    });
  });

  // T6 (non-vacuity): both modals self-gate — a non-matching waitingFor.type
  // renders nothing and never dispatches.
  it("renders nothing on a non-matching waitingFor type (T6)", () => {
    seed({ type: "Priority", data: { player: 0 } });

    const declare = render(<DeclareShortcutModal />);
    expect(declare.container.firstChild).toBeNull();
    cleanup();

    const respond = render(<RespondToShortcutModal />);
    expect(respond.container.firstChild).toBeNull();

    expect(dispatchMock).not.toHaveBeenCalled();
  });

  // T7 (non-vacuity + MP-safety + site-1 revert-guard): a LoopShortcut whose
  // proposer is the opponent (seat 1) renders nothing for the local seat (0)
  // and never dispatches. `turn_decision_controller: null` rules out the
  // delegated-turn branch, so the ONLY reason it null-renders is the seat gate.
  // (If the usePlayerId site-1 fix were reverted, even a proposer:0 offer would
  // null-render → T1/T2 would fail — so those tests non-vacuously cover site-1.)
  it("renders nothing for a non-actor seat (T7)", () => {
    seed(buildLoopShortcutWaitingFor({ proposer: 1 }), {
      turn_decision_controller: null,
      active_player: 0,
    });

    const { container } = render(<DeclareShortcutModal />);
    expect(container.firstChild).toBeNull();
    expect(dispatchMock).not.toHaveBeenCalled();
  });
});
