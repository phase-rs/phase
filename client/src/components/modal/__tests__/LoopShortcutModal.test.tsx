import { act, cleanup, fireEvent, isInaccessible, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  AmountAssignment,
  InteractionChoice,
  InteractionChoiceId,
  InteractionId,
  InteractionPreview,
  InteractionPreviewRequest,
  InteractionResponseSpec,
  InteractionShortcutPin,
  InteractionShortcutPoint,
  InteractionShortcutPreview,
  InteractionShortcutPreviewEntry,
  ViewerInteraction,
} from "../../../adapter/generated/interaction";
import type {
  DecisionPoint,
  EngineAdapter,
  GameState,
  WaitingFor,
} from "../../../adapter/types.ts";
import { dispatchInteraction, previewInteractionResponse } from "../../../game/dispatch.ts";
import { useAppNotificationStore } from "../../../stores/appToastStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import {
  buildGameState,
  buildLoopShortcutWaitingFor,
  buildRespondToShortcutWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { DeclareShortcutModal, RespondToShortcutModal } from "../LoopShortcutModal.tsx";

// The pin route leaves through `dispatchInteraction`; the count-only route leaves through the
// store's own `dispatch`. Both are observed by every routing row, so a regression in either
// direction fires. The rest of the module is left ORIGINAL, so the preview seam the rows below
// drive is the real implementation.
vi.mock("../../../game/dispatch.ts", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../game/dispatch.ts")>()),
  dispatchAction: vi.fn(),
  dispatchInteraction: vi.fn(),
}));

const dispatchMock = vi.fn();

type ShortcutSpec = Extract<InteractionResponseSpec, { type: "shortcut" }>["data"];
type ShortcutReplySpec = Extract<InteractionResponseSpec, { type: "shortcutReply" }>["data"];

/** The engine's published shortcut response spec, delivered on `viewerInteraction` exactly as
 *  `gameStore.legalResultState` assigns it. Defaults mirror the live publisher
 *  (`game/interaction.rs`): a Fixed window and `allow_decline: true`. */
function shortcutInteraction(
  overrides: Partial<ShortcutSpec> = {},
  // The offer's identity. Defaults to the literal every existing row was already written
  // against, so parameterizing it changes no existing row; the A→B rows below pass distinct
  // ids because a rotating id is precisely what they discriminate on.
  interactionId = "session.0.1",
  // The offer's published candidates — the choices its decision points name by id. Defaults to
  // the empty list.
  candidates: InteractionChoice[] = [],
): ViewerInteraction {
  const spec: ShortcutSpec = {
    count: { type: "fixed", data: { min: 1, max: 5, suggested: 5 } },
    points: [],
    allowDecline: true,
    preview: [],
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
          data: { spec: { type: "shortcut", data: spec }, candidates },
        },
        surfaces: [],
        progress: { selected: 0, minimum: 1, maximum: 1, aggregate: null, confirmable: false },
      },
    ],
    attachmentFans: {},
  attachmentViews: {},
    availability: { type: "inputRequired" },
  };
}

// A ConvokeTaps decision-point with two tappable creatures (informational — the
// engine auto-taps via select_convoke_taps; the modal renders it read-only).
const convokePoint: DecisionPoint = {
  slot: { source: { ThisObject: { source_id: 40, incarnation: null } }, index: 0 },
  kind: { ConvokeTaps: { tappable: [40, 41] } },
};

// ─── Projection builders. Each row states only what it varies. ────────────────────────────────
const cid = (id: string) => id as InteractionChoiceId;
const amt = (id: string, amount: number): AmountAssignment => ({ choiceId: cid(id), amount });
const fixedCount = (min: number, max: number, suggested: number): ShortcutSpec["count"] => ({
  type: "fixed",
  data: { min, max, suggested },
});

function targetsPoint(
  group: number,
  ids: string[],
  overrides: Partial<InteractionShortcutPoint> = {},
): InteractionShortcutPoint {
  return {
    group,
    kind: "targets",
    min: 1,
    max: 1,
    unique: false,
    ordered: true,
    readOnly: false,
    candidateIds: ids.map(cid),
    ...overrides,
  };
}

function mayPoint(
  group: number,
  ids: string[],
  overrides: Partial<InteractionShortcutPoint> = {},
): InteractionShortcutPoint {
  return {
    group,
    kind: "mayChoice",
    min: 1,
    max: 1,
    unique: true,
    ordered: false,
    readOnly: false,
    candidateIds: ids.map(cid),
    ...overrides,
  };
}

/** The engine's published accept-or-shorten spec, delivered on `viewerInteraction` exactly as
 *  `gameStore.legalResultState` assigns it. The sibling of `shortcutInteraction`, one predicate
 *  apart; none existed before the responder read a declaration. Defaults mirror the live
 *  publisher on a count-only proposal: no statement point and no declared element. */
function respondInteraction(
  overrides: Partial<ShortcutReplySpec> = {},
  candidates: InteractionChoice[] = [],
  interactionId = "session.0.2",
): ViewerInteraction {
  const spec: ShortcutReplySpec = {
    minIteration: 0,
    maxIteration: 5,
    points: [],
    declared: null,
    allocationGroup: null,
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
          data: { spec: { type: "shortcutReply", data: spec }, candidates },
        },
        surfaces: [],
        progress: { selected: 0, minimum: 1, maximum: 1, aggregate: null, confirmable: false },
      },
    ],
    attachmentFans: {},
    attachmentViews: {},
    availability: { type: "inputRequired" },
  };
}

/** A read-only announced-target statement point — what the engine publishes on the respond side,
 *  where the responder's only outbound values are Accept and Shorten. */
function statementTargetsPoint(group: number, ids: string[]): InteractionShortcutPoint {
  return targetsPoint(group, ids, { min: 0, max: 0, unique: true, readOnly: true });
}

/** A read-only optional-decision statement point: SUBJECT then ANSWER, in that order. */
function statementMayPoint(group: number, subjectId: string, answerId: string) {
  return mayPoint(group, [subjectId, answerId], {
    min: 0,
    max: 0,
    ordered: true,
    readOnly: true,
  });
}

/** A read-only point: `readOnly` is the field the routing rule turns on. */
function readOnlyPoint(group: number, kind: "convokeTaps" | "manaColor"): InteractionShortcutPoint {
  return {
    group,
    kind,
    min: 0,
    max: 0,
    unique: true,
    ordered: false,
    readOnly: true,
    candidateIds: [],
  };
}

/** A non-read-only kind this modal does not render. */
function unrenderablePoint(
  group: number,
  kind: "mode" | "unlessBreak",
): InteractionShortcutPoint {
  return {
    group,
    kind,
    min: 1,
    max: 1,
    unique: true,
    ordered: true,
    readOnly: false,
    candidateIds: [cid(`${kind}-0`)],
  };
}

function seatCandidate(id: string, seat: number): InteractionChoice {
  return {
    id: cid(id),
    surfaces: [{ type: "player", data: { role: "target", index: null, seat } }],
    status: { type: "available" },
  };
}

function objectCandidate(id: string, name: string | null, reference: string): InteractionChoice {
  return {
    id: cid(id),
    surfaces: [
      {
        type: "object",
        data: {
          role: "target",
          index: null,
          reference,
          name,
          zone: null,
          controller: null,
          power: null,
          tapped: null,
        },
      },
    ],
    status: { type: "available" },
  };
}

/** One published option, its `value` surface stated by the caller — the axis the modal's
 *  wording whitelist reads. */
function mayAnswer(id: string, value: string): InteractionChoice {
  return {
    id: cid(id),
    surfaces: [{ type: "value", data: { role: "accept", index: null, value } }],
    status: { type: "available" },
  };
}

/** A may point's two published options. The control reads the `value` surface these carry —
 *  never the index. */
function mayCandidates(takeId: string, declineId: string): InteractionChoice[] {
  return [mayAnswer(takeId, "take"), mayAnswer(declineId, "decline")];
}

function element(
  count: number,
  allocation: AmountAssignment[],
  entries: InteractionShortcutPreviewEntry[] = [],
): InteractionShortcutPreview {
  return { count, entries, allocation };
}

/** The pins of the single submission the pin route sent. Throws rather than returning a shape, so
 *  a row asserting on it cannot pass against a submission that never happened. */
function submittedPins(callIndex = 0): InteractionShortcutPin[] {
  const call = vi.mocked(dispatchInteraction).mock.calls[callIndex];
  if (!call) throw new Error("dispatchInteraction was not called");
  const response = call[0].response;
  if (response.type !== "shortcut") throw new Error(`not a shortcut submission: ${response.type}`);
  return response.data.pins;
}

const confirmButton = () => screen.getByRole("button", { name: "Take the shortcut" });
const allocationRow = (subject: string) =>
  screen.getByRole("spinbutton", { name: `Repetitions for ${subject}` });
const countBox = () => screen.getByRole("spinbutton", { name: "Number of iterations" });
/** The announce control, whose accessible name names the subject it announces — so the query
 *  matches the shape rather than a literal. `getAllByRole` returns DOM order, which is the
 *  rendered row order, and the anchors keep a control that lost its subject from still matching. */
const ANNOUNCE = /^Announce .+$/;

/** Every control this suite's DOM implementation scores focusable, paired with the name assistive
 *  technology announces for it. The population is a computed property — a non-negative `tabIndex`
 *  as that implementation scores it, restricted to what the accessibility tree exposes — not a
 *  list of tags or roles, so a control of a shape used nowhere here yet is still inside the
 *  invariant; its root is `document.body`, the same root `screen` queries from. Disabled controls
 *  stay in: they score focusable here and the dialog announces them. `aria-hidden` subtrees stay
 *  out: they are not in the accessibility tree and carry no name obligation. Two shapes are
 *  deliberately outside it — an element made a control by an ARIA `role` alone, which no role
 *  taxonomy reachable from this file can tell apart from a live region, and
 *  `<summary>`/`[contenteditable]`, which browsers focus but this harness scores -1. The name is
 *  derived from how this dialog labels a control, then checked against the real accessible-name
 *  computation, so a control named some other way cannot slip through as an empty string. */
