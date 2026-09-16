import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import type {
  DraftCardInstance,
  SeatPublicView,
  SharedStackPileView,
  SharedStackRefusal,
  SharedStackView,
} from "../../../adapter/draft-adapter";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import {
  DRAFT_WORKSPACE_PILE_SCALE_DEFAULT,
  type ResponsiveDraftLayout,
} from "../workspace/workspacePreferences";
import { WinstonPileTable } from "../WinstonPileTable";

// The image ladder is not what this surface is about, and resolving it would
// reach the Scryfall service. Same stub the pack-display tests use.
// Partial: `importOriginal` keeps the module's other runtime exports
// (`BoundedCache`, `useLocaleArt`) rather than replacing the module with two
// functions. Nothing on this surface's current path reaches them, so this is
// not a bug fix -- it is the mock contract the rest of the suite follows, and
// the reason it is followed is that a bare factory turns "someone imported
// another export" into an undefined-is-not-a-function at a distance.
vi.mock("../../../hooks/useCardImage", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../hooks/useCardImage")>()),
  useCardImage: () => ({ src: null, isLoading: false }),
  // The face-down stacks resolve the shared public card back through the same
  // hook. Stubbed to "no art yet" so the backs render their vector fallback
  // rather than reaching the image service.
  useCardBackImage: () => ({ src: null, advanceFailedSource: undefined }),
}));

// Records what this surface asks the preview for. Renders nothing: the claim
// under test is which preview behaviour a pile turn requires, not how the
// preview draws it — that is pinned in the preview's own suite.
interface RecordedPreview {
  card?: { name: string } | null;
  mode?: string;
  mobileLayout?: string;
  onDismiss?: () => void;
}
const previewProps: RecordedPreview[] = vi.hoisted(() => []);
vi.mock("../../card/HoverCardPreview", () => ({
  HoverCardPreview: (props: RecordedPreview) => {
    previewProps.push(props);
    return null;
  },
}));

function card(id: string, name: string): DraftCardInstance {
  return {
    instance_id: id,
    name,
    set_code: "tst",
    collector_number: "1",
    rarity: "common",
    colors: ["U"],
    cmc: 2,
    type_line: "Instant",
  };
}

const seats: SeatPublicView[] = [
  {
    seat_index: 0,
    display_name: "Alice",
    is_bot: false,
    connected: true,
    has_submitted_deck: false,
    pick_status: "Pending",
    active_pack_count: 0,
    drafted_card_count: 0,
    face_up_draft_cards: [],
  },
  {
    seat_index: 1,
    display_name: "Bob",
    is_bot: false,
    connected: true,
    has_submitted_deck: false,
    pick_status: "Waiting",
    active_pack_count: 0,
    drafted_card_count: 0,
    face_up_draft_cards: [],
  },
];

/** A pile exactly as the engine publishes one: counts, a revealed PREFIX, and a
 *  verdict per decision. `null` refusal means legal. */
function pile(
  index: number,
  total: number,
  revealed: DraftCardInstance[],
  take: SharedStackRefusal | null,
  decline: SharedStackRefusal | null,
): SharedStackPileView {
  return {
    index,
    total,
    revealed,
    legality: [
      { decision: "Take", refusal: take },
      { decision: "Decline", refusal: decline },
    ],
  };
}

/** The live turn as the engine publishes it. Every field here is PUBLIC —
 *  including `active_pile`, which every viewer receives — so the same object
 *  serves the active seat and an onlooker; what distinguishes them is the
 *  `viewerSeat` the table is rendered with, plus the `revealed` prefixes, which
 *  are the one viewer-scoped thing. `active_seat` is 0 (Alice). */
function activeTurn(piles: SharedStackPileView[], activePile: number): SharedStackView {
  return {
    main_stack_remaining: 17,
    total_cards: 23,
    active_seat: 0,
    active_pile: activePile,
    piles,
    decisions: 4,
    // Empty: this fixture exercises a display/transport path, and no client
    // consumer reads the history yet. Its fidelity to the reducer is pinned
    // in `draft-core` (`history_records_sizes_and_decisions_and_never_cards`).
    history: [],
    forced_draw: null,
  };
}

