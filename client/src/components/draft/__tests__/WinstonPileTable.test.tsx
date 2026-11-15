import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import type {
  DraftCardInstance,
  SeatPublicView,
  SharedStackPileView,
  SharedStackRefusal,
  SharedStackView,
} from "../../../adapter/draft-adapter";
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

// `HoverCardPreview` is NOT mocked here, and that is what lets "renders no
// preview overlay of its own over the piles" fail at all: the real component
// runs, and its overlay carries `data-card-preview`. MEASURED — rendering a
// `<HoverCardPreview>` inside `RevealedCard` while `lifted` reddens that row
// and no other in this file or `DraftPodPage.winston.test.tsx`.

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
    // an empty pile is a real pile and its column should read as one.
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
    // The list is a grid of one track per pile, so a notice with no span would
    // be squeezed into the first pile's column. A marker assertion: happy-dom
    // does no layout, so this pins the declaration and not the width.
    expect((notice as HTMLElement).style.gridColumn).toBe("1 / -1");
  });

  it("scrolls its own columns wherever the page will not scroll for it", () => {
    // Every viewport under 1200px wide is a non-desktop band, and the page puts
    // the surface in a fixed-height `overflow-hidden` box there. A pile column
    // has no bounded height — a full card plus a strip per further card, and a
    // decline adds a card — and one that overflows takes the cursor pile's
    // Take/Decline buttons off-screen with no way to reach them.
    renderTable(activeTurn([pile(0, 1, [card("c1", "Ponder")], null, null)], 0), {
      responsiveLayout: "tablet-landscape",
    });

    expect(document.querySelector("[data-winston-pile-table]"))
      .toHaveAttribute("data-winston-scrolls-piles", "true");
    expect(document.querySelector("[data-winston-pile-list]")).toHaveClass("overflow-y-auto");

    cleanup();

    // Desktop is the paired negative: the page scrolls, so a second scroller
    // here would trap the columns in a short box for no reason.
    renderTable(activeTurn([pile(0, 1, [card("c1", "Ponder")], null, null)], 0), {
      responsiveLayout: "desktop",
    });

    expect(document.querySelector("[data-winston-pile-table]"))
      .toHaveAttribute("data-winston-scrolls-piles", "false");
    expect(document.querySelector("[data-winston-pile-list]")).not.toHaveClass("overflow-y-auto");
  });

  // ── Column layout ─────────────────────────────────────────────────────
  //
  // `vitest.config.ts` sets `environment: "happy-dom"`, and happy-dom performs
  // no layout: `getBoundingClientRect()` on a sized element returns 0x0, which
  // a `node -e` probe against the installed copy prints directly. So every
  // assertion below pins a DECLARATION, a class or an inline style string, and
  // none of them establishes that the columns appear side by side or that the
  // overlap looks right on a screen. Only a human looking at the running app
  // establishes that.

  const BANDS: ResponsiveDraftLayout[] = [
    "phone-portrait",
    "phone-landscape",
    "tablet-portrait",
    "tablet-landscape",
    "desktop",
  ];

  it.each(BANDS)("gives each published pile its own grid column in %s", (band) => {
    renderTable(
      activeTurn(
        [
          pile(0, 3, [], null, null),
          pile(1, 1, [card("c1", "Ponder")], null, null),
          pile(2, 4, [], null, null),
        ],
        1,
      ),
      { responsiveLayout: band },
    );

    const list = document.querySelector<HTMLElement>("[data-winston-pile-list]");
    expect(list).not.toBeNull();
    expect(list!).toHaveClass("grid");
    // The band selects the scroller and nothing else: the track list is the
    // same in all five.
    expect(list!.style.gridTemplateColumns).toBe("repeat(3, minmax(0, 1fr))");
  });

  it("takes the column count from the projection rather than assuming three", () => {
    // The discriminating arm for the row above: a hard-coded `repeat(3, ...)`
    // passes every band there and fails here. Synthetic on purpose — the only
    // live `SharedStackPiles` row is the `pile_count: 3` in
    // `types::DraftKind::procedure`'s `DraftKind::Winston` arm, and
    // `git grep -n 'pile_count: 3' -- crates/draft-core/src` returns that row,
    // two prose mentions of it, and two hits under `#[cfg(test)]` — but the
    // track count is read from `piles`, so a format published with a different
    // count must not meet a layout that assumed 3.
    renderTable(activeTurn([pile(0, 3, [], null, null), pile(1, 2, [], null, null)], 0));

    expect(document.querySelector<HTMLElement>("[data-winston-pile-list]")!.style.gridTemplateColumns)
      .toBe("repeat(2, minmax(0, 1fr))");
  });

  it("stacks a pile's cards at the pool's exposure ratio, in percent of the stack width", () => {
    // `revealed.length === total`, so nothing is face down and the first card is
    // the top of the column.
    renderTable(
      activeTurn(
        [pile(0, 3, [card("c1", "Ponder"), card("c2", "Opt"), card("c3", "Brainstorm")], null, null)],
        0,
      ),
    );

    const stack = document.querySelector<HTMLElement>("[data-winston-pile-stack]");
    expect(stack).not.toBeNull();
    const cards = Array.from(document.querySelectorAll<HTMLElement>("[data-winston-revealed-card]"));
    expect(cards).toHaveLength(3);

    // The stack is declared a card wide. `not.toBe("")` first, because two
    // missing widths compare equal to each other and the pair would then pass
    // on a component that declared neither.
    expect(stack!.style.width).not.toBe("");
    expect(stack!.style.width).toBe(cards[0]!.style.width);
    // ...and that width is a maximum at both levels, so a narrow column caps it.
    expect(stack!.style.maxWidth).toBe("100%");
    expect(cards[0]!).toHaveClass("max-w-full");
    // Positioned, and carrying no explicit layer. This is the precondition the
    // component's comment about paint order rests on; the paint order itself is
    // CSS behaviour and is not asserted anywhere, here or elsewhere.
    expect(cards[0]!).toHaveClass("relative");
    expect(cards[0]!.style.zIndex).toBe("");

    // The top card sits flush; every later one is pulled up by the same amount.
    expect(cards[0]!.style.marginTop).toBe("");
    expect(cards[1]!.style.marginTop).toBe(cards[2]!.style.marginTop);
    expect(cards[1]!.style.marginTop).toMatch(/^-[\d.]+%$/);

    // THE RATIO THE LAYOUT WAS CHOSEN FOR. Granting the CSS rule that a
    // percentage top margin resolves against the containing block's inline size
    // — which nothing here measures — the overlap, the card height and the
    // strip left showing are all in units of card WIDTH, so height minus
    // overlap is the exposed strip. The 0.16 is restated here rather than
    // imported from the component, because it is the ratio this surface was
    // asked for and not a number the code may choose.
    const cardHeightsInWidths = 680 / 488;
    const overlapInWidths = -Number.parseFloat(cards[1]!.style.marginTop) / 100;
    expect(cardHeightsInWidths - overlapInWidths).toBeCloseTo(0.16, 10);
  });

  it("runs the face-down fan down the column, not across it", () => {
    // 4 cards, 2 looked at, so 2 backs. The fan comes BEFORE the revealed cards
    // in the column and they take their stack offset from it, so the
    // declarations describe one continuous run rather than two stacks in
    // different directions. Whether it looks like one is not in reach of this
    // lane, but both halves of that premise are asserted below: the offsets,
    // and the document order they are only meaningful in.
    renderTable(
      activeTurn([pile(0, 4, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const fan = document.querySelector<HTMLElement>("[data-winston-pile-facedown]");
    expect(fan).not.toBeNull();
    const backs = Array.from(fan!.querySelectorAll<HTMLElement>(":scope > *"));
    // The positive control for every negative assertion below: the backs exist
    // and are the elements being read.
    expect(backs).toHaveLength(2);

    // Vertical: a top margin, and none of the horizontal fan's declarations.
    expect(backs[0]!.style.marginTop).toBe("");
    expect(backs[1]!.style.left).toBe("");
    expect(backs[1]!.style.zIndex).toBe("");
    // The two negatives above are satisfied by an absolutely placed back that
    // simply dropped `left` and `zIndex`, so these two are what keeps them from
    // passing vacuously — the fan has to be in flow and positioned, not merely
    // missing the horizontal declarations.
    expect(backs[1]!).toHaveClass("relative");
    expect(backs[1]!).not.toHaveClass("absolute");

    const cards = Array.from(document.querySelectorAll<HTMLElement>("[data-winston-revealed-card]"));
    expect(cards).toHaveLength(2);
    // One strip per back and one per card, at the same step throughout.
    expect(backs[1]!.style.marginTop).toBe(cards[0]!.style.marginTop);
    expect(cards[0]!.style.marginTop).toBe(cards[1]!.style.marginTop);
    // And the fan itself is the top of the column, so it takes no margin.
    expect(fan!.style.marginTop).toBe("");
    // The load-bearing half: the first REVEALED card is offset. A stack index
    // that ignored the fan would leave it flush.
    expect(cards[0]!.style.marginTop).not.toBe("");
    // The other half. Without it the row passes with the fan rendered AFTER the
    // revealed cards: the offsets come from `drawsFaceDownStack`, which that
    // move does not touch, so every other assertion here reads the same value
    // either way. What it costs on screen — a run of backs below the cards
    // instead of heading them — is a layout consequence this lane cannot
    // measure, which is why the order is pinned directly. MEASURED: moving
    // `<FaceDownStack>` in `Pile` to after the `shownRevealed.map` reddens this
    // row and only this row across both Winston files; with this assertion
    // removed the same mutation reddens nothing.
    expect(fan!.compareDocumentPosition(cards[0]!) & Node.DOCUMENT_POSITION_FOLLOWING)
      .toBeTruthy();
  });

  it("puts the first revealed card at the top of the column when nothing is face down", () => {
    // The paired negative for the row above, on the same code path: no fan, so
    // the first revealed card takes no margin. Together the two pin that the
    // offset tracks `drawsFaceDownStack` rather than being constant either way.
    renderTable(activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0));

    expect(document.querySelector("[data-winston-pile-facedown]")).toBeNull();
    const cards = Array.from(document.querySelectorAll<HTMLElement>("[data-winston-revealed-card]"));
    expect(cards).toHaveLength(2);
    expect(cards[0]!.style.marginTop).toBe("");
    expect(cards[1]!.style.marginTop).not.toBe("");
  });

  /** The rect the pool's own band row stubs, at `top: 100`: a 100x139 card
   *  whose top edge is at `clientY` 100. Called that way the fields match the
   *  pool's literal one for one, `bottom: top + 139` landing on its 239; the
   *  `top` is a parameter rather than a constant only so the release row below
   *  can place a second card one strip down at 116. The band is
   *  `100 * STACK_EXPOSED_WIDTH_RATIO` = 16px, so 115 is inside it and 117 is
   *  not — the same two probes `CardPoolBoard.test.tsx`'s
   *  `reveals_sixteen_percent_of_the_card_width_between_stacked_cards` uses.
   *  MEASURED under this file's own environment: happy-dom returns
   *  `{top: 0, width: 0, height: 0}` from `getBoundingClientRect` for a
   *  `render`ed element carrying an explicit `width`/`height` style. Without a
   *  stub the band therefore collapses to `clientY <= 0`, and `fireEvent`
   *  defaults an unset `clientY` to 0 — so every row below would pass or fail
   *  for a reason nobody chose. */
  function stubCardRect(el: HTMLElement, top: number): void {
    el.getBoundingClientRect = () => ({
      top, left: 0, right: 100, bottom: top + 139, width: 100, height: 139,
      x: 0, y: top, toJSON: () => ({}),
    });
  }

  it("lifts a covered card clear of its neighbour when the pointer is on its exposed strip", () => {
    // Stacked, a covered card shows one strip of itself, so the card under the
    // pointer has to come out from under the one covering it. MEASURED: this
    // row covers the `onPointerEnter` binding (deleting it reddens the pen and
    // band rows too), and deleting `onPointerLeave` reddens exactly this row
    // and "drops a focus lift when the pointer leaves the card" — this one
    // entering the leave off a hover lift and that one off a focus lift. The
    // band row below is what pins WHERE in the card the pointer has to be.
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;
    stubCardRect(revealed, 100);
    expect(revealed).not.toHaveClass("z-10");

    fireEvent.pointerEnter(revealed, { clientY: 115, pointerType: "mouse" });
    expect(revealed).toHaveClass("z-10");

    fireEvent.pointerLeave(revealed, { pointerType: "mouse" });
    expect(revealed).not.toHaveClass("z-10");
  });

  it("gives a touch pointer no hover lift", () => {
    // The paired negative for the row above: same fixture, same stubbed rect,
    // same in-band `clientY` of 115, so `pointerType` is the only difference
    // the hover GATE can see. A touch pointer merely entering or moving over a
    // card raises nothing — touch gets its lift from a TAP instead, which is
    // the paired POSITIVE below, "lifts a covered card when a touch player
    // taps it". `pointerId: 1` matches this repo's touch idiom (`grep -n
    // pointerId client/src/hooks/__tests__/useCardHover.test.tsx` prints five
    // lines, all on touch events) but nothing in `RevealedCard` reads it.
    // Without the stub happy-dom's 0x0 rect would reduce the band to
    // `clientY <= 0` and a `fireEvent` default `clientY` of 0 would satisfy
    // it, which is a pass nobody chose.
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;
    stubCardRect(revealed, 100);

    fireEvent.pointerEnter(revealed, { clientY: 115, pointerId: 1, pointerType: "touch" });
    fireEvent.pointerMove(revealed, { clientY: 115, pointerId: 1, pointerType: "touch" });

    expect(revealed).not.toHaveClass("z-10");
  });

  it("lifts a covered card when a touch player taps it", () => {
    // The tap toggle's positive arm. `fireEvent.click(el, { pointerType })`
    // silently DROPS `pointerType` under happy-dom — `MouseEvent` declares no
    // such field, and `@testing-library/dom` constructs the event with no
    // `Object.assign` onto it — so the click here is a BARE `fireEvent.click`,
    // and the touch signal comes entirely from the real `PointerEvent`
    // `fireEvent.pointerDown` fires first, matching this repo's own idiom
    // (`useDraftWorkspaceDrag.test.tsx::firePointerActivation`). `RevealedCard`
    // reads that pointerdown's `pointerType`, not the click's own.
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;
    fireEvent.pointerDown(revealed, { pointerType: "touch" });
    fireEvent.click(revealed);

    expect(revealed).toHaveClass("z-10");
  });

  it("drops a tapped lift when the same card is tapped again", () => {
    // A plain double-tap toggle: the second tap must release what the first
    // one lifted, which requires `onPointerDown` to snapshot the CURRENT
    // `lifted` prop on every tap rather than a value fixed once. MEASURED:
    // hard-coding that snapshot's `wasLifted` to `false` (so it never reads
    // the live prop) reddens the second assertion below and nothing else
    // across this file or `DraftPodPage.winston.test.tsx`.
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;

    fireEvent.pointerDown(revealed, { pointerType: "touch" });
    fireEvent.click(revealed);
    expect(revealed).toHaveClass("z-10");

    fireEvent.pointerDown(revealed, { pointerType: "touch" });
    fireEvent.click(revealed);
    expect(revealed).not.toHaveClass("z-10");
  });

  it("survives a browser that focuses the card on tap before the click fires", () => {
    // No lane here observes REAL browser hit-testing, or whether any given
    // mobile browser focuses a `tabIndex={0}` div on tap — REASONING, not
    // measured (see the module doc's LAYOUT paragraph and `RevealedCard`'s own
    // `onPointerDown` comment). What this row pins is that IF a browser does,
    // the toggle still lands right: `fireEvent.focus` here stands in for that
    // browser's default focus-on-tap action, landing between `pointerdown` and
    // `click` exactly as it would there. MEASURED: reading the live `lifted`
    // prop in `onClick` instead of the ref's pre-tap snapshot makes the tap
    // read the lift ITS OWN focus just wrote and toggle it back off —
    // reddening this row and nothing else across this file or
    // `DraftPodPage.winston.test.tsx`.
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;

    fireEvent.pointerDown(revealed, { pointerType: "touch" });
    fireEvent.focus(revealed);
    fireEvent.click(revealed);

    expect(revealed).toHaveClass("z-10");
  });

  it("gives a mouse click no lift", () => {
    // Two genuinely different code paths, not decoration: a click's own
    // `pointerType` is never read (that field is populated inconsistently
    // across browsers — Chromium/WebKit dispatch a compatibility `click` as a
    // `PointerEvent`, Firefox has historically dispatched a bare `MouseEvent`
    // — which is why the allowlist reads the PRECEDING `pointerdown`'s
    // `pointerType` instead). The first probe has no preceding `pointerdown`
    // at all, so the ref is `null` and the allowlist fails closed on that.
    // The second probe has a real `pointerdown`, but recorded as `"mouse"`,
    // so the allowlist fails closed on the recorded value instead.
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;

    fireEvent.click(revealed);
    expect(revealed).not.toHaveClass("z-10");

    fireEvent.pointerDown(revealed, { pointerType: "mouse" });
    fireEvent.click(revealed);
    expect(revealed).not.toHaveClass("z-10");
  });

  it("clears a completed tap's own record before a later bare click reads it", () => {
    // Pins the `tapRef.current = null` clear the `onClick` comment claims.
    // Without it, a later bare `click` (no preceding `pointerdown`) would
    // replay the FIRST tap's stale record and re-toggle the lift even though
    // something else — here, a blur — already dropped it in between. Uses
    // focus/blur rather than a second touch tap to change `lifted` in
    // between, because a second real tap would itself write a fresh
    // `tapRef` entry and so could never observe a STALE one.
    // MEASURED: deleting `tapRef.current = null;` reddens the final
    // assertion below.
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;

    fireEvent.pointerDown(revealed, { pointerType: "touch" });
    fireEvent.click(revealed);
    expect(revealed).toHaveClass("z-10");

    fireEvent.focus(revealed);
    fireEvent.blur(revealed);
    expect(revealed).not.toHaveClass("z-10");

    fireEvent.click(revealed); // bare click, no preceding pointerdown
    expect(revealed).not.toHaveClass("z-10");
  });

  it("lifts one card at a time in a pile", () => {
    // The hoist's own regression test: `lifted` used to be local `useState`
    // inside `RevealedCard`, so two cards in the same pile could each hold
    // their own lift. `Pile` now owns a single `liftedCard`, so tapping the
    // second card must release the first — required for touch to work at all,
    // since a lifted card takes `z-10` and paints over the strip of the card
    // after it. MEASURED: reverting the hoist (restoring `RevealedCard`'s own
    // `useState`) reddens the final assertion below.
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const c1 = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;
    const c2 = document.querySelector<HTMLElement>("[data-winston-revealed-card='c2']")!;

    fireEvent.pointerDown(c1, { pointerType: "touch" });
    fireEvent.click(c1);
    expect(c1).toHaveClass("z-10");

    fireEvent.pointerDown(c2, { pointerType: "touch" });
    fireEvent.click(c2);
    expect(c2).toHaveClass("z-10");
    expect(c1).not.toHaveClass("z-10");
  });

  it("leaves a hovered card's lift alone when a different card loses focus", () => {
    // The hoist's other regression, caught in review rather than by a test
    // written blind: under a single `liftedCard`, an UNGUARDED "off" call from
    // any card would clear it unconditionally, so card A losing focus could
    // wipe card B's live MOUSE hover lift out from under the cursor still
    // sitting on B. `Pile`'s reducer only clears `liftedCard` when the card
    // calling `onLiftChange(false)` IS the one currently holding it.
    // MEASURED: dropping that identity guard (clearing unconditionally on
    // `next === false`, as the naive hoist first did) reddens the final
    // assertion below.
    renderTable(
      activeTurn(
        [pile(0, 3, [card("c1", "Ponder"), card("c2", "Opt"), card("c3", "Brainstorm")], null, null)],
        0,
      ),
    );

    const a = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;
    const b = document.querySelector<HTMLElement>("[data-winston-revealed-card='c2']")!;
    stubCardRect(b, 100);

    fireEvent.focus(a);
    expect(a).toHaveClass("z-10");

    fireEvent.pointerEnter(b, { clientY: 115, pointerType: "mouse" });
    expect(b).toHaveClass("z-10");
    expect(a).not.toHaveClass("z-10"); // sanity: the pile's one slot moved to B

    fireEvent.blur(a);
    expect(b).toHaveClass("z-10");
  });

  it("lifts a covered card for a pen pointer too", () => {
    // The paired POSITIVE for the touch row, and what makes "exclude touch"
    // rather than "require mouse" a measured choice instead of a preference:
    // rewriting the gate as `if (event.pointerType !== "mouse") return;` reds
    // this row and no other — MEASURED across this file and
    // `DraftPodPage.winston.test.tsx`. A pen genuinely hovers — `useCardHover.test.tsx`
    // runs its own hover rows over `["mouse", "pen"]` for the same reason.
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;
    stubCardRect(revealed, 100);

    fireEvent.pointerEnter(revealed, { clientY: 115, pointerType: "pen" });

    expect(revealed).toHaveClass("z-10");
  });

  it("releases a lifted card when the pointer drops below its exposed strip, and lifts the one under it instead", () => {
    // The reported defect. Replacing the band predicate with a bare
    // `onLiftChange(true)` — the behaviour before this commit, where a lift
    // was held for as long as the pointer was anywhere inside the card —
    // reddens this row. MEASURED: it also reddens "drops a focus lift once
    // the pointer moves below the card's exposed strip" below, since both
    // rows read the same predicate.
    //
    // What a real browser then does — re-route the pointer to the card
    // underneath the instant the one above stops being `z-10` — is hit-testing,
    // and happy-dom does none, so the hand-off is FIRED here rather than
    // observed. The two halves that belong to this file are what is asserted:
    // the card above releases at the boundary, and the same `clientY` is inside
    // the card below's own strip. The second card's `top` is one strip down
    // from the first's, which is what the negative percentage top margin
    // produces — REASONING, since happy-dom resolves no percentage margin (see
    // the LAYOUT note in `WinstonPileTable.tsx`).
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const above = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;
    const below = document.querySelector<HTMLElement>("[data-winston-revealed-card='c2']")!;
    stubCardRect(above, 100);
    stubCardRect(below, 116);

    fireEvent.pointerMove(above, { clientY: 115, pointerType: "mouse" });
    expect(above).toHaveClass("z-10");

    fireEvent.pointerMove(above, { clientY: 117, pointerType: "mouse" });
    expect(above).not.toHaveClass("z-10");

    fireEvent.pointerEnter(below, { clientY: 117, pointerType: "mouse" });
    expect(below).toHaveClass("z-10");
  });

  it("drops a focus lift once the pointer moves below the card's exposed strip", () => {
    // ONE `lifted` flag serves focus and hover both, so the mouse wins a
    // disagreement with the keyboard. Deliberate: the alternative — separate
    // flags OR-ed together — would leave the card under a focused one
    // unreachable by pointer for as long as the focus held, because the
    // focused card's `z-10` box covers it. That last clause is REASONING about
    // browser hit-testing; what this row measures is the flag.
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;
    stubCardRect(revealed, 100);

    fireEvent.focus(revealed);
    expect(revealed).toHaveClass("z-10");

    fireEvent.pointerMove(revealed, { clientY: 130, pointerType: "mouse" });
    expect(revealed).not.toHaveClass("z-10");
  });

  it("drops a focus lift when the pointer leaves the card", () => {
    // The other half of the ONE-flag design: `onPointerLeave` clears the lift
    // whether a pointer or a focus put it there. The sibling row above drives
    // the same flag through a move that lands below the strip; this one
    // drives it through the leave, which is a different binding and is
    // band-gated by nothing. MEASURED: giving focus its own flag and guarding
    // the leave with it (`onPointerLeave={(e) => { if (!focusedRef.current)
    // onLiftChange(false); }}`, plus an `onFocus`/`onBlur` pair that sets the
    // ref) reddens this row and no other across this file and
    // `DraftPodPage.winston.test.tsx`. No rect is stubbed and none is wanted:
    // neither a focus nor a leave reads one.
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;

    fireEvent.focus(revealed);
    expect(revealed).toHaveClass("z-10");

    fireEvent.pointerLeave(revealed, { pointerType: "mouse" });
    expect(revealed).not.toHaveClass("z-10");
  });

  it("centres each pile's stack in its column and keeps the column's padding thin", () => {
    // Marker assertions, and named as ones: happy-dom resolves no Tailwind, so
    // these pin the DECLARATIONS and not a measured box. Both are sizing
    // judgements the column layout forced and neither was observed before — the
    // stack is a fixed `cardWidth` inside a `minmax(0, 1fr)` track, so without
    // `mx-auto` it sits against the track's leading edge; and the row layout's
    // `p-3` came out of a full row's slack, where a one-card-wide column has
    // none to give.
    renderTable(activeTurn([pile(0, 1, [card("c1", "Ponder")], null, null)], 0));

    const pileEl = document.querySelector("[data-winston-pile='0']")!;
    expect(pileEl).toHaveClass("p-2");
    expect(pileEl).not.toHaveClass("p-3");
    expect(pileEl.querySelector("[data-winston-pile-stack]")).toHaveClass("mx-auto");
  });

  it("lifts a covered card for a keyboard player too", () => {
    // `RevealedCard` is already focusable (`tabIndex={0}`, pinned by "reaches
    // the card a keyboard player is deciding on"). What focus did not do was
    // raise the card itself out from under its neighbour, which stacking is
    // what made necessary. No rect is stubbed and none is wanted: a focus has
    // no cursor, so the lift is not band-gated.
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card]")!;
    fireEvent.focus(revealed);
    expect(revealed).toHaveClass("z-10");

    fireEvent.blur(revealed);
    expect(revealed).not.toHaveClass("z-10");
  });

  it("puts the decision controls under the pile they decide, not in its header", () => {
    // The pair moved off the header line, which is a sizing judgement about a
    // one-card-wide column that nothing here measures. What this pins is the
    // part that would break the suite: they stay inside this pile's own
    // element. `decisionButton` above is this file's only PILE-SCOPED
    // `data-winston-decision` query — the qualifier is load-bearing, since the
    // file scopes plenty of OTHER selectors to a pile. The four remaining
    // `data-winston-decision` queries here count the buttons document-wide,
    // and that count is the same wherever they sit
    // (`grep -rn data-winston-decision client/src`).
    // MEASURED: rendering the actions block as a sibling of
    // `[data-winston-pile]` instead of a child reddens exactly the rows that
    // call the helper and no others, this one among them.
    renderTable(activeTurn([pile(0, 1, [card("c1", "Ponder")], null, null)], 0));

    const pileEl = document.querySelector("[data-winston-pile='0']")!;
    const actions = pileEl.querySelector("[data-winston-pile-actions]");
    expect(actions).not.toBeNull();
    expect(actions!.contains(decisionButton(0, "Take"))).toBe(true);
    expect(actions!.contains(decisionButton(0, "Decline"))).toBe(true);

    const stack = pileEl.querySelector("[data-winston-pile-stack]")!;
    expect(stack.compareDocumentPosition(actions!) & Node.DOCUMENT_POSITION_FOLLOWING)
      .toBeTruthy();

    // A marker assertion, and named as one: happy-dom resolves no Tailwind, so
    // this pins the declaration and not a measured width. The row layout gave
    // each button a `min-w-[6rem]` floor, sized for a pair sitting SIDE BY SIDE
    // across a full-width row — in
    // `git show upstream/main:client/src/components/draft/WinstonPileTable.tsx`
    // the pile list is a column flex, so each pile spans the container, and
    // the pair sits in an `ml-auto flex shrink-0 gap-2` span on the header
    // line. Stacked in a one-card-wide column the pair wants a fill instead.
    // Whether the floor would actually overflow such a column is a
    // resolved-layout question, and no lane in this repo can ask it.
    expect(decisionButton(0, "Take")).toHaveClass("w-full");
    expect(decisionButton(0, "Take")).not.toHaveClass("min-w-[6rem]");
    // And no padding override either. `menuButtonClass`'s `sm` already carries
    // `px-4`, and a `px-2` appended after it in the class attribute loses to it
    // on stylesheet source order — so one here would be dead weight that reads
    // like a narrower button. See the note at the call site for the check.
    expect(decisionButton(0, "Take")).not.toHaveClass("px-2");
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

  it("renders no preview overlay of its own over the piles", () => {
    // Defect 2, and the row is written so that it CAN fail:
    // `HoverCardPreview` is deliberately not mocked in this file, so putting
    // the element back renders the real component, whose overlay carries
    // `data-card-preview` (`CardPreview.tsx`). A dock-side overlay covering
    // the piles is the reported behaviour; what stands in for it is the lift,
    // at whatever the player set the pile scale to.
    renderTable(activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0));

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;
    stubCardRect(revealed, 100);
    fireEvent.pointerEnter(revealed, { clientY: 115, pointerType: "mouse" });
    fireEvent.focus(revealed);

    expect(document.querySelector("[data-card-preview]")).toBeNull();
  });

  it("reaches the card a keyboard player is deciding on", () => {
    // The decision controls are real buttons, so a keyboard player can Take a
    // pile. Focusing its cards is how they can first read one, and the name is
    // declared on the card itself. The `aria-label` and `role` DECLARATIONS
    // are what this pins and all it pins: whether the label then reaches
    // assistive technology through a `div` carrying `role="button"` is not
    // established anywhere in this repo. Nothing else in this file asserts
    // it.
    renderTable(activeTurn([pile(0, 3, [card("c1", "Ponder")], null, null)], 0));

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card]");
    expect(revealed).not.toBeNull();
    expect(revealed!.tabIndex).toBe(0);
    expect(revealed!.getAttribute("aria-label")).toBe("Ponder");
    expect(revealed!.getAttribute("role")).toBe("button");
  });

  it("toggles the lift when the focused card takes Enter", () => {
    // The card is a control now (`role="button"`), and iOS Safari's
    // compatibility-click gate is the reason -- see `RevealedCard`'s own
    // `role`/`onKeyDown` comment. `onFocus` above already lifts the card
    // unconditionally, so this row starts from lifted and pins that Enter
    // TOGGLES rather than only ever re-lifting. MEASURED: deleting the
    // `onKeyDown` handler reddens the second assertion below.
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;
    expect(revealed).toHaveAttribute("aria-pressed", "false");
    fireEvent.focus(revealed);
    expect(revealed).toHaveClass("z-10");
    expect(revealed).toHaveAttribute("aria-pressed", "true");

    fireEvent.keyDown(revealed, { key: "Enter" });
    expect(revealed).not.toHaveClass("z-10");
    expect(revealed).toHaveAttribute("aria-pressed", "false");

    fireEvent.keyDown(revealed, { key: "Enter" });
    expect(revealed).toHaveClass("z-10");
    expect(revealed).toHaveAttribute("aria-pressed", "true");
  });

  it("toggles the lift when the focused card takes Space", () => {
    // The paired key for the row above: ARIA authoring practice for a
    // `role="button"` element requires BOTH keys, since only a native
    // `<button>` gets either for free. MEASURED: deleting the `onKeyDown`
    // handler reddens the second assertion below.
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;
    fireEvent.focus(revealed);
    expect(revealed).toHaveClass("z-10");

    fireEvent.keyDown(revealed, { key: " " });
    expect(revealed).not.toHaveClass("z-10");

    fireEvent.keyDown(revealed, { key: " " });
    expect(revealed).toHaveClass("z-10");
  });

  it("gives an unrelated key no lift toggle", () => {
    // The allowlist's negative: only Enter and Space are meant to toggle.
    // MEASURED: removing the `event.key !== "Enter" && event.key !== " "`
    // check (so `onKeyDown` toggles on any key) reddens this row's own
    // assertion; nothing else in this file or `DraftPodPage.winston.test.tsx`
    // presses a key other than Enter or Space on a revealed card, so no other
    // row catches it.
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const revealed = document.querySelector<HTMLElement>("[data-winston-revealed-card='c1']")!;
    fireEvent.focus(revealed);
    expect(revealed).toHaveClass("z-10");

    fireEvent.keyDown(revealed, { key: "Tab" });
    expect(revealed).toHaveClass("z-10");
  });

  // ── Spread toggle ────────────────────────────────────────────────────
  //
  // The other route to a covered card, alongside the tap above: a real
  // `<button>` per pile that un-stacks its revealed faces outright, reaching
  // the same cards with no dependence on a pointer's own reported type.

  it("un-stacks a pile's revealed faces when the player spreads it", () => {
    renderTable(
      activeTurn(
        [pile(0, 3, [card("c1", "Ponder"), card("c2", "Opt"), card("c3", "Brainstorm")], null, null)],
        0,
      ),
    );

    const cards = Array.from(document.querySelectorAll<HTMLElement>("[data-winston-revealed-card]"));
    // Positive control: stacked by default, so the toggle below has something
    // to undo.
    expect(cards[1]!.style.marginTop).toMatch(/^-[\d.]+%$/);

    const spreadToggle = screen.getByRole("button", { name: "Spread out pile 1" });
    // The 44px touch target the decision buttons carry
    // (`buttonStyles.ts::menuButtonClass`'s `sm` size), asserted directly
    // rather than left to the commit message: MEASURED, flipping this
    // button's `size` from `"sm"` to `"icon"` reddens this assertion.
    expect(spreadToggle).toHaveClass("min-h-11");
    fireEvent.click(spreadToggle);

    expect(cards[1]!.style.marginTop).not.toMatch(/^-/);
    // A plain gap, uniform down the column, not a scaled-up overlap.
    expect(cards[1]!.style.marginTop).toBe(cards[2]!.style.marginTop);
    // The top card takes no margin whether spread or stacked.
    expect(cards[0]!.style.marginTop).toBe("");
  });

  it("leaves the face-down fan stacked when a pile is spread", () => {
    // Backs are a height, never contents (`FaceDownStack`'s own doc), so
    // spreading them would buy card-heights of nothing — `stackMarginTop`'s
    // `spread` argument reaches `RevealedCard` only, never `FaceDownStack`.
    renderTable(
      activeTurn([pile(0, 4, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const fan = document.querySelector<HTMLElement>("[data-winston-pile-facedown]")!;
    const backs = Array.from(fan.querySelectorAll<HTMLElement>(":scope > *"));
    // Positive control: the fan exists and still overlaps before the toggle.
    expect(backs).toHaveLength(2);
    expect(backs[1]!.style.marginTop).toMatch(/^-[\d.]+%$/);

    fireEvent.click(screen.getByRole("button", { name: "Spread out pile 1" }));

    // Unchanged: the fan still overlaps.
    expect(backs[1]!.style.marginTop).toMatch(/^-[\d.]+%$/);
    // The first revealed card, which stacks ONTO the fan, now clears it with
    // the plain gap instead of the negative overlap.
    const cards = Array.from(document.querySelectorAll<HTMLElement>("[data-winston-revealed-card]"));
    expect(cards[0]!.style.marginTop).not.toMatch(/^-/);
  });

  it("restacks a pile when the spread toggle is pressed again", () => {
    renderTable(
      activeTurn([pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null)], 0),
    );

    const spreadToggle = screen.getByRole("button", { name: "Spread out pile 1" });
    expect(spreadToggle).toHaveAttribute("aria-expanded", "false");

    fireEvent.click(spreadToggle);
    // `getByRole` with the STACKED label throws if the label ever freezes on
    // one string instead of tracking the toggle.
    const stackToggle = screen.getByRole("button", { name: "Stack up pile 1" });
    expect(stackToggle).toHaveAttribute("aria-expanded", "true");

    fireEvent.click(stackToggle);
    expect(screen.getByRole("button", { name: "Spread out pile 1" }))
      .toHaveAttribute("aria-expanded", "false");
    const cards = Array.from(document.querySelectorAll<HTMLElement>("[data-winston-revealed-card]"));
    expect(cards[1]!.style.marginTop).toMatch(/^-[\d.]+%$/);
  });

  it("spreads one pile without spreading its neighbours", () => {
    renderTable(
      activeTurn(
        [
          pile(0, 2, [card("c1", "Ponder"), card("c2", "Opt")], null, null),
          pile(1, 2, [card("c3", "Brainstorm"), card("c4", "Preordain")], null, null),
        ],
        0,
      ),
    );

    fireEvent.click(screen.getByRole("button", { name: "Spread out pile 1" }));

    const pile0Cards = document.querySelectorAll<HTMLElement>(
      "[data-winston-pile='0'] [data-winston-revealed-card]",
    );
    const pile1Cards = document.querySelectorAll<HTMLElement>(
      "[data-winston-pile='1'] [data-winston-revealed-card]",
    );
    expect(pile0Cards[1]!.style.marginTop).not.toMatch(/^-/);
    expect(pile1Cards[1]!.style.marginTop).toMatch(/^-[\d.]+%$/);
  });

  it("offers the spread toggle on every pile, including one with nothing to spread", () => {
    // No `shownRevealed.length > 1` gate: a length comparison feeding a
    // control would be a THIRD sum in a file whose module doc names exactly
    // two ("splitting a published total into its face-up and face-down halves
    // and turning two counts into a bar width are the only sums here").
    renderTable(
      activeTurn(
        [pile(0, 1, [card("c1", "Ponder")], null, null), pile(1, 3, [], null, null)],
        0,
      ),
    );

    expect(document.querySelectorAll("[data-winston-pile-spread]")).toHaveLength(2);
  });
});