function controlNames(): string[] {
  const controls = [...document.body.querySelectorAll<HTMLElement>("*")].filter(
    (el) => el.tabIndex >= 0 && !isInaccessible(el),
  );
  return controls.map((el) => {
    const name = (el.getAttribute("aria-label") ?? el.textContent ?? "").trim();
    expect(el).toHaveAccessibleName(name);
    return name;
  });
}

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
    vi.mocked(dispatchInteraction).mockReset();
    vi.mocked(dispatchInteraction).mockResolvedValue(undefined);
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
    fireEvent.click(screen.getByRole("button", { name: "Increase the number of iterations" }));
    expect(box).toHaveValue("3");
    expect(
      screen.getByRole("button", { name: "Increase the number of iterations" }),
    ).toBeDisabled();

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

  // The engine publishes one element per count, and the modal renders the one whose count the
  // player picked — verbatim, never rescaled. The two elements below are DELIBERATELY
  // non-proportional to their counts: a component that rescaled the count-4 element to 2 would
  // show -20, and one that ignored the picker would still show -40.
  it("renders the element matching the picked count and never rescales it", () => {
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 4 } } }),
      {},
      shortcutInteraction({
        count: { type: "fixed", data: { min: 1, max: 4, suggested: 4 } },
        preview: [
          {
            count: 2,
            entries: [
              { family: "life", player: 1, amount: -7 },
              { family: "mana", player: null, amount: 3 },
            ],
          },
          {
            count: 4,
            entries: [
              { family: "life", player: 1, amount: -40 },
              { family: "mana", player: null, amount: 12 },
            ],
          },
        ],
      }),
    );
    render(<DeclareShortcutModal />);

    expect(screen.getByText("Repeating 4 times produces:")).toBeInTheDocument();
    expect(screen.getByText("-40 life — P2")).toBeInTheDocument();
    expect(screen.getByText("12 mana")).toBeInTheDocument();

    fireEvent.change(screen.getByRole("spinbutton"), { target: { value: "2" } });
    expect(screen.getByText("Repeating 2 times produces:")).toBeInTheDocument();
    expect(screen.getByText("-7 life — P2")).toBeInTheDocument();
    expect(screen.getByText("3 mana")).toBeInTheDocument();
    expect(screen.queryByText("-40 life — P2")).toBeNull();
    expect(screen.queryByText("-20 life — P2")).toBeNull();
  });

  // The engine samples the count window, so a count inside it may carry no element. The match is
  // exact: neither neighbour's magnitudes may leak in, and nothing may be interpolated between
  // them. The paired positive is the same spec at a count that IS published.
  it("renders no preview lines for a picked count the engine did not publish", () => {
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 4 } } }),
      {},
      shortcutInteraction({
        count: { type: "fixed", data: { min: 1, max: 4, suggested: 4 } },
        preview: [
          { count: 1, entries: [{ family: "life", player: 1, amount: -5 }] },
          { count: 4, entries: [{ family: "life", player: 1, amount: -40 }] },
        ],
      }),
    );
    render(<DeclareShortcutModal />);

    // Paired positive: the suggested count IS published and renders.
    expect(screen.getByText("-40 life — P2")).toBeInTheDocument();

    fireEvent.change(screen.getByRole("spinbutton"), { target: { value: "3" } });
    expect(screen.queryByText(/produces:/)).toBeNull();
    expect(screen.queryByText("-5 life — P2")).toBeNull();
    expect(screen.queryByText("-40 life — P2")).toBeNull();
    expect(screen.queryByText("-15 life — P2")).toBeNull();
  });

  // An offer that publishes no magnitudes at all renders no preview block, paired against the
  // same seed carrying one element.
  it("renders no preview block when the engine published no elements", () => {
    const offer = (preview: ShortcutSpec["preview"]) =>
      shortcutInteraction({
        count: { type: "fixed", data: { min: 1, max: 4, suggested: 4 } },
        preview,
      });
    seed(buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 4 } } }), {}, offer([]));
    render(<DeclareShortcutModal />);
    expect(screen.queryByText(/produces:/)).toBeNull();

    cleanup();
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 4 } } }),
      {},
      offer([{ count: 4, entries: [{ family: "life", player: 1, amount: -40 }] }]),
    );
    render(<DeclareShortcutModal />);
    expect(screen.getByText("-40 life — P2")).toBeInTheDocument();
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

  // ── CR 732.2b: what the responding opponent SEES ─────────────────────────────────────────
  //
  // T4 and T5 above stay UNMODIFIED: they seed `viewerInteraction: null`, which is the degrade
  // path, and they are the standing instrument that the base three lines are the shipped ones.

  /** The allocation rows in render order. `getAllByText` throws on an empty match, which is the
   *  query's own control against an assertion satisfied by rendering nothing. */
  const allocationRows = () =>
    screen.getAllByText(/^P\d+ — ×\d+$/).map((node) => node.textContent);

  // Every rendered magnitude is a direct read of a published entry. The discriminator is
  // the MUTATION: the same seed re-rendered with ONE published entry changed must render a
  // DIFFERENT specific string. A client-side recomputation reproduces the first value in both
  // renders and fails the second half. -13 and -29 are reachable by no arithmetic over the
  // other published fields (count 7, segments 2 and 5).
  //
  // CR 732.2a + CR 732.2b: the allocation rows are asserted IN ORDER — that order IS the
  // announced sequence for the decision they partition, which is why the modal states no order
  // lines beside them, and it is the object the responder names a place in. Distinct seats and
  // distinct magnitudes, so a reversal is a different list.
  it("renders the published element's magnitude, and follows it when it changes", () => {
    const render_at = (amount: number) => {
      cleanup();
      seed(
        buildRespondToShortcutWaitingFor(),
        {},
        respondInteraction(
          {
            points: [statementTargetsPoint(0, ["r0", "r1"])],
            declared: element(
              7,
              [amt("r0", 2), amt("r1", 5)],
              [{ family: "life", player: 1, amount }],
            ),
            allocationGroup: 0,
          },
          [seatCandidate("r0", 1), seatCandidate("r1", 2)],
        ),
      );
      render(<RespondToShortcutModal />);
    };

    render_at(-13);
    expect(screen.getByText("-13 life — P2")).toBeInTheDocument();
    expect(screen.getByText("Proposed declaration:")).toBeInTheDocument();
    expect(allocationRows()).toEqual(["P2 — ×2", "P3 — ×5"]);

    render_at(-29);
    expect(screen.getByText("-29 life — P2")).toBeInTheDocument();
    expect(screen.queryByText("-13 life — P2")).not.toBeInTheDocument();
    // The partition is unchanged, so the magnitude moved BECAUSE the published entry did —
    // not because anything the modal computes from the allocation did.
    expect(allocationRows()).toEqual(["P2 — ×2", "P3 — ×5"]);
  });

  // The degrade half: a respond window whose spec carries NO declared sequence renders the base
  // three lines unchanged and adds nothing. Paired with the row above, the two are a SWITCH.
  it("renders the base three lines and nothing else without a declared sequence", () => {
    seed(buildRespondToShortcutWaitingFor(), {}, respondInteraction());
    render(<RespondToShortcutModal />);

    expect(screen.getByText("This loop deals lethal damage.")).toBeInTheDocument();
    expect(screen.getByText("Repeat until the game ends.")).toBeInTheDocument();
    expect(screen.queryByText("Proposed declaration:")).not.toBeInTheDocument();
    expect(screen.queryByText(/^Repeating /)).not.toBeInTheDocument();
  });

  // Hostile: an ORDER-ONLY declaration — points but no element — renders the announcement
  // order and no magnitude. The order is asserted by its rendered position numbers, so a
  // renderer that reversed or sorted it fails.
  it("renders an order-only declaration as an order and no magnitudes", () => {
    seed(
      buildRespondToShortcutWaitingFor(),
      {},
      respondInteraction({ points: [statementTargetsPoint(0, ["r0", "r1"])] }, [
        seatCandidate("r0", 2),
        seatCandidate("r1", 0),
      ]),
    );
    render(<RespondToShortcutModal />);

    expect(screen.getByText("1. P3")).toBeInTheDocument();
    expect(screen.getByText("2. P1")).toBeInTheDocument();
    expect(screen.queryByText("2. P3")).not.toBeInTheDocument();
    expect(screen.queryByText(/^Repeating /)).not.toBeInTheDocument();
    expect(screen.queryByText(/ — ×/)).not.toBeInTheDocument();
  });

  // Hostile: TWO announced-target decisions. The allocation is stated over the one the engine
  // NAMES — so that decision's order is already the allocation lines' own order — and every other
  // one carries no allocation and must state its order. A renderer reading only the first
  // `targets` point shows the responder half the proposal, which is the object CR 732.2b gives
  // them a right to shorten.
  it("states every announced-target decision, not just the first", () => {
    const points = [statementTargetsPoint(0, ["r0", "r1"]), statementTargetsPoint(1, ["r2", "r3"])];
    const candidates = [
      seatCandidate("r0", 1),
      seatCandidate("r1", 2),
      seatCandidate("r2", 3),
      seatCandidate("r3", 0),
    ];

    seed(
      buildRespondToShortcutWaitingFor(),
      {},
      respondInteraction(
        { points, declared: element(7, [amt("r0", 2), amt("r1", 5)]), allocationGroup: 0 },
        candidates,
      ),
    );
    render(<RespondToShortcutModal />);

    // The first decision reaches the responder as the allocation, in its published order.
    expect(screen.getByText("P2 — ×2")).toBeInTheDocument();
    expect(screen.getByText("P3 — ×5")).toBeInTheDocument();
    // The second reaches it as an ORDER — the assertion that fails before the fix, where a
    // published allocation suppressed every order line.
    expect(screen.getByText("1. P4")).toBeInTheDocument();
    expect(screen.getByText("2. P1")).toBeInTheDocument();
    // And the allocated decision is not restated as an order beside its own partition.
    expect(screen.queryByText("1. P2")).not.toBeInTheDocument();
    expect(screen.queryByText("2. P3")).not.toBeInTheDocument();

    // The other half of the same reduction: with NO allocation to state, both decisions publish
    // an order and both must render — a renderer reading `points.find(…)` renders only the first.
    cleanup();
    seed(buildRespondToShortcutWaitingFor(), {}, respondInteraction({ points }, candidates));
    render(<RespondToShortcutModal />);

    expect(screen.getByText("1. P2")).toBeInTheDocument();
    expect(screen.getByText("2. P3")).toBeInTheDocument();
    expect(screen.getByText("1. P4")).toBeInTheDocument();
    expect(screen.getByText("2. P1")).toBeInTheDocument();
  });

  // Hostile: the allocation is stated over the SECOND announced-target decision. Only the
  // published group separates that from the first, so a renderer dropping the allocated decision
  // by position states the wrong decision's order and withholds the right one's.
  it("drops the order of the decision the engine names, not the first one", () => {
    seed(
      buildRespondToShortcutWaitingFor(),
      {},
      respondInteraction(
        {
          points: [
            statementTargetsPoint(0, ["r0", "r1"]),
            statementTargetsPoint(1, ["r2", "r3"]),
          ],
          declared: element(7, [amt("r2", 3), amt("r3", 4)]),
          allocationGroup: 1,
        },
        [
          seatCandidate("r0", 1),
          seatCandidate("r1", 2),
          seatCandidate("r2", 3),
          seatCandidate("r3", 0),
        ],
      ),
    );
    render(<RespondToShortcutModal />);

    expect(allocationRows()).toEqual(["P4 — ×3", "P1 — ×4"]);
    // The decision with no allocation over it states its order...
    expect(screen.getByText("1. P2")).toBeInTheDocument();
    expect(screen.getByText("2. P3")).toBeInTheDocument();
    // ...and the allocated one is not restated as an order beside its own partition.
    expect(screen.queryByText("1. P4")).not.toBeInTheDocument();
    expect(screen.queryByText("2. P1")).not.toBeInTheDocument();
  });

  // Hostile: a declared element with EMPTY entries renders no preview block — the shared
  // `PreviewLines` already states nothing for one — but still states the partition. "Renders
  // nothing" cannot pass for "renders the right nothing".
  it("renders the partition for an element carrying no magnitudes", () => {
    seed(
      buildRespondToShortcutWaitingFor(),
      {},
      respondInteraction(
        {
          points: [statementTargetsPoint(0, ["r0", "r1"])],
          declared: element(7, [amt("r0", 2), amt("r1", 5)], []),
          allocationGroup: 0,
        },
        [seatCandidate("r0", 1), seatCandidate("r1", 2)],
      ),
    );
    render(<RespondToShortcutModal />);

    expect(screen.getByText("P2 — ×2")).toBeInTheDocument();
    expect(screen.getByText("P3 — ×5")).toBeInTheDocument();
    expect(screen.queryByText(/^Repeating /)).not.toBeInTheDocument();
  });

  // The client half of the answered-optional-decision claim. An engine test renders nothing, and
  // the claim is about a RENDER: a component that states "taken" for every optional decision
  // passes a uniform fixture and fails this one. Both lines are positive DOM reads by their full
  // text, so "renders nothing twice" cannot satisfy the difference.
  it("renders each answered optional decision with its OWN answer", () => {
    seed(
      buildRespondToShortcutWaitingFor(),
      {},
      respondInteraction(
        {
          points: [
            statementMayPoint(0, "s0", "a0"),
            statementMayPoint(1, "s1", "a1"),
          ],
        },
        [
          objectCandidate("s0", "Sue Storm", "402"),
          ...mayCandidates("a0", "unused-decline"),
          objectCandidate("s1", "Reed Richards", "401"),
          ...mayCandidates("unused-take", "a1"),
        ],
      ),
    );
    render(<RespondToShortcutModal />);

    const taken = screen.getByText("Sue Storm — taken each iteration");
    const declined = screen.getByText("Reed Richards — declined each iteration");
    expect(taken).toBeInTheDocument();
    expect(declined).toBeInTheDocument();
    expect(taken.textContent).not.toEqual(declined.textContent);
    // The uniform sibling: a component keying the wording off anything but each point's own
    // answer candidate would render this string twice, and it must render it zero times here.
    expect(screen.queryByText("Reed Richards — taken each iteration")).not.toBeInTheDocument();
  });

  // The uniform sibling, so the difference above is a BRANCH rather than a shape the component
  // always emits.
  it("renders two identical answers when the declaration answered both the same way", () => {
    seed(
      buildRespondToShortcutWaitingFor(),
      {},
      respondInteraction(
        {
          points: [
            statementMayPoint(0, "s0", "a0"),
            statementMayPoint(1, "s1", "a1"),
          ],
        },
        [
          objectCandidate("s0", "Sue Storm", "402"),
          ...mayCandidates("a0", "unused-decline"),
          objectCandidate("s1", "Reed Richards", "401"),
          ...mayCandidates("a1", "unused-decline-2"),
        ],
      ),
    );
    render(<RespondToShortcutModal />);

    expect(screen.getByText("Sue Storm — taken each iteration")).toBeInTheDocument();
    expect(screen.getByText("Reed Richards — taken each iteration")).toBeInTheDocument();
    expect(screen.queryByText(/declined each iteration/)).not.toBeInTheDocument();
  });

  // Hostile: the ONLY declaration content is a may point whose answer this modal has no wording
  // for, so every row it could render is dropped and the box would be a title over nothing.
  it("omits the declaration panel when no may row survives", () => {
    seed(
      buildRespondToShortcutWaitingFor(),
      {},
      respondInteraction({ points: [statementMayPoint(0, "s0", "a0")] }, [
        objectCandidate("s0", "Sue Storm", "402"),
        mayAnswer("a0", "abstain"),
      ]),
    );
    render(<RespondToShortcutModal />);

    expect(screen.queryByText("Proposed declaration:")).not.toBeInTheDocument();
  });

  // The paired positive, one literal apart: the same fixture with a whitelisted answer keeps the
  // title AND its row, so a renderer that dropped the panel unconditionally fails here.
  it("keeps the declaration panel when its one may row survives", () => {
    seed(
      buildRespondToShortcutWaitingFor(),
      {},
      respondInteraction({ points: [statementMayPoint(0, "s0", "a0")] }, [
        objectCandidate("s0", "Sue Storm", "402"),
        mayAnswer("a0", "take"),
      ]),
    );
    render(<RespondToShortcutModal />);

    expect(screen.getByText("Proposed declaration:")).toBeInTheDocument();
    expect(screen.getByText("Sue Storm — taken each iteration")).toBeInTheDocument();
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

  // ═══ The pin-ingress declaration UI ═══════════════════════════════════════════════════════
  //
  // What these rows prove: the modal SENDS what it renders. What they cannot prove: that the
  // engine accepts it — a frontend suite cannot drive the engine. That link is type-level,
  // through the generated bindings, plus the engine-side adapter-contract fixture.

  // P5-1: the dispatched pin carries the SELECTED element's published allocation verbatim. The
  // fixture's split is deliberately non-even, so no plausible client-side rule reproduces it —
  // an even split of 5 over these candidates is not [4,1].
  it("dispatches the published allocation verbatim on a pointed Fixed offer (P5-1)", () => {
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 5 } } }),
      {},
      shortcutInteraction(
        {
          count: fixedCount(1, 5, 5),
          points: [targetsPoint(2, ["k4", "k5", "k6"])],
          preview: [
            element(5, [amt("k4", 4), amt("k5", 1)], [{ family: "life", player: 1, amount: -5 }]),
          ],
        },
        "session.0.1",
        [seatCandidate("k4", 1), seatCandidate("k5", 2), seatCandidate("k6", 3)],
      ),
    );
    render(<DeclareShortcutModal />);

    fireEvent.click(confirmButton());

    expect(dispatchInteraction).toHaveBeenCalledWith({
      interactionId: "session.0.1",
      response: {
        type: "shortcut",
        data: {
          decision: { type: "fixed", data: { iterations: 5 } },
          pins: [
            {
              group: 2,
              choiceIds: ["k4", "k5"],
              amounts: [
                { choiceId: "k4", amount: 4 },
                { choiceId: "k5", amount: 1 },
              ],
            },
          ],
        },
      },
    });
    // Reach-guard: more than one segment went out, so an equality between two empty lists cannot
    // satisfy this row.
    expect(submittedPins()[0].amounts).toHaveLength(2);
    expect(dispatchMock).not.toHaveBeenCalled();
  });

  // P5-2: an authored distribution dispatches as authored, the per-seat LIFE lines go with it,
  // and the invariant families stay. Leg A is legs B/C's paired positive — a state change, not a
  // missing element — and the even-split button proves the gate two-way.
  it("hides the previewed life lines for an authored split and keeps the badges (P5-2)", () => {
    seed(
      buildLoopShortcutWaitingFor({
        schema: { iteration_count: { Fixed: 5 } },
        // Two axes deduping to two display families, so leg C's invariance cannot be satisfied by
        // a 1-vs-0 coincidence.
        certificate: { unbounded: [{ DamageDealt: 1 }, "TokensCreated"] },
      }),
      {},
      shortcutInteraction(
        {
          count: fixedCount(1, 5, 5),
          points: [targetsPoint(2, ["k4", "k5"])],
          preview: [
            element(5, [amt("k4", 4), amt("k5", 1)], [{ family: "life", player: 1, amount: -2 }]),
          ],
        },
        "session.0.1",
        [seatCandidate("k4", 1), seatCandidate("k5", 2)],
      ),
    );
    render(<DeclareShortcutModal />);

    // leg A — unedited: the published split renders, and so do its life lines.
    expect(screen.getByText("-2 life — P2")).toBeInTheDocument();
    const imgCount = screen.getAllByRole("img").length;
    expect(imgCount).toBeGreaterThan(1);
    expect(screen.queryByText(/custom distribution/i)).toBeNull();

    // leg B — authored away from the published split.
    fireEvent.change(allocationRow("P2"), { target: { value: "3" } });
    fireEvent.change(allocationRow("P3"), { target: { value: "2" } });
    expect(screen.queryByText("-2 life — P2")).toBeNull();
    expect(screen.getByText(/custom distribution/i)).toBeInTheDocument();
    fireEvent.click(confirmButton());
    expect(submittedPins()).toEqual([
      { group: 2, choiceIds: ["k4", "k5"], amounts: [amt("k4", 3), amt("k5", 2)] },
    ]);

    // leg C — the survivors: the family badges and the player's own count are untouched.
    expect(screen.getAllByRole("img")).toHaveLength(imgCount);
    expect(countBox()).toHaveValue("5");

    // Hostile sibling: clearing the edit restores the published split, so the gate is two-way.
    fireEvent.click(screen.getByRole("button", { name: "Reset to the even split" }));
    expect(screen.getByText("-2 life — P2")).toBeInTheDocument();
    vi.mocked(dispatchInteraction).mockClear();
    fireEvent.click(confirmButton());
    expect(submittedPins()).toEqual([
      { group: 2, choiceIds: ["k4", "k5"], amounts: [amt("k4", 4), amt("k5", 1)] },
    ]);
  });

  // P5-3: the same route on a one-segment allocation — a POSITIVE row, not an exception.
  it("takes the pin route with a single published victim (P5-3)", () => {
    const offer = (candidateIds: string[]) =>
      shortcutInteraction(
        {
          count: fixedCount(1, 7, 7),
          points: [targetsPoint(2, candidateIds)],
          preview: [element(7, [amt("k4", 7)])],
        },
        "session.0.1",
        [seatCandidate("k4", 1)],
      );

    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 7 } } }),
      {},
      offer(["k4"]),
    );
    render(<DeclareShortcutModal />);
    fireEvent.click(confirmButton());
    expect(submittedPins()).toEqual([{ group: 2, choiceIds: ["k4"], amounts: [amt("k4", 7)] }]);
    expect(dispatchMock).not.toHaveBeenCalled();

    // Hostile sibling: the same point with no published candidate is not renderable, so the offer
    // keeps the count-only route.
    cleanup();
    vi.mocked(dispatchInteraction).mockClear();
    seed(buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 7 } } }), {}, offer([]));
    render(<DeclareShortcutModal />);
    fireEvent.click(confirmButton());
    expect(dispatchInteraction).not.toHaveBeenCalled();
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DeclareShortcut",
      data: { count: { Fixed: 7 }, template: null },
    });
  });

  // P5-4: every non-read-only point gets a pin, each from its OWN candidate list.
  it("pins every non-read-only point from its own candidate list (P5-4)", () => {
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 18 } } }),
      {},
      shortcutInteraction(
        {
          count: fixedCount(1, 18, 18),
          points: [
            mayPoint(0, ["m0take", "m0dec"]),
            mayPoint(1, ["m1take", "m1dec"]),
            targetsPoint(2, ["k4", "k5"]),
          ],
          preview: [element(18, [amt("k4", 9), amt("k5", 9)])],
        },
        "session.0.1",
        [
          ...mayCandidates("m0take", "m0dec"),
          ...mayCandidates("m1take", "m1dec"),
          seatCandidate("k4", 1),
          seatCandidate("k5", 2),
        ],
      ),
    );
    render(<DeclareShortcutModal />);

    // Reach-guard: a full pin set cannot pass on a modal that dispatches unconditionally.
    expect(confirmButton()).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Take optional ability 1" }));
    expect(confirmButton()).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Decline optional ability 2" }));
    expect(confirmButton()).toBeEnabled();

    fireEvent.click(confirmButton());
    expect(submittedPins()).toEqual([
      { group: 0, choiceIds: ["m0take"], amounts: [] },
      { group: 1, choiceIds: ["m1dec"], amounts: [] },
      { group: 2, choiceIds: ["k4", "k5"], amounts: [amt("k4", 9), amt("k5", 9)] },
    ]);
    expect(dispatchMock).not.toHaveBeenCalled();

    // Hostile sibling: picking the SAME option on both points must still send two different ids,
    // which is what shows each control reads its own point's list rather than a shared index.
    vi.mocked(dispatchInteraction).mockClear();
    fireEvent.click(screen.getByRole("button", { name: "Take optional ability 2" }));
    fireEvent.click(confirmButton());
    expect(submittedPins()).toEqual([
      { group: 0, choiceIds: ["m0take"], amounts: [] },
      { group: 1, choiceIds: ["m1take"], amounts: [] },
      { group: 2, choiceIds: ["k4", "k5"], amounts: [amt("k4", 9), amt("k5", 9)] },
    ]);
  });

  // P5-5: a point-free offer keeps the `GameAction` route, so the count-only path is shown live
  // rather than assumed so.
  it("keeps the GameAction route on a point-free offer (P5-5)", () => {
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 5 } } }),
      {},
      shortcutInteraction({ count: fixedCount(1, 5, 5) }),
    );
    render(<DeclareShortcutModal />);
    fireEvent.click(confirmButton());
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DeclareShortcut",
      data: { count: { Fixed: 5 }, template: null },
    });
    expect(dispatchInteraction).not.toHaveBeenCalled();
  });

  // P5-6: an offer whose published points are all read-only keeps the count-only route and sends
  // no pins. Both read-only kinds are covered, so the row covers the set rather than a member.
  it("keeps the GameAction route and sends no pins when every point is read-only (P5-6)", () => {
    for (const kind of ["convokeTaps", "manaColor"] as const) {
      seed(
        buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 5 } } }),
        {},
        shortcutInteraction({
          count: fixedCount(1, 5, 5),
          points: [readOnlyPoint(0, kind)],
          preview: [element(5, [])],
        }),
      );
      render(<DeclareShortcutModal />);
      fireEvent.click(confirmButton());
      expect(dispatchMock, kind).toHaveBeenCalledWith({
        type: "DeclareShortcut",
        data: { count: { Fixed: 5 }, template: null },
      });
      expect(dispatchInteraction, kind).not.toHaveBeenCalled();
      cleanup();
      dispatchMock.mockClear();
    }
  });

  // P5-7: the announce branch. CR 732.2c — an UntilLethal declaration names the ONE subject its
  // drive resolves at every repetition, so `choiceIds` holds exactly the selected id, `amounts`
  // is empty and no allocation box renders. The FIXED branch's own dispatch is asserted unchanged
  // by the allocation rows earlier in this file, so this is a branch change and not a control
  // deletion.
  it("announces the one selected subject on an UntilLethal offer with a targets point (P5-7)", () => {
    seed(
      buildLoopShortcutWaitingFor(),
      {},
      shortcutInteraction(
        { count: { type: "untilLethal" }, points: [targetsPoint(2, ["k4", "k5", "k6"])] },
        "session.0.1",
        [seatCandidate("k4", 1), seatCandidate("k5", 2), seatCandidate("k6", 3)],
      ),
    );
    render(<DeclareShortcutModal />);

    // No allocation amounts — and no count picker either, which is BASE behaviour on UntilLethal.
    // The positive control for this query is P5-1, where it finds the boxes.
    expect(screen.queryAllByRole("spinbutton")).toHaveLength(0);

    // NOTHING SELECTED: no client-side default, so Confirm refuses and nothing is dispatched.
    expect(confirmButton()).toBeDisabled();
    fireEvent.click(confirmButton());
    expect(dispatchInteraction).not.toHaveBeenCalled();
    expect(dispatchMock).not.toHaveBeenCalled();

    // Select the SECOND published candidate. A control that ignored the selection and dispatched
    // the head of `candidateIds` passes a first-candidate leg and fails this one.
    const announce = screen.getAllByRole("button", { name: ANNOUNCE });
    expect(announce).toHaveLength(3);
    fireEvent.click(announce[1]);
    expect(screen.getAllByRole("button", { name: ANNOUNCE })[1]).toHaveAttribute(
      "aria-pressed",
      "true",
    );

    fireEvent.click(confirmButton());
    expect(dispatchInteraction).toHaveBeenCalledWith({
      interactionId: "session.0.1",
      response: {
        type: "shortcut",
        data: {
          decision: { type: "acceptSuggested" },
          pins: [{ group: 2, choiceIds: ["k5"], amounts: [] }],
        },
      },
    });
    expect(dispatchMock).not.toHaveBeenCalled();
  });

  // P5-8: an unrenderable point keeps the `GameAction` route AND suppresses every control on the
  // renderable points beside it — a live control whose answer the count-only branch discards is
  // worse than no control. The mixed shapes below — an unrenderable point beside a renderable
  // one — are what make `pinRoute`'s renderability conjunct an `every` rather than a `some`.
  it("keeps the GameAction route on an unrenderable point and renders no may control (P5-8)", () => {
    const shapes: Array<[string, InteractionShortcutPoint[]]> = [
      ["mode", [unrenderablePoint(0, "mode")]],
      ["unlessBreak", [unrenderablePoint(0, "unlessBreak")]],
      ["multi-position targets", [targetsPoint(2, ["k4", "k5"], { max: 2 })]],
      ["mixed targets + mode", [targetsPoint(2, ["k4"]), unrenderablePoint(3, "mode")]],
      [
        "multi-position targets beside a may point",
        [targetsPoint(2, ["k4", "k5"], { max: 2 }), mayPoint(0, ["m0take", "m0dec"])],
      ],
    ];
    for (const [label, points] of shapes) {
      seed(
        buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 5 } } }),
        {},
        shortcutInteraction(
          { count: fixedCount(1, 5, 5), points, preview: [element(5, [amt("k4", 5)])] },
          "session.0.1",
          [seatCandidate("k4", 1), seatCandidate("k5", 2), ...mayCandidates("m0take", "m0dec")],
        ),
      );
      render(<DeclareShortcutModal />);
      // The positive control for this query is P5-4 and P5-15, where the identical query in this
      // same file FINDS these buttons on offers that do route.
      expect(screen.queryAllByRole("button", { name: /optional ability/i }), label).toHaveLength(0);
      fireEvent.click(confirmButton());
      expect(dispatchMock, label).toHaveBeenCalledWith({
        type: "DeclareShortcut",
        data: { count: { Fixed: 5 }, template: null },
      });
      expect(dispatchInteraction, label).not.toHaveBeenCalled();
      cleanup();
      dispatchMock.mockClear();
    }
  });

  // P5-9: an offer publishing no preview keeps the `GameAction` route.
  it("keeps the GameAction route when the offer publishes no preview (P5-9)", () => {
    const shapes: Array<[string, Partial<ShortcutSpec>, number]> = [
      ["isolating", { count: fixedCount(1, 5, 5), points: [targetsPoint(2, ["k4"])] }, 5],
      [
        "object-growth",
        {
          count: fixedCount(1, 1000, 1000),
          points: [targetsPoint(2, ["k4"]), targetsPoint(3, ["k5"]), readOnlyPoint(4, "manaColor")],
        },
        1000,
      ],
    ];
    for (const [label, spec, declared] of shapes) {
      seed(
        buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 5 } } }),
        {},
        shortcutInteraction(spec, "session.0.1", [seatCandidate("k4", 1), seatCandidate("k5", 2)]),
      );
      render(<DeclareShortcutModal />);
      expect(countBox(), label).toBeInTheDocument();
      fireEvent.click(confirmButton());
      expect(dispatchInteraction, label).not.toHaveBeenCalled();
      expect(dispatchMock, label).toHaveBeenCalledWith({
        type: "DeclareShortcut",
        data: { count: { Fixed: declared }, template: null },
      });
      cleanup();
      dispatchMock.mockClear();
    }

    // MANDATORY paired positive, in the same invocation: the isolating offer with a published
    // preview added must still reach the pin ingress, or a conjunct that refuses everything would
    // satisfy the shapes above vacuously.
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 5 } } }),
      {},
      shortcutInteraction(
        {
          count: fixedCount(1, 5, 5),
          points: [targetsPoint(2, ["k4"])],
          preview: [element(5, [amt("k4", 5)])],
        },
        "session.0.1",
        [seatCandidate("k4", 1)],
      ),
    );
    render(<DeclareShortcutModal />);
    fireEvent.click(confirmButton());
    expect(submittedPins()).toEqual([{ group: 2, choiceIds: ["k4"], amounts: [amt("k4", 5)] }]);
    expect(dispatchMock).not.toHaveBeenCalled();
  });

  // P5-10: a SECOND non-read-only targets point is unanswerable from published data — the
  // published allocation names the first point's candidates and nothing else — so the whole offer
  // keeps the count-only route rather than sending a pin for it.
  it("keeps the GameAction route when a second targets point is published (P5-10)", () => {
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 5 } } }),
      {},
      shortcutInteraction(
        {
          count: fixedCount(1, 5, 5),
          // Disjoint candidate sets: a shared-`effective` implementation cannot pass by
          // coincidence, because the second pin's ids are not in the first point's list.
          points: [targetsPoint(2, ["k4", "k5"]), targetsPoint(3, ["k6", "k7"])],
          preview: [element(5, [amt("k4", 3), amt("k5", 2)])],
        },
        "session.0.1",
        [
          seatCandidate("k4", 1),
          seatCandidate("k5", 2),
          seatCandidate("k6", 3),
          seatCandidate("k7", 0),
        ],
      ),
    );
    render(<DeclareShortcutModal />);
    fireEvent.click(confirmButton());
    expect(dispatchInteraction).not.toHaveBeenCalled();
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DeclareShortcut",
      data: { count: { Fixed: 5 }, template: null },
    });
  });

  // P5-11: no hand-derived bound, per surface, and the row state parses-and-rejects rather than
  // clamping. Each allocation row's ceiling is the PICKED count and moves with it.
  it("reads both windows from the engine and refuses rather than clamping (P5-11)", () => {
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 5 } } }),
      {},
      shortcutInteraction(
        {
          count: fixedCount(1, 5, 5),
          points: [targetsPoint(2, ["k4", "k5"])],
          preview: [
            element(5, [amt("k4", 3), amt("k5", 2)]),
            element(3, [amt("k4", 2), amt("k5", 1)]),
          ],
        },
        "session.0.1",
        [seatCandidate("k4", 1), seatCandidate("k5", 2)],
      ),
    );
    render(<DeclareShortcutModal />);

    // leg A — both windows are the engine's.
    expect(countBox()).toHaveAttribute("aria-valuemin", "1");
    expect(countBox()).toHaveAttribute("aria-valuemax", "5");
    expect(allocationRow("P2")).toHaveAttribute("aria-valuemax", "5");
    fireEvent.change(countBox(), { target: { value: "3" } });
    expect(allocationRow("P2")).toHaveAttribute("aria-valuemax", "3");
    fireEvent.change(countBox(), { target: { value: "5" } });

    // leg B — an out-of-window row entry is REFUSED and left VISIBLE, never coerced into range.
    fireEvent.change(allocationRow("P2"), { target: { value: "6" } });
    expect(allocationRow("P2")).toHaveValue("6");
    expect(allocationRow("P2")).toHaveAttribute("aria-invalid", "true");
    expect(confirmButton()).toBeDisabled();
    fireEvent.click(confirmButton());
    expect(dispatchInteraction).not.toHaveBeenCalled();
    expect(dispatchMock).not.toHaveBeenCalled();

    // Paired positive in the same row: a legal partition re-enables Confirm and is what goes out,
    // so a modal whose Confirm never enables cannot satisfy leg B.
    fireEvent.change(allocationRow("P2"), { target: { value: "4" } });
    fireEvent.change(allocationRow("P3"), { target: { value: "1" } });
    expect(confirmButton()).toBeEnabled();
    fireEvent.click(confirmButton());
    expect(submittedPins()).toEqual([
      { group: 2, choiceIds: ["k4", "k5"], amounts: [amt("k4", 4), amt("k5", 1)] },
    ]);
  });

  // P5-13: offer rotation clears every new piece of state, through the existing `key={offerId}`.
  // Two legs because the allocation and announce controls are mutually exclusive on one offer: a
  // Fixed offer carries allocation + may, an UntilLethal one carries announce + may.
  //
  // ⚠ Same warning as the rows above that assert offer A's typed count does not survive into
  // offer B: `view.rerender`, never a second `render`.
  it("clears the allocation, the may pick and the announced subject on a new offer (P5-13)", () => {
    const fixedOffer = (interactionId: string) =>
      shortcutInteraction(
        {
          count: fixedCount(1, 5, 5),
          points: [mayPoint(0, ["m0take", "m0dec"]), targetsPoint(2, ["k4", "k5"])],
          preview: [element(5, [amt("k4", 3), amt("k5", 2)])],
        },
        interactionId,
        [...mayCandidates("m0take", "m0dec"), seatCandidate("k4", 1), seatCandidate("k5", 2)],
      );

    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 5 } } }),
      {},
      fixedOffer("session.0.1"),
    );
    const view = render(<DeclareShortcutModal />);

    // Positive reach-guard: the edits actually LANDED, so "back to default" cannot pass on a
    // control that never accepted input.
    fireEvent.change(allocationRow("P2"), { target: { value: "1" } });
    fireEvent.click(screen.getByRole("button", { name: "Take optional ability 1" }));
    expect(allocationRow("P2")).toHaveValue("1");
    expect(screen.getByRole("button", { name: "Take optional ability 1" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );

    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 5 } } }),
      {},
      fixedOffer("session.0.2"),
    );
    view.rerender(<DeclareShortcutModal />);
    expect(allocationRow("P2")).toHaveValue("3");
    expect(screen.getByRole("button", { name: "Take optional ability 1" })).toHaveAttribute(
      "aria-pressed",
      "false",
    );
    cleanup();

    const announceOffer = (interactionId: string) =>
      shortcutInteraction(
        {
          count: { type: "untilLethal" },
          points: [mayPoint(0, ["m0take", "m0dec"]), targetsPoint(2, ["k4", "k5"])],
        },
        interactionId,
        [...mayCandidates("m0take", "m0dec"), seatCandidate("k4", 1), seatCandidate("k5", 2)],
      );
    seed(buildLoopShortcutWaitingFor(), {}, announceOffer("session.0.3"));
    const announceView = render(<DeclareShortcutModal />);

    const pressed = () =>
      screen
        .getAllByRole("button", { name: ANNOUNCE })
        .map((b) => b.getAttribute("aria-pressed"));
    // Positive reach-guard: the selection actually LANDED, so "back to nothing selected" cannot
    // pass on a control that never accepted input.
    fireEvent.click(screen.getAllByRole("button", { name: ANNOUNCE })[1]);
    expect(pressed()).toEqual(["false", "true"]);

    seed(buildLoopShortcutWaitingFor(), {}, announceOffer("session.0.4"));
    announceView.rerender(<DeclareShortcutModal />);
    expect(pressed()).toEqual(["false", "false"]);
  });

  // P5-14: the seat gate runs ABOVE the routing branch, so a full three-point projection renders
  // nothing for a non-actor seat. Paired with P5-4's identical projection at proposer 0.
  it("renders nothing for a non-actor seat on the pin route too (P5-14)", () => {
    seed(
      buildLoopShortcutWaitingFor({ proposer: 1, schema: { iteration_count: { Fixed: 18 } } }),
      { turn_decision_controller: null, active_player: 0 },
      shortcutInteraction(
        {
          count: fixedCount(1, 18, 18),
          points: [
            mayPoint(0, ["m0take", "m0dec"]),
            mayPoint(1, ["m1take", "m1dec"]),
            targetsPoint(2, ["k4", "k5"]),
          ],
          preview: [element(18, [amt("k4", 9), amt("k5", 9)])],
        },
        "session.0.1",
        [
          ...mayCandidates("m0take", "m0dec"),
          ...mayCandidates("m1take", "m1dec"),
          seatCandidate("k4", 1),
          seatCandidate("k5", 2),
        ],
      ),
    );

    const { container } = render(<DeclareShortcutModal />);
    expect(container.firstChild).toBeNull();
    expect(dispatchMock).not.toHaveBeenCalled();
    expect(dispatchInteraction).not.toHaveBeenCalled();
  });

  // P5-15: a bounded MAY-ONLY offer routes to the pin ingress and answers its may points, with no
  // allocation control. This is the shape the routing rule's placement decides: the
  // `targetsControl !== null` test belongs to `renderable`'s targets arm, never to `pinRoute` —
  // as a conjunct there it would send this whole class to the count-only path.
  it("routes a bounded may-only offer and answers its may points (P5-15)", () => {
    const offer = (mayIds: string[]) =>
      shortcutInteraction(
        {
          count: fixedCount(1, 18, 18),
          points: [mayPoint(0, mayIds.slice(0, 2)), mayPoint(1, mayIds.slice(2))],
          preview: [element(18, [], [{ family: "life", player: 1, amount: -9 }])],
        },
        "session.0.1",
        [...mayCandidates("m0take", "m0dec"), ...mayCandidates("m1take", "m1dec")],
      );

    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 18 } } }),
      {},
      offer(["m0take", "m0dec", "m1take", "m1dec"]),
    );
    render(<DeclareShortcutModal />);

    // Reach-guard: the modal rendered its published state rather than nothing.
    expect(screen.getByText("-9 life — P2")).toBeInTheDocument();
    // The allocation control is absent. The name filter is required: the count picker is itself a
    // spinbutton on a Fixed offer. Positive control for this query: P5-1 and P5-11 find them.
    expect(screen.queryAllByRole("spinbutton", { name: /repetitions for/i })).toHaveLength(0);

    expect(confirmButton()).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Take optional ability 1" }));
    expect(confirmButton()).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Decline optional ability 2" }));
    expect(confirmButton()).toBeEnabled();

    fireEvent.click(confirmButton());
    expect(dispatchInteraction).toHaveBeenCalledWith({
      interactionId: "session.0.1",
      response: {
        type: "shortcut",
        data: {
          decision: { type: "fixed", data: { iterations: 18 } },
          pins: [
            { group: 0, choiceIds: ["m0take"], amounts: [] },
            { group: 1, choiceIds: ["m1dec"], amounts: [] },
          ],
        },
      },
    });
    expect(dispatchMock).not.toHaveBeenCalled();

    // Admitted-member hunt against a mayChoice arm that ignores its own domain: the same shape
    // with both points' candidate lists emptied is not renderable, so it keeps BASE behaviour.
    cleanup();
    vi.mocked(dispatchInteraction).mockClear();
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 18 } } }),
      {},
      shortcutInteraction({
        count: fixedCount(1, 18, 18),
        points: [mayPoint(0, []), mayPoint(1, [])],
        preview: [element(18, [])],
      }),
    );
    render(<DeclareShortcutModal />);
    fireEvent.click(confirmButton());
    expect(dispatchInteraction).not.toHaveBeenCalled();
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DeclareShortcut",
      data: { count: { Fixed: 18 }, template: null },
    });
  });

  // P5-16: `targetsControl` is computed AFTER `allocationPoint` and is null when it is absent, so
  // an UntilLethal offer with no targets point renders NO announce control and still answers its
  // may points. Together with P5-7 this shows the announce control follows the POINT, not the
  // count spec.
  it("renders no announce control on an UntilLethal offer with no targets point (P5-16)", () => {
    seed(
      buildLoopShortcutWaitingFor(),
      {},
      shortcutInteraction(
        {
          count: { type: "untilLethal" },
          points: [mayPoint(0, ["m0take", "m0dec"]), mayPoint(1, ["m1take", "m1dec"])],
        },
        "session.0.1",
        [...mayCandidates("m0take", "m0dec"), ...mayCandidates("m1take", "m1dec")],
      ),
    );
    render(<DeclareShortcutModal />);

    // Positive control for this query: P5-7, where it finds the announce buttons on an
    // UntilLethal offer that DOES publish a targets point.
    expect(screen.queryAllByRole("button", { name: ANNOUNCE })).toHaveLength(0);

    fireEvent.click(screen.getByRole("button", { name: "Take optional ability 1" }));
    fireEvent.click(screen.getByRole("button", { name: "Take optional ability 2" }));
    fireEvent.click(confirmButton());
    expect(dispatchInteraction).toHaveBeenCalledWith({
      interactionId: "session.0.1",
      response: {
        type: "shortcut",
        data: {
          decision: { type: "acceptSuggested" },
          pins: [
            { group: 0, choiceIds: ["m0take"], amounts: [] },
            { group: 1, choiceIds: ["m1take"], amounts: [] },
          ],
        },
      },
    });
    expect(dispatchMock).not.toHaveBeenCalled();
  });

  // P5-17: a count the offer published no element for renders zeros and refuses Confirm — no
  // seeded split and no nearest match. Authoring a partition there is what makes it a rendered
  // state, not a dead end.
  it("seeds nothing at a count the engine published no element for (P5-17)", () => {
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 8 } } }),
      {},
      shortcutInteraction(
        {
          count: fixedCount(1, 8, 8),
          points: [targetsPoint(2, ["k4", "k5"])],
          // The window's endpoints only — 5 is unsampled.
          preview: [element(1, [amt("k4", 1)]), element(8, [amt("k4", 4), amt("k5", 4)])],
        },
        "session.0.1",
        [seatCandidate("k4", 1), seatCandidate("k5", 2)],
      ),
    );
    render(<DeclareShortcutModal />);

    // leg A — at the published count the rows read the published split and Confirm is enabled, so
    // a modal that never enables cannot satisfy this row.
    expect(allocationRow("P2")).toHaveAttribute("aria-valuenow", "4");
    expect(allocationRow("P3")).toHaveAttribute("aria-valuenow", "4");
    expect(confirmButton()).toBeEnabled();

    // leg B — the gap. Nothing is seeded from anywhere.
    fireEvent.change(countBox(), { target: { value: "5" } });
    expect(allocationRow("P2")).toHaveAttribute("aria-valuenow", "0");
    expect(allocationRow("P3")).toHaveAttribute("aria-valuenow", "0");
    expect(screen.queryByText(/produces:/)).toBeNull();
    expect(confirmButton()).toBeDisabled();
    fireEvent.click(confirmButton());
    expect(dispatchInteraction).not.toHaveBeenCalled();
    expect(dispatchMock).not.toHaveBeenCalled();

    // leg C — authoring a partition there enables Confirm and dispatches it, under the count the
    // player picked.
    fireEvent.change(allocationRow("P2"), { target: { value: "3" } });
    fireEvent.change(allocationRow("P3"), { target: { value: "2" } });
    expect(screen.getByText(/custom distribution/i)).toBeInTheDocument();
    expect(confirmButton()).toBeEnabled();
    fireEvent.click(confirmButton());
    expect(vi.mocked(dispatchInteraction).mock.calls[0][0].response).toEqual({
      type: "shortcut",
      data: {
        decision: { type: "fixed", data: { iterations: 5 } },
        pins: [{ group: 2, choiceIds: ["k4", "k5"], amounts: [amt("k4", 3), amt("k5", 2)] }],
      },
    });

    // leg D — the sibling that separates "no element" from "a SHORT allocation": at count 1 an
    // element exists carrying one segment, so the zero row is dropped by the positive-parts
    // filter rather than declared.
    vi.mocked(dispatchInteraction).mockClear();
    fireEvent.change(countBox(), { target: { value: "1" } });
    expect(allocationRow("P2")).toHaveAttribute("aria-valuenow", "1");
    expect(allocationRow("P3")).toHaveAttribute("aria-valuenow", "0");
    expect(confirmButton()).toBeEnabled();
    fireEvent.click(confirmButton());
    expect(submittedPins()).toEqual([{ group: 2, choiceIds: ["k4"], amounts: [amt("k4", 1)] }]);
  });

  // P5-18: the candidate label renders a player seat, an object's RAW name, and the published
  // reference when that name is null. The object fixture's name is deliberately a real key path
  // in `en/game.json`, so a `t()` passthrough would be visible as "Take the shortcut" instead of
  // the raw string.
  it("labels player, object and unnamed-object candidates (P5-18)", () => {
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 3 } } }),
      {},
      shortcutInteraction(
        {
          count: fixedCount(1, 3, 3),
          points: [targetsPoint(2, ["k4", "k5", "k6"])],
          preview: [element(3, [amt("k4", 1), amt("k5", 1), amt("k6", 1)])],
        },
        "session.0.1",
        [
          seatCandidate("k4", 1),
          objectCandidate("k5", "comboShortcut.confirm", "obj-55"),
          objectCandidate("k6", null, "obj-77"),
        ],
      ),
    );
    render(<DeclareShortcutModal />);

    // The player arm is the paired positive.
    expect(
      screen.getByRole("spinbutton", { name: "Repetitions for P2" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("spinbutton", { name: "Repetitions for comboShortcut.confirm" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("spinbutton", { name: "Repetitions for obj-77" }),
    ).toBeInTheDocument();
  });

  // P5-19: on the pin route the KEYBOARD entry point refuses in exactly the state the button
  // does, and mints no count the player did not type. `AmountInput` calls `onSubmit`
  // unconditionally on Enter and deliberately does not re-guard, so the refusal has to sit at the
  // top of the handler — a row that clicks a disabled button cannot see this.
  it("refuses on Enter in exactly the state the button refuses (P5-19)", () => {
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 18 } } }),
      {},
      shortcutInteraction(
        {
          count: fixedCount(1, 18, 18),
          points: [
            mayPoint(0, ["m0take", "m0dec"]),
            mayPoint(1, ["m1take", "m1dec"]),
            targetsPoint(2, ["k4", "k5"]),
          ],
          // The published allocation already sums to the count, so only the MAY leg of
          // `declarationComplete` can be unmet in leg B.
          preview: [element(18, [amt("k4", 9), amt("k5", 9)])],
        },
        "session.0.1",
        [
          ...mayCandidates("m0take", "m0dec"),
          ...mayCandidates("m1take", "m1dec"),
          seatCandidate("k4", 1),
          seatCandidate("k5", 2),
        ],
      ),
    );
    render(<DeclareShortcutModal />);

    // leg A — an out-of-window count entry. Enter in that very box must mint nothing.
    fireEvent.change(countBox(), { target: { value: "19" } });
    fireEvent.keyDown(countBox(), { key: "Enter" });
    expect(dispatchInteraction).not.toHaveBeenCalled();
    expect(dispatchMock).not.toHaveBeenCalled();

    // leg B — a legal count with one may point unanswered, fired from BOTH box families, so a
    // repair that guards only the surface the count picker owns cannot pass.
    fireEvent.change(countBox(), { target: { value: "18" } });
    fireEvent.click(screen.getByRole("button", { name: "Take optional ability 1" }));
    fireEvent.keyDown(countBox(), { key: "Enter" });
    fireEvent.keyDown(allocationRow("P2"), { key: "Enter" });
    expect(dispatchInteraction).not.toHaveBeenCalled();
    expect(dispatchMock).not.toHaveBeenCalled();

    // leg C — the instrument fires: with the declaration complete, Enter in the count box sends
    // the whole submission. Without this leg a modal that ignores Enter entirely satisfies A and
    // B vacuously.
    fireEvent.click(screen.getByRole("button", { name: "Decline optional ability 2" }));
    fireEvent.keyDown(countBox(), { key: "Enter" });
    expect(dispatchInteraction).toHaveBeenCalledWith({
      interactionId: "session.0.1",
      response: {
        type: "shortcut",
        data: {
          decision: { type: "fixed", data: { iterations: 18 } },
          pins: [
            { group: 0, choiceIds: ["m0take"], amounts: [] },
            { group: 1, choiceIds: ["m1dec"], amounts: [] },
            { group: 2, choiceIds: ["k4", "k5"], amounts: [amt("k4", 9), amt("k5", 9)] },
          ],
        },
      },
    });
    expect(dispatchMock).not.toHaveBeenCalled();
  });

  // P5-20: the visible-subject class — a control that asks about a SPECIFIC subject renders that
  // subject where a sighted player can read it. Every assertion is on rendered TEXT, never on an
  // accessible name: the accessible names carry the subject whether or not the visible text does,
  // so only a visible-text assertion discriminates on this property. All three members of the
  // class are driven — allocation rows, may panels and announce rows — which takes two offers,
  // because allocation and ranking cannot coexist: the published count spec selects exactly one
  // `targetsControl` kind.
  it("renders every per-subject control's subject visibly (P5-20)", () => {
    // A — a fixed-count offer with three victims and two may points.
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 6 } } }),
      {},
      shortcutInteraction(
        {
          count: fixedCount(1, 6, 6),
          points: [
            mayPoint(0, ["m0take", "m0dec"]),
            mayPoint(1, ["m1take", "m1dec"]),
            targetsPoint(2, ["k4", "k5", "k6"]),
          ],
          preview: [element(6, [amt("k4", 2), amt("k5", 2), amt("k6", 2)])],
        },
        "session.0.1",
        [
          ...mayCandidates("m0take", "m0dec"),
          ...mayCandidates("m1take", "m1dec"),
          seatCandidate("k4", 1),
          seatCandidate("k5", 2),
          seatCandidate("k6", 3),
        ],
      ),
    );
    render(<DeclareShortcutModal />);

    // Reach-guard: the allocation panel is mounted, and each box carries both names — the
    // accessible one queried here and the visible one asserted below.
    expect(allocationRow("P2")).toBeInTheDocument();
    // Each victim's box states WHICH victim it is. Drop the visible subject and these three
    // spinboxes are indistinguishable on screen, so each of these three queries fails.
    for (const seat of ["P2", "P3", "P4"]) {
      expect(screen.getByText(seat), seat).toBeInTheDocument();
    }
    // Two may panels, two DIFFERENT visible headings. `getByText` throws when more than one node
    // matches, so a call that resolves is itself the proof that the two subjects are distinct.
    expect(
      screen.getByText("Optional ability 1 — repeat this choice each iteration?"),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Optional ability 2 — repeat this choice each iteration?"),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Take optional ability 1" })).toBeInTheDocument();

    // B — the announce member of the class, on the only offer shape that reaches it.
    cleanup();
    seed(
      buildLoopShortcutWaitingFor(),
      {},
      shortcutInteraction(
        {
          count: { type: "untilLethal" },
          points: [mayPoint(0, ["m0take", "m0dec"]), targetsPoint(2, ["k4", "k5", "k6"])],
        },
        "session.0.1",
        [
          ...mayCandidates("m0take", "m0dec"),
          seatCandidate("k4", 1),
          seatCandidate("k5", 2),
          seatCandidate("k6", 3),
        ],
      ),
    );
    render(<DeclareShortcutModal />);

    // Reach-guard: the announce panel is mounted, one row per candidate.
    expect(screen.getAllByRole("button", { name: ANNOUNCE })).toHaveLength(3);
    for (const seat of ["P2", "P3", "P4"]) {
      expect(screen.getByText(seat), seat).toBeInTheDocument();
    }
    expect(
      screen.getByText("Optional ability 1 — repeat this choice each iteration?"),
    ).toBeInTheDocument();
  });
  // P5-21: no control in `controlNames`' population is nameless, and no two share a name. P5-20
  // closes the per-subject class on the screen; this closes it for a screen reader, which
  // navigates BY the accessible name — controls sharing one subject-free label are
  // indistinguishable there however clearly the rows read on screen. An invariant over the
  // population `controlNames` computes, not a list of today's controls: a control added later
  // that reaches for a shared subject-free label reds this row without anyone remembering to
  // extend a list. Both offers are driven because the published count spec selects exactly one
  // `targetsControl` kind, and neither branch may hand out a duplicate.
  it("gives every focusable control in the a11y tree a distinct accessible name (P5-21)", () => {
    // A — the allocation branch: a count picker and three victim rows, each an amount control
    // with two steppers, plus two may panels.
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 6 } } }),
      {},
      shortcutInteraction(
        {
          count: fixedCount(1, 6, 6),
          points: [
            mayPoint(0, ["m0take", "m0dec"]),
            mayPoint(1, ["m1take", "m1dec"]),
            targetsPoint(2, ["k4", "k5", "k6"]),
          ],
          preview: [element(6, [amt("k4", 2), amt("k5", 2), amt("k6", 2)])],
        },
        "session.0.1",
        [
          ...mayCandidates("m0take", "m0dec"),
          ...mayCandidates("m1take", "m1dec"),
          seatCandidate("k4", 1),
          seatCandidate("k5", 2),
          seatCandidate("k6", 3),
        ],
      ),
    );
    render(<DeclareShortcutModal />);

    const allocationNames = controlNames();
    // Reach-guard: the enumeration reached all four amount controls, the ones whose steppers a
    // shared label would collapse onto each other, so the assertions below run over a populated
    // set rather than an empty one.
    expect(allocationNames).toEqual(
      expect.arrayContaining([
        "Decrease the number of iterations",
        "Decrease repetitions for P2",
        "Decrease repetitions for P3",
        "Decrease repetitions for P4",
      ]),
    );
    expect(allocationNames.filter((n) => n.length === 0)).toEqual([]);
    expect(new Set(allocationNames).size, allocationNames.join(" | ")).toBe(
      allocationNames.length,
    );

    // B — the announce branch, whose selection button repeats once per row.
    cleanup();
    seed(
      buildLoopShortcutWaitingFor(),
      {},
      shortcutInteraction(
        {
          count: { type: "untilLethal" },
          points: [mayPoint(0, ["m0take", "m0dec"]), targetsPoint(2, ["k4", "k5", "k6"])],
        },
        "session.0.1",
        [
          ...mayCandidates("m0take", "m0dec"),
          seatCandidate("k4", 1),
          seatCandidate("k5", 2),
          seatCandidate("k6", 3),
        ],
      ),
    );
    render(<DeclareShortcutModal />);

    const announceNames = controlNames();
    expect(announceNames).toEqual(
      expect.arrayContaining(["Announce P2", "Announce P3", "Announce P4"]),
    );
    expect(announceNames.filter((n) => n.length === 0)).toEqual([]);
    expect(new Set(announceNames).size, announceNames.join(" | ")).toBe(announceNames.length);
  });

  // P5-22: the may panel's ordinal counts the panels ON SCREEN, not the published points.
  // Numbering by `group` would head the only panel "Optional ability 2" and tell the player the
  // dialog is withholding a choice it is obliged to render. Shape A leads with a targets point,
  // shape B with a read-only one and carries TWO may panels, so the numbering is shown contiguous
  // rather than merely offset. Both shapes assert the DISPATCHED group as well: renumbering the
  // wire instead of the display would satisfy every screen assertion here and corrupt the
  // submission.
  it("numbers the may panels by rendered position while pinning by group (P5-22)", () => {
    // A — a targets point leads, a may point follows.
    seed(
      buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 6 } } }),
      {},
      shortcutInteraction(
        {
          count: fixedCount(1, 6, 6),
          points: [targetsPoint(0, ["k4", "k5"]), mayPoint(1, ["m1take", "m1dec"])],
          preview: [element(6, [amt("k4", 3), amt("k5", 3)])],
        },
        "session.0.1",
        [seatCandidate("k4", 1), seatCandidate("k5", 2), ...mayCandidates("m1take", "m1dec")],
      ),
    );
    render(<DeclareShortcutModal />);

    // Reach-guard: the offer took the pin route (the allocation control beside the panel is only
    // rendered there), so the single panel below is a rendered may point, not an absent one.
    expect(allocationRow("P2")).toBeInTheDocument();
    // The whole set of headings, so an "ability 1" that renders ALONGSIDE a stray "ability 2"
    // cannot pass. `getAllByText` throws on an empty match, which is the query's own control.
    expect(screen.getAllByText(/^Optional ability \d+ —/).map((n) => n.textContent)).toEqual([
      "Optional ability 1 — repeat this choice each iteration?",
    ]);
    fireEvent.click(screen.getByRole("button", { name: "Take optional ability 1" }));
    fireEvent.click(confirmButton());
    // The published group, not the display ordinal, is what the engine is asked to pin.
    expect(submittedPins()).toEqual([
      { group: 0, choiceIds: ["k4", "k5"], amounts: [amt("k4", 3), amt("k5", 3)] },
      { group: 1, choiceIds: ["m1take"], amounts: [] },
    ]);

    // B — a read-only point leads and both may points follow.
    cleanup();
    vi.mocked(dispatchInteraction).mockClear();
    seed(
      buildLoopShortcutWaitingFor(),
      {},
      shortcutInteraction(
        {
          count: { type: "untilLethal" },
          points: [
            readOnlyPoint(0, "convokeTaps"),
            mayPoint(1, ["m1take", "m1dec"]),
            mayPoint(2, ["m2take", "m2dec"]),
          ],
        },
        "session.0.1",
        [...mayCandidates("m1take", "m1dec"), ...mayCandidates("m2take", "m2dec")],
      ),
    );
    render(<DeclareShortcutModal />);

    expect(screen.getAllByText(/^Optional ability \d+ —/).map((n) => n.textContent)).toEqual([
      "Optional ability 1 — repeat this choice each iteration?",
      "Optional ability 2 — repeat this choice each iteration?",
    ]);
    fireEvent.click(screen.getByRole("button", { name: "Take optional ability 1" }));
    fireEvent.click(screen.getByRole("button", { name: "Decline optional ability 2" }));
    fireEvent.click(confirmButton());
    // Each panel answers ITS OWN point: the ordinals shifted, the pins did not, and the read-only
    // point still receives none.
    expect(submittedPins()).toEqual([
      { group: 1, choiceIds: ["m1take"], amounts: [] },
      { group: 2, choiceIds: ["m2dec"], amounts: [] },
    ]);
  });
});