function renderTable(
  sharedStack: SharedStackView,
  overrides: {
    onDecide?: (pile: number, decision: string) => void;
    interactionLocked?: boolean;
    playFirstChooser?: number | null;
    /** Defaults to seat 0, which `activeTurn` makes the ACTIVE seat. */
    viewerSeat?: number | null;
    /** A round 1 by default, so a rendered card width is the base width and a
     *  scale assertion reads as a multiple of it. */
    pileScale?: number;
    setPileScale?: (next: number) => void;
    /** Desktop by default, where the page scrolls and this surface does not
     *  own its own height. */
    responsiveLayout?: ResponsiveDraftLayout;
  } = {},
) {
  const onDecide = overrides.onDecide ?? vi.fn();
  const setPileScale = overrides.setPileScale ?? vi.fn();
  const rendered = render(
    <WinstonPileTable
      sharedStack={sharedStack}
      seats={seats}
      viewerSeat={overrides.viewerSeat === undefined ? 0 : overrides.viewerSeat}
      playFirstChooser={overrides.playFirstChooser ?? null}
      interactionLocked={overrides.interactionLocked ?? false}
      onDecide={onDecide}
      pileScale={overrides.pileScale ?? 1}
      setPileScale={setPileScale}
      responsiveLayout={overrides.responsiveLayout ?? "desktop"}
    />,
  );
  return { ...rendered, onDecide, setPileScale };
}

function decisionButton(pileIndex: number, decision: "Take" | "Decline"): HTMLButtonElement {
  const host = document.querySelector(`[data-winston-pile="${pileIndex}"]`);
  expect(host).not.toBeNull();
  const button = host!.querySelector<HTMLButtonElement>(`[data-winston-decision="${decision}"]`);
  expect(button).not.toBeNull();
  return button!;
}

describe("WinstonPileTable", () => {
  afterEach(cleanup);

  beforeEach(() => {
    previewProps.length = 0;
    usePreferencesStore.setState({ draftCardPreviewMode: "none" });
  });

  it("enables Take exactly when the engine publishes no refusal for it", () => {
    // Paired positive: the same fixture shape, differing ONLY in the published
    // verdict, so neither arm can pass by accident. Nothing about the counts
    // changes between them — a client that derived legality from `total` would
    // answer identically in both.
    const legal = renderTable(activeTurn([pile(0, 3, [card("c1", "Ponder")], null, null)], 0));
    expect(decisionButton(0, "Take")).toBeEnabled();
    cleanup();

    renderTable(activeTurn([pile(0, 3, [card("c1", "Ponder")], "PileEmpty", null)], 0));
    expect(decisionButton(0, "Take")).toBeDisabled();
    // The engine's OWN reason is rendered, not one reinvented here.
    expect(screen.getByText("This pile is empty, so there is nothing to take.")).toBeInTheDocument();
    expect(decisionButton(0, "Take")).toHaveAttribute(
      "title",
      "This pile is empty, so there is nothing to take.",
    );
    expect(legal).toBeTruthy();
  });

  it("disables Decline on a published refusal and says why taking is mandatory", () => {
    renderTable(activeTurn([pile(0, 2, [card("c1", "Ponder")], null, "NoGuaranteedCard")], 0));

    expect(decisionButton(0, "Take")).toBeEnabled();
    expect(decisionButton(0, "Decline")).toBeDisabled();
    expect(
      screen.getByText(
        "Declining can no longer leave you a card this turn, so taking this pile is your only legal move.",
      ),
    ).toBeInTheDocument();
  });

  it("refuses to enable a decision the engine published no verdict for", () => {
    // Hostile fixture: a legality vector that does not mention `Take` at all.
    // "No verdict" must not read as permission.
    const unverdicted: SharedStackPileView = {
      index: 0,
      total: 4,
      revealed: [],
      legality: [{ decision: "Decline", refusal: null }],
    };
    renderTable(activeTurn([unverdicted], 0));

    expect(decisionButton(0, "Take")).toBeDisabled();
    expect(screen.getByText("The draft published no verdict for this decision.")).toBeInTheDocument();
    expect(decisionButton(0, "Decline")).toBeEnabled();
  });

  it("dispatches the engine's own pile index, and only for the pile being decided", () => {
    const onDecide = vi.fn();
    renderTable(
      activeTurn(
        [
          pile(0, 1, [], null, null),
          pile(1, 5, [card("c2", "Opt")], null, null),
          pile(2, 2, [], "PileNotActive", "PileNotActive"),
        ],
        1,
      ),
      { onDecide },
    );

    // Only the cursor pile carries controls.
    expect(document.querySelectorAll("[data-winston-decision]")).toHaveLength(2);
    fireEvent.click(decisionButton(1, "Take"));
    expect(onDecide).toHaveBeenCalledWith(1, "Take");

    fireEvent.click(decisionButton(1, "Decline"));
    expect(onDecide).toHaveBeenCalledWith(1, "Decline");
  });

  it("shows nothing face up and offers no control to a seat whose turn it is not", () => {
    // Exactly what `filter_for_player` publishes to the NON-ACTIVE seat, and the
    // discriminating detail is what it does NOT withhold: every `revealed` is
    // empty, but the counts AND `active_pile` are published in full, identical
    // to the active seat's own projection. The onlooker is seat 1; `active_seat`
    // is 0. A `null` cursor here would be fiction — the engine publishes it.
    const spectatingSeat: SharedStackView = {
      main_stack_remaining: 17,
      total_cards: 23,
      active_seat: 0,
      active_pile: 1,
      piles: [pile(0, 3, [], null, null), pile(1, 1, [], null, null), pile(2, 4, [], null, null)],
      decisions: 4,
      // Empty: this fixture exercises a display/transport path, and no client
      // consumer reads the history yet. Its fidelity to the reducer is pinned
      // in `draft-core` (`history_records_sizes_and_decisions_and_never_cards`).
      history: [],
      forced_draw: null,
    };
    renderTable(spectatingSeat, { viewerSeat: 1 });

    // Reach guard: the table rendered, and rendered the public counts.
    expect(screen.getByText("17 cards face down in the main stack")).toBeInTheDocument();
    expect(screen.getByText("23 cards left in the draft")).toBeInTheDocument();
    // Whose turn it is comes from `active_seat` compared against `viewerSeat`,
    // NOT from `active_pile` — which is non-null here precisely so that a
    // regression to `active_pile !== null` reds this line.
    expect(screen.getByText("Alice is deciding")).toBeInTheDocument();
    expect(screen.getByText("The piles stay face down until it is your turn.")).toBeInTheDocument();

    // The cursor IS rendered to the onlooker: which pile is being handled is
    // open information at a physical table. This is the positive half — without
    // it, the "no controls" assertion below could pass on a table that simply
    // failed to identify the cursor at all.
    expect(document.querySelector("[data-winston-pile-active='true']"))
      .toBe(document.querySelector('[data-winston-pile="1"]'));

    // And the secret half: nothing face up, and no control — even though every
    // published verdict on this view says the decision is legal FOR THE ACTIVE
    // SEAT, and even though this viewer can see which pile that seat is on.
    expect(document.querySelectorAll("[data-winston-revealed-card]")).toHaveLength(0);
    expect(document.querySelectorAll("[data-winston-decision]")).toHaveLength(0);
  });

  /**
   * The paired positive for the test above, differing ONLY in `viewerSeat`: the
   * SAME published projection, rendered for the active seat, does offer the
   * controls. That is what proves the suppression above is a seat comparison
   * and not a table that never renders controls at all.
   */
  it("offers the cursor pile's controls to the seat whose turn it is", () => {
    const sameProjection: SharedStackView = {
      main_stack_remaining: 17,
      total_cards: 23,
      active_seat: 0,
      active_pile: 1,
      piles: [pile(0, 3, [], null, null), pile(1, 1, [], null, null), pile(2, 4, [], null, null)],
      decisions: 4,
      // Empty: this fixture exercises a display/transport path, and no client
      // consumer reads the history yet. Its fidelity to the reducer is pinned
      // in `draft-core` (`history_records_sizes_and_decisions_and_never_cards`).
      history: [],
      forced_draw: null,
    };
    renderTable(sameProjection, { viewerSeat: 0 });

    expect(screen.getByText("Your turn — pile 2")).toBeInTheDocument();
    expect(document.querySelectorAll("[data-winston-decision]")).toHaveLength(2);
    expect(decisionButton(1, "Take")).toBeEnabled();
  });

  /**
   * A viewer with no assigned seat is NOT the active seat. `null` must fall to
   * the no-controls side rather than comparing equal to anything.
   */
  it("offers no control to a viewer with no assigned seat", () => {
    renderTable(activeTurn([pile(0, 3, [card("c1", "Ponder")], null, null)], 0), {
      viewerSeat: null,
    });

    expect(document.querySelectorAll("[data-winston-decision]")).toHaveLength(0);
    expect(screen.getByText("Alice is deciding")).toBeInTheDocument();
  });

  it("renders the revealed prefix verbatim and the rest as a count", () => {
    renderTable(
      activeTurn([pile(0, 4, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    expect(document.querySelectorAll("[data-winston-revealed-card]")).toHaveLength(2);
    expect(screen.getByText("Ponder")).toBeInTheDocument();
    expect(screen.getByText("Opt")).toBeInTheDocument();
    // The pile is taller than the prefix, and the remainder is a HEIGHT only.
    expect(screen.getByText("4 cards")).toBeInTheDocument();
    expect(screen.getByText("Your turn — pile 1")).toBeInTheDocument();
  });

  it("draws the unlooked-at remainder as card backs, not the pile's whole height", () => {
    // 4 cards, 2 of them already looked at. The stack stands for the OTHER two.
    // A stack drawn from `total` would say 4 and claim the seat has not seen
    // cards it is looking at right now; one drawn from a constant would say the
    // same thing for every pile on the table.
    renderTable(
      activeTurn([pile(0, 4, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const stack = document.querySelector("[data-winston-pile-facedown]");
    expect(stack).not.toBeNull();
    expect(stack).toHaveAttribute("data-winston-pile-facedown-count", "2");
    // Contents stay unpublished: the backs carry no card identity at all.
    expect(stack!.textContent).toBe("");
    expect(stack!.querySelectorAll("[data-winston-revealed-card]")).toHaveLength(0);
  });

  it("keeps a declined pile's prefix, and never the card the decline buried", () => {
    // MID-TURN, and the shape the engine really publishes: the seat looked at
    // pile 1, declined it, and is now on pile 2 -- so the engine still sends
    // pile 1's prefix, because that seat did look at it.
    //
    // THE RULE, AND WHY THE PREFIX STAYS. You may not re-examine a declined pile
    // at a physical table because doing so would show you the card the decline
    // just added. The engine removes that reason structurally: a decline APPENDS
    // its drawn card and the view slices `pile[..inspected[i]]`, so the buried
    // card sits beyond the prefix and cannot be published. What is left is
    // exactly what the seat legitimately saw, and it stays theirs for the rest
    // of the turn (`inspected` is zeroed at the next turn's start).
    //
    // This surface used to blank the prefix on top of that. It was a second
    // visibility authority in the display layer, and a one-sided one:
    // `bot_ai::opponent_read` joins these prefixes against the public decline
    // history to read open colours, so blanking them took that read away from
    // the human and left it with the bot.
    renderTable(
      activeTurn(
        [
          pile(0, 3, [card("passed-1", "Ponder"), card("passed-2", "Opt")], null, null),
          pile(1, 2, [card("cursor-1", "Brainstorm")], null, null),
        ],
        1,
      ),
    );

    // The declined pile keeps what the seat saw.
    expect(screen.getByText("Ponder")).toBeInTheDocument();
    expect(screen.getByText("Opt")).toBeInTheDocument();
    expect(document.querySelectorAll("[data-winston-pile='0'] [data-winston-revealed-card]"))
      .toHaveLength(2);
    // THE LOAD-BEARING NUMBER. The pile stands 3 tall and 2 are published, so
    // exactly ONE card is face down: the one the decline buried. If this ever
    // reads 0, the seat is being shown a card it put there blind.
    expect(document.querySelector("[data-winston-pile='0'] [data-winston-pile-facedown]"))
      .toHaveAttribute("data-winston-pile-facedown-count", "1");
    // Spent, not live: readable, visibly not the decision in front of you.
    expect(document.querySelector("[data-winston-pile='0'] [data-winston-pile-spent]"))
      .not.toBeNull();

    // The paired positive on the SAME render: the pile under decision is face up
    // and is NOT marked spent, so the treatment is keyed on the cursor.
    expect(screen.getByText("Brainstorm")).toBeInTheDocument();
    expect(document.querySelectorAll("[data-winston-pile='1'] [data-winston-revealed-card]"))
      .toHaveLength(1);
    expect(document.querySelector("[data-winston-pile='1'] [data-winston-pile-spent]")).toBeNull();
  });

  it("draws nothing face down for a pile the seat is looking all the way through", () => {
    // The paired negative, and the active seat's state at the cursor on EVERY
    // turn: the engine sets `inspected` to the pile's full height there, so
    // `revealed.length === total`. A slot here would claim a card nobody has
    // seen, on the one pile the screen is about.
    renderTable(activeTurn([pile(0, 1, [card("c1", "Ponder")], null, null)], 0));

    expect(document.querySelector("[data-winston-pile-facedown]")).toBeNull();
  });

  it("still draws a slot for a pile with no cards at all", () => {
    // The other `count === 0`, and the reason the branch is not simply deleted:
    // an empty pile is a real pile and its row should read as one.
    renderTable(activeTurn([pile(0, 0, [], null, "PileEmpty")], 0));

    expect(document.querySelector("[data-winston-pile-facedown]"))
      .toHaveAttribute("data-winston-pile-facedown-count", "0");
  });

  it("keeps the decision controls reachable while a forced draw is on screen", () => {
    // The notice holds a full-size card with a fixed width and aspect ratio, so
    // it cannot shrink. Outside the scroller it was an unshrinkable block
    // competing with the only flex-1 item in a fixed-height box, and on a short
    // viewport it took the whole box — collapsing the pile list to nothing and
    // putting Take and Decline out of reach. It is on screen for the whole of
    // the seat's NEXT turn, which is exactly when those buttons are needed.
    const stack = activeTurn([pile(0, 1, [card("c1", "Ponder")], null, null)], 0);
    renderTable(
      { ...stack, forced_draw: card("drawn-1", "Dreaded Bat-Cloud") },
      { responsiveLayout: "phone-landscape" },
    );

    const list = document.querySelector("[data-winston-pile-list]");
    const notice = document.querySelector("[data-winston-forced-draw]");
    expect(notice).not.toBeNull();
    // Inside the scroller, so the whole column scrolls as one and nothing below
    // it can be pushed out of the box.
    expect(list!.contains(notice!)).toBe(true);
    // And the controls are still rendered on the same surface.
    expect(list!.contains(decisionButton(0, "Take"))).toBe(true);
  });

  it("scrolls its own rows wherever the page will not scroll for it", () => {
    // Every viewport under 1200px wide is a non-desktop band, and the page puts
    // the surface in a fixed-height `overflow-hidden` box there. Three
    // full-card rows do not fit, and a row that overflows takes the cursor
    // pile's Take/Decline buttons off-screen with no way to reach them.
    renderTable(activeTurn([pile(0, 1, [card("c1", "Ponder")], null, null)], 0), {
      responsiveLayout: "tablet-landscape",
    });

    expect(document.querySelector("[data-winston-pile-table]"))
      .toHaveAttribute("data-winston-scrolls-piles", "true");
    expect(document.querySelector("[data-winston-pile-list]")).toHaveClass("overflow-y-auto");

    cleanup();

    // Desktop is the paired negative: the page scrolls, so a second scroller
    // here would trap the rows in a short box for no reason.
    renderTable(activeTurn([pile(0, 1, [card("c1", "Ponder")], null, null)], 0), {
      responsiveLayout: "desktop",
    });

    expect(document.querySelector("[data-winston-pile-table]"))
      .toHaveAttribute("data-winston-scrolls-piles", "false");
    expect(document.querySelector("[data-winston-pile-list]")).not.toHaveClass("overflow-y-auto");
  });

  it("scales the cards by the stored pile scale rather than a fixed width", () => {
    const { unmount } = renderTable(
      activeTurn([pile(0, 1, [card("c1", "Ponder")], null, null)], 0),
      { pileScale: 1 },
    );
    const atOne = document.querySelector<HTMLElement>("[data-winston-revealed-card]")!.style.width;
    unmount();

    renderTable(activeTurn([pile(0, 1, [card("c1", "Ponder")], null, null)], 0), { pileScale: 2 });
    const atTwo = document.querySelector<HTMLElement>("[data-winston-revealed-card]")!.style.width;

    // Read as a RATIO, so the assertion survives a change to the base width the
    // pack surface shares — it is the scaling that is under test, not 146px.
    expect(Number.parseFloat(atTwo)).toBeCloseTo(Number.parseFloat(atOne) * 2);
  });

  it("offers the same scale affordances the pack surface does", () => {
    const { setPileScale } = renderTable(
      activeTurn([pile(0, 1, [card("c1", "Ponder")], null, null)], 0),
      { pileScale: 1 },
    );

    fireEvent.click(screen.getByRole("button", { name: "Increase pile scale" }));
    expect(setPileScale).toHaveBeenLastCalledWith(1.1);
    fireEvent.click(screen.getByRole("button", { name: "Decrease pile scale" }));
    expect(setPileScale).toHaveBeenLastCalledWith(0.9);
    fireEvent.click(screen.getByRole("button", { name: "Reset pile scale" }));
    // The engine-free default, read from the preferences module rather than
    // retyped, so a retuned default moves this row with it.
    expect(setPileScale).toHaveBeenLastCalledWith(DRAFT_WORKSPACE_PILE_SCALE_DEFAULT);

    fireEvent.change(screen.getByRole("slider", { name: "Pile scale" }), { target: { value: "1.8" } });
    expect(setPileScale).toHaveBeenLastCalledWith(1.8);
  });

  it("names the card a forced draw took off the stack", () => {
    // The one card in the format a player receives without seeing it. The
    // engine publishes it to that seat alone; this asserts the surface actually
    // says so rather than leaving the player to hunt their pool.
    const stack = activeTurn([pile(0, 1, [], null, null)], 0);
    renderTable({ ...stack, forced_draw: card("drawn-1", "Dreaded Bat-Cloud") });

    expect(document.querySelector("[data-winston-forced-draw='drawn-1']")).not.toBeNull();
    expect(
      screen.getByText(
        "You declined every pile, so you drew Dreaded Bat-Cloud off the top of the main stack.",
      ),
    ).toBeInTheDocument();
  });

  it("says nothing about a forced draw the engine did not publish", () => {
    // The paired negative, and the privacy leg: `forced_draw` is null for every
    // viewer but the seat that drew, so a surface that rendered a notice from
    // anything else — the pool, the history, a local flag — would show one here.
    renderTable(activeTurn([pile(0, 1, [card("c1", "Ponder")], null, null)], 0));

    expect(document.querySelector("[data-winston-forced-draw]")).toBeNull();
  });

  it("locks both controls while a decision is in flight, claiming no refusal", () => {
    renderTable(activeTurn([pile(0, 3, [card("c1", "Ponder")], null, null)], 0), {
      interactionLocked: true,
    });

    expect(decisionButton(0, "Take")).toBeDisabled();
    expect(decisionButton(0, "Decline")).toBeDisabled();
    // A lock is not a legality statement, so no engine reason is attached.
    expect(decisionButton(0, "Take")).not.toHaveAttribute("title");
  });

  it("states the play-first choice as an instruction, never as a control", () => {
    renderTable(activeTurn([pile(0, 3, [], null, null)], 0), { playFirstChooser: 1 });

    const advisory = screen.getByText("Bob chooses who plays first in the games after the draft.");
    expect(advisory).toBeInTheDocument();
    expect(advisory.tagName).toBe("P");
    expect(advisory.querySelector("button")).toBeNull();
  });

  it("omits the play-first line when the engine published no chooser", () => {
    renderTable(activeTurn([pile(0, 3, [], null, null)], 0), { playFirstChooser: null });

    expect(document.querySelector("[data-winston-play-first]")).toBeNull();
  });

  it("asks for a readable preview even when draft previews are switched off", () => {
    // `draftCardPreviewMode` ships as "none", which is a fair default for a
    // pack: those cards render large enough to read where they sit. A pile's
    // are 88px thumbnails and the turn is a decision about what they ARE, so
    // "none" here is not a preference — it is an unreadable screen.
    renderTable(activeTurn([pile(0, 3, [card("c1", "Ponder")], null, null)], 0));

    expect(previewProps[previewProps.length - 1]?.mode).toBe("side");
  });

  it("passes a mode the player actually chose through untouched", () => {
    // The paired positive, and the reason the row above is not "this surface
    // ignores the preference": only the off state is substituted for.
    usePreferencesStore.setState({ draftCardPreviewMode: "follow" });

    renderTable(activeTurn([pile(0, 3, [card("c1", "Ponder")], null, null)], 0));

    expect(previewProps[previewProps.length - 1]?.mode).toBe("follow");
  });

  it("owns the state its overlay dismisses, and never blocks the turn behind it", () => {
    // Without both of these a narrow viewport (under the preview's 1024px
    // breakpoint) gets the blocking full-screen modal whose default dismiss
    // clears the in-game inspector — unrelated state — leaving an overlay over
    // a live turn that no tap can close.
    renderTable(activeTurn([pile(0, 3, [card("c1", "Ponder")], null, null)], 0));

    const props = previewProps[previewProps.length - 1];
    expect(props?.mobileLayout).toBe("compact");
    expect(typeof props?.onDismiss).toBe("function");
  });

  it("reaches the card a keyboard player is deciding on", () => {
    // The decision controls are real buttons, so a keyboard player can Take a
    // pile. Focusing its cards is how they can first read one.
    renderTable(activeTurn([pile(0, 3, [card("c1", "Ponder")], null, null)], 0));

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card]");
    expect(revealed).not.toBeNull();
    expect(revealed!.tabIndex).toBe(0);

    fireEvent.focus(revealed!);
    expect(previewProps[previewProps.length - 1]?.card).toMatchObject({ name: "Ponder" });

    fireEvent.blur(revealed!);
    expect(previewProps[previewProps.length - 1]?.card).toBeNull();
  });

  it("marks its cards for the preview's own stale-hover sweep", () => {
    // The marker is not decoration: `HoverCardPreview`'s cleanup effect clears
    // a preview on the next pointer move unless a `[data-deck-card-hover]`
    // element is still hovered. Without it, passing `onDismiss` above would
    // close the preview the moment the pointer twitched over the card. The
    // attribute comes from `mouseHoverPreview`, which also carries the
    // pointerleave rule that stops a narrow-viewport overlay closing the
    // gesture that opened it — both written for exactly this surface, whose
    // cards are replaced under a stationary pointer every turn.
    renderTable(activeTurn([pile(0, 3, [card("c1", "Ponder")], null, null)], 0));

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card]");
    expect(revealed).not.toBeNull();
    expect(revealed!.hasAttribute("data-deck-card-hover")).toBe(true);
  });
});