// ── The round-trip preview of an authored allocation. ────────────────────────────────────────

type StoreOverrides = Parameters<typeof setGameStoreForTest>[0];

const PREVIEW_OFFER_ID = "session.0.1" as InteractionId;

/** The request shape the seam takes, built from primitives so no row depends on a mint. */
function previewRequest(id: string): InteractionPreviewRequest {
  return {
    requestId: id as InteractionPreviewRequest["requestId"],
    interactionId: PREVIEW_OFFER_ID,
    response: { type: "shortcut", data: { decision: { type: "fixed", data: { iterations: 5 } }, pins: [] } },
  } as InteractionPreviewRequest;
}

/** An engine answer echoing the request's own id, carrying one per-victim entry. */
function answerWith(request: InteractionPreviewRequest, amount: number): InteractionPreview {
  return {
    requestId: request.requestId,
    interactionId: request.interactionId,
    status: { type: "confirmable" },
    progress: { selected: 1, minimum: 1, maximum: 1, aggregate: null, confirmable: true },
    outcome: "advanced",
    summaries: [],
    shortcutPreview: element(5, [amt("k4", 4), amt("k5", 1)], [
      { family: "life", player: 2, amount },
    ]),
  } as unknown as InteractionPreview;
}

/** The pin-route offer rows 9 and 10 author a split on: two announced seats, an even published
 *  split of the count, and one published life line. */
function seedPreviewOffer(store: StoreOverrides = {}) {
  const waitingFor = buildLoopShortcutWaitingFor({
    schema: { iteration_count: { Fixed: 5 } },
    certificate: { unbounded: [{ DamageDealt: 1 }] },
  });
  setGameStoreForTest({
    gameState: buildGameState({ objects: {}, priority_player: 0, waiting_for: waitingFor }),
    waitingFor,
    dispatch: dispatchMock,
    viewerInteraction: shortcutInteraction(
      {
        count: fixedCount(1, 5, 5),
        points: [targetsPoint(2, ["k4", "k5"])],
        preview: [
          element(5, [amt("k4", 3), amt("k5", 2)], [{ family: "life", player: 1, amount: -2 }]),
        ],
      },
      "session.0.1",
      [seatCandidate("k4", 1), seatCandidate("k5", 2)],
    ),
    engineCommitEpoch: 7,
    gameMode: "ai",
    ...store,
  } as StoreOverrides);
}

/** Author the 4/1 split. The first edit leaves the declaration INCOMPLETE (the rows sum to 6),
 *  so exactly one settled declaration reaches the effect. */
function authorFourOne() {
  fireEvent.change(allocationRow("P2"), { target: { value: "4" } });
  fireEvent.change(allocationRow("P3"), { target: { value: "1" } });
}

function authorOneFour() {
  fireEvent.change(allocationRow("P2"), { target: { value: "1" } });
  fireEvent.change(allocationRow("P3"), { target: { value: "4" } });
}

/** An adapter implementing only what the seam's guards read, plus whatever a row installs. */
const bareAdapter = () => ({ getSnapshot: vi.fn() }) as unknown as EngineAdapter;

/** Counts unhandled rejections carrying a row's own sentinel tag, restoring vitest's own
 *  listeners so the in-row positive control cannot poison the rest of the run. */
function captureUnhandled() {
  const seen: unknown[] = [];
  const prior = process.listeners("unhandledRejection");
  process.removeAllListeners("unhandledRejection");
  const onUnhandled = (reason: unknown) => seen.push(reason);
  process.on("unhandledRejection", onUnhandled);
  return {
    sentinels: (tag: string) => seen.filter((r) => r instanceof Error && r.message === tag),
    restore() {
      process.off("unhandledRejection", onUnhandled);
      for (const l of prior) process.on("unhandledRejection", l as never);
    },
  };
}

describe("DeclareShortcutModal — authored-split preview", () => {
  beforeEach(() => {
    dispatchMock.mockReset();
    dispatchMock.mockResolvedValue(undefined);
    vi.mocked(dispatchInteraction).mockReset();
    vi.mocked(dispatchInteraction).mockResolvedValue(undefined);
    useAppNotificationStore.setState({ notification: null });
  });

  afterEach(() => {
    cleanup();
  });

  // Row 9 (main): the rendered per-victim line is a read of the RETURNED element. The two
  // iterations submit the IDENTICAL declaration and differ only in what the engine answered, so
  // a client-side recomputation renders the same string twice and the row fails.
  it("renders the magnitudes the engine returned, not a recomputation", async () => {
    for (const amount of [-9, -7]) {
      cleanup();
      const previewInteraction = vi.fn((request: InteractionPreviewRequest) =>
        Promise.resolve(answerWith(request, amount)),
      );
      seedPreviewOffer({ adapter: { ...bareAdapter(), previewInteraction } as EngineAdapter });
      render(<DeclareShortcutModal />);

      authorFourOne();

      expect(await screen.findByText(`${amount} life — P3`)).toBeInTheDocument();
      expect(screen.queryByText(/custom distribution/i)).toBeNull();
    }
  });

  // Row 9(i-a): the seam's own contract, asserted where the two directions differ. Deleting
  // `dispatch.ts`'s capability check makes the identical call REJECT with a TypeError.
  it("resolves null without the capability and the answer with it", async () => {
    const request = previewRequest("seam-1");

    seedPreviewOffer({ adapter: bareAdapter() });
    await expect(previewInteractionResponse(request)).resolves.toBeNull();

    // Sibling: the FIRST guard, so the row distinguishes the capability check from its upstream
    // neighbours rather than conflating them.
    const previewInteraction = vi.fn((r: InteractionPreviewRequest) =>
      Promise.resolve(answerWith(r, -9)),
    );
    seedPreviewOffer({
      adapter: { ...bareAdapter(), previewInteraction } as EngineAdapter,
      gameMode: "spectate",
    });
    await expect(previewInteractionResponse(request)).resolves.toBeNull();
    expect(previewInteraction).not.toHaveBeenCalled();

    // MANDATORY PAIRED POSITIVE: with the capability the identical call reaches the adapter and
    // resolves the echoed answer, so the null above is the capability check's own answer rather
    // than an earlier short-circuit's.
    seedPreviewOffer({ adapter: { ...bareAdapter(), previewInteraction } as EngineAdapter });
    await expect(previewInteractionResponse(request)).resolves.toMatchObject({
      requestId: request.requestId,
    });
    expect(previewInteraction).toHaveBeenCalledOnce();
  });

  // Row 9(i-b): the render is a SWITCH between two defined states, not a missing element.
  it("switches between the returned lines and the landed custom-distribution state", async () => {
    seedPreviewOffer({ adapter: bareAdapter() });
    render(<DeclareShortcutModal />);
    authorFourOne();

    expect(await screen.findByText(/custom distribution/i)).toBeInTheDocument();
    expect(screen.queryByText(/life — P3$/)).toBeNull();

    cleanup();
    const previewInteraction = vi.fn((request: InteractionPreviewRequest) =>
      Promise.resolve(answerWith(request, -9)),
    );
    seedPreviewOffer({ adapter: { ...bareAdapter(), previewInteraction } as EngineAdapter });
    render(<DeclareShortcutModal />);
    authorFourOne();

    expect(await screen.findByText("-9 life — P3")).toBeInTheDocument();
    expect(screen.queryByText(/custom distribution/i)).toBeNull();
  });

  // Row 9(i-c): the switch's third arm. `PreviewLines` states nothing for an element carrying no
  // magnitudes, so an answer whose entries are empty is a state WITHOUT magnitudes and states the
  // landed split, exactly as the absent answer the row above covers does.
  it("renders the custom-distribution state for an answer whose element carries no entries", async () => {
    const gates: Array<() => void> = [];
    const previewInteraction = vi.fn(
      (request: InteractionPreviewRequest) =>
        new Promise<InteractionPreview>((resolve) => {
          gates.push(() =>
            resolve({
              ...answerWith(request, -9),
              shortcutPreview: element(5, [amt("k4", 4), amt("k5", 1)]),
            } as InteractionPreview),
          );
        }),
    );
    seedPreviewOffer({ adapter: { ...bareAdapter(), previewInteraction } as EngineAdapter });
    render(<DeclareShortcutModal />);

    authorFourOne();
    expect(gates).toHaveLength(1);
    // The answer LANDS here, so the state asserted below is the empty entry list's own — not the
    // pre-answer state it is spelled the same as.
    await act(async () => {
      gates[0]();
      await Promise.resolve();
    });

    expect(screen.getByText(/custom distribution/i)).toBeInTheDocument();
    expect(screen.queryByText(/life — P\d/)).toBeNull();
    expect(screen.queryByText(/Repeating/)).toBeNull();

    // MANDATORY PAIRED POSITIVE: identical mechanics, ONE entry added to the SAME element, and the
    // preview arm renders. So the gate above does land an answer, and the fallback is the entry
    // count's answer rather than a dead transport.
    cleanup();
    gates.length = 0;
    const populated = vi.fn(
      (request: InteractionPreviewRequest) =>
        new Promise<InteractionPreview>((resolve) => {
          gates.push(() => resolve(answerWith(request, -9)));
        }),
    );
    seedPreviewOffer({
      adapter: { ...bareAdapter(), previewInteraction: populated } as EngineAdapter,
    });
    render(<DeclareShortcutModal />);
    authorFourOne();
    expect(gates).toHaveLength(1);
    await act(async () => {
      gates[0]();
      await Promise.resolve();
    });

    expect(screen.getByText("-9 life — P3")).toBeInTheDocument();
    expect(screen.queryByText(/custom distribution/i)).toBeNull();
  });

  // Row 9(ii): the effect HANDLES a rejected request. The rendered state is deliberately not the
  // signal — the effect's leading clear makes it identical either way.
  it("leaves no unhandled rejection when the transport fails", async () => {
    const SENTINEL = "row-9ii-transport-failure";
    const capture = captureUnhandled();
    try {
      // POSITIVE CONTROL: the instrument observes a deliberately unhandled sentinel rejection,
      // so an empty projection below is a real negative rather than a dead listener.
      void Promise.reject(new Error(SENTINEL));
      // NOISE CONTROL: an unrelated unhandled rejection must not move the sentinel projection.
      void Promise.reject(new Error("row-9ii-unrelated-noise"));
      await new Promise((resolve) => setTimeout(resolve, 0));
      const control = capture.sentinels(SENTINEL).length;
      expect(control).toBe(1);

      const previewInteraction = vi.fn(() => Promise.reject(new Error(SENTINEL)));
      seedPreviewOffer({ adapter: { ...bareAdapter(), previewInteraction } as EngineAdapter });
      render(<DeclareShortcutModal />);
      authorFourOne();
      await screen.findByText(/custom distribution/i);
      await new Promise((resolve) => setTimeout(resolve, 0));

      expect(previewInteraction).toHaveBeenCalledOnce();
      expect(capture.sentinels(SENTINEL).length - control).toBe(0);
    } finally {
      capture.restore();
    }
  });

  // Row 9(iii): the effect key is GATED on an authored split. Removing the `custom` conjunct
  // issues a request for the published allocation and the first leg fails.
  it("issues no request for the offer's own published allocation", async () => {
    const previewInteraction = vi.fn((request: InteractionPreviewRequest) =>
      Promise.resolve(answerWith(request, -9)),
    );
    seedPreviewOffer({ adapter: { ...bareAdapter(), previewInteraction } as EngineAdapter });
    render(<DeclareShortcutModal />);

    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(previewInteraction).not.toHaveBeenCalled();

    // PAIRED POSITIVE: the same spy IS called once the split is authored, so "not called" is a
    // gate rather than a dead fixture.
    authorFourOne();
    expect(await screen.findByText("-9 life — P3")).toBeInTheDocument();
    expect(previewInteraction).toHaveBeenCalledOnce();
  });

  // Row 10a: correlation on the ANSWER's echoed id. Two in flight, the EARLIER answering LAST.
  it("renders the later answer when an earlier one arrives after it", async () => {
    const gates: Array<() => void> = [];
    const previewInteraction = vi.fn(
      (request: InteractionPreviewRequest) =>
        new Promise<InteractionPreview>((resolve) => {
          const first =
            request.response.type === "shortcut"
              ? request.response.data.pins[0]?.amounts?.[0]?.amount
              : undefined;
          const amount = first === 4 ? -4 : -1;
          gates.push(() => resolve(answerWith(request, amount)));
        }),
    );
    seedPreviewOffer({ adapter: { ...bareAdapter(), previewInteraction } as EngineAdapter });
    render(<DeclareShortcutModal />);

    authorFourOne();
    authorOneFour();
    expect(gates).toHaveLength(2);

    // The LATER request answers first, then the earlier one.
    await act(async () => {
      gates[1]();
      await Promise.resolve();
      gates[0]();
      await Promise.resolve();
    });

    // PAIRED POSITIVE: the two answers carry DIFFERENT non-empty entries, so "renders the later
    // one" is distinguishable from "renders nothing".
    expect(await screen.findByText("-1 life — P3")).toBeInTheDocument();
    expect(screen.queryByText("-4 life — P3")).toBeNull();
  });

  // Row 10b: the board-identity latch. `requestId` cannot catch this — the engine answered the
  // right request correctly, for a board that no longer exists.
  it("discards an answer for a board a snapshot commit moved past", async () => {
    const gates: Array<() => void> = [];
    const previewInteraction = vi.fn(
      (request: InteractionPreviewRequest) =>
        new Promise<InteractionPreview>((resolve) => {
          gates.push(() => resolve(answerWith(request, -9)));
        }),
    );
    seedPreviewOffer({ adapter: { ...bareAdapter(), previewInteraction } as EngineAdapter });
    render(<DeclareShortcutModal />);

    authorFourOne();
    expect(gates).toHaveLength(1);
    act(() => {
      useGameStore.setState({ engineCommitEpoch: 8 });
    });
    await act(async () => {
      gates[0]();
      await Promise.resolve();
    });

    expect(await screen.findByText(/custom distribution/i)).toBeInTheDocument();
    expect(screen.queryByText("-9 life — P3")).toBeNull();

    // MANDATORY PAIRED POSITIVE: the same answer with the epoch UNCHANGED does render, so the
    // discard is a branch rather than a render that never happens.
    cleanup();
    gates.length = 0;
    seedPreviewOffer({ adapter: { ...bareAdapter(), previewInteraction } as EngineAdapter });
    render(<DeclareShortcutModal />);
    authorFourOne();
    expect(gates).toHaveLength(1);
    await act(async () => {
      gates[0]();
      await Promise.resolve();
    });
    expect(await screen.findByText("-9 life — P3")).toBeInTheDocument();
  });

  // Row 11: an adapter that cannot submit produces a user-visible error. With the guard above
  // the `try` the only `catch` never sees the throw and the notification stays null.
  it("reports an adapter that cannot submit an interaction", async () => {
    const actual = await vi.importActual<typeof import("../../../game/dispatch.ts")>(
      "../../../game/dispatch.ts",
    );
    vi.mocked(dispatchInteraction).mockImplementation(actual.dispatchInteraction);
    // Reach-guard: the store starts with no notification, so a non-null read below is this
    // row's own effect.
    expect(useAppNotificationStore.getState().notification).toBeNull();

    seedPreviewOffer({ adapter: bareAdapter() });
    render(<DeclareShortcutModal />);
    fireEvent.click(confirmButton());

    await waitFor(() =>
      expect(useAppNotificationStore.getState().notification).not.toBeNull(),
    );
    // Reach-guard: the delegating mock really was entered, so the assertion is about a path the
    // click took rather than one it never reached.
    expect(vi.mocked(dispatchInteraction)).toHaveBeenCalledOnce();
    expect(useAppNotificationStore.getState().notification?.description).toBe(
      "This game connection does not support interaction responses",
    );
  });
});
