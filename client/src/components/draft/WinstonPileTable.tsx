/**
 * Winston Pile Table — the drafting-phase surface for a `SharedStackPiles` draft.
 *
 * DISPLAY LAYER, and unusually strictly so. Every fact on this screen is read from
 * `DraftPlayerView.shared_stack` exactly as the engine published it:
 *
 *   * whether a control is available comes from `pile.legality[].refusal`, the
 *     engine's single legality authority (`shared_stack::refusal_for`), never from
 *     a pile's `total` or from `main_stack_remaining`. NO arithmetic anywhere in
 *     this file feeds a control — splitting a published total into its face-up
 *     and face-down halves and turning two counts into a bar width are the only
 *     sums here, and neither reaches a button — because any such arithmetic
 *     would be a SECOND authority for a question the reducer already answers,
 *     and the two could then disagree, which is the one failure mode the
 *     published vector exists to make impossible;
 *   * whose turn it is comes from `active_seat`, compared against this viewer's
 *     own seat. `active_pile` is NOT that answer: the engine publishes the
 *     cursor to every viewer (it is open information at a physical table, and
 *     the `legality` vector discloses it regardless), so a non-null
 *     `active_pile` says nothing about whose turn it is;
 *   * WHICH pile is being decided on comes from `active_pile`, and is rendered
 *     for every viewer — an onlooker sees the highlight but gets no controls;
 *   * what may be shown face up comes from `pile.revealed`, the prefix the
 *     engine publishes to the active seat. A declined pile keeps the cards that
 *     seat inspected, while the card the decline added remains outside that
 *     prefix and therefore face down; the engine clears every prefix when the
 *     turn ends. This surface renders that published boundary directly rather
 *     than adding a second visibility authority;
 *   * what the viewer's own forced draw was comes from `forced_draw`, which the
 *     engine publishes to that seat alone and is the SOLE authority for it.
 *     This component adds only a `viewerSeat !== null` test, which excludes a
 *     seatless spectator and nothing else — it cannot tell whose draw a card is
 *     from the published shape, so it is not a second lock on the field and is
 *     not written as one.
 *
 * `play_first_chooser` is rendered as an INSTRUCTION to the players and never as a
 * control: the engine does not enforce the choice (it is advisory), so offering a
 * button would claim an authority no reducer backs.
 *
 * LAYOUT. The piles are COLUMNS — one grid column per published pile — and each
 * pile's cards STACK vertically inside its column, each card pulled up over the
 * one before it so that all but the last shows only a strip of itself. That is
 * the card pool's own primitive at the pool's own exposure ratio:
 * `STACK_EXPOSED_WIDTH_RATIO` below carries the same name and the same value as
 * the pool's, in `WorkspaceCard`. They are two separate declarations and nothing
 * imports one from the other — `git grep -n STACK_EXPOSED_WIDTH_RATIO -- client`
 * prints both, and is also the check that they still agree.
 *
 * The overlap is a NEGATIVE PERCENTAGE top margin, and the percentage rather
 * than a pixel count is deliberate. REASONING, not measurement, for the whole
 * of this paragraph: a percentage top margin resolves against the containing
 * block's INLINE size rather than its height, which is a CSS box-model rule,
 * and this repo has no lane that observes it — the client suite runs under
 * happy-dom, which returns a 0x0 `getBoundingClientRect` for a sized element.
 * Granting the rule, one constant stated in units of card WIDTH — a card height
 * less one strip — expresses the same overlap at whatever width the card is
 * finally drawn at, including a width a narrow column capped. `WorkspaceCard`
 * states that same product as the literal `-123.3442622951%` — that value
 * ROUNDED to ten decimal places, so the two strings agree to nine and diverge
 * at the tenth. This file derives it from the ratio instead
 * (`node -e 'console.log((680 / 488 - 0.16) * 100)'` prints
 * `123.34426229508198`).
 *
 * This surface used to be three full-width ROWS, on the argument that a Winston
 * turn is a decision about one pile's cards so that pile should have the whole
 * width to show them side by side. That argument was about the cards inside a
 * pile running HORIZONTALLY, in the `overflow-x-auto` scroller this commit
 * deletes (`git diff upstream/main -- <this file> | grep overflow-x-auto`), and
 * it does not survive the cards being stacked.
 *
 * The cost of stacking them is real, and is why this was a judgement call and
 * not a tidy-up: in the pile under decision every card but the last now shows
 * only its top strip. At the shipped default that strip is
 * `DRAFT_PACK_CARD_BASE_WIDTH_PX * DRAFT_WORKSPACE_PILE_SCALE_DEFAULT *
 * STACK_EXPOSED_WIDTH_RATIO` ≈ 31px, off a card drawn ≈197px WIDE (the first
 * two of those constants) and so ≈275px TALL (that width times
 * `CARD_FACE_HEIGHT / CARD_FACE_WIDTH`) — arithmetic over five in-tree
 * constants, and about a name bar's worth. Reading a covered card takes a Tab,
 * or a hover over the strip it still shows rather than anywhere on its box —
 * "releases a lifted card when the pointer drops below its exposed strip, and
 * lifts the one under it instead". Why the band and not the whole box is
 * REASONING about browser hit-testing, written out at `RevealedCard`'s
 * `updateLift`. A touch tap on that same strip does the same thing —
 * REASONING, not measured; no lane here drives a real tap, and `RevealedCard`'s
 * own `onPointerDown`/`onClick` comment says what that reasoning rests on —
 * or the pile header's own spread toggle un-stacks that pile's faces outright
 * and needs no gesture at all ("un-stacks a pile's revealed faces when the
 * player spreads it").
 *
 * What a lift shows is the card's OWN FACE at the pile scale, and nothing
 * larger: this surface renders NO enlarged preview of its own ("renders no
 * preview overlay of its own over the piles"), so how readable a lifted card is
 * is settled by the pile scale control
 * (`git grep -n DRAFT_WORKSPACE_PILE_SCALE_M -- client/src/components/draft/workspace/workspacePreferences.ts`
 * prints the range the player picks from, 0.4 to 2.9). An overlay stood in for
 * that until this commit and came out on a user report that it covered the
 * piles they were choosing between — a report about a running browser, not
 * something this repo measures. The price is REASONING for the same reason
 * everything else about the drawn box here is — what fits inside a strip is a
 * question about rendered glyphs, and the client lane runs under happy-dom,
 * which resolves no layout — and the price is that a covered card's type line,
 * P/T and rules text are behind the card covering it until it is lifted.
 *
 * What the rows cost instead was HEIGHT: three of them, each at least a full
 * card tall, on a page whose other half is the player's own pool. Which of
 * those two costs matters more is a judgement, and it was made deliberately in
 * favour of the height; no command decides it.
 *
 * The piles not being decided on are still heights, and a height still reads
 * better as a fanned stack of card backs than as a number in a box. The fan now
 * runs DOWN the column at the same `STACK_EXPOSED_WIDTH_RATIO` the revealed
 * cards expose. The scale control is still the pack surface's control on this
 * surface's own stored value.
 */

import { useRef, useState, type PointerEvent } from "react";
import { useTranslation } from "react-i18next";

import type { ResponsiveDraftLayout } from "./workspace/workspacePreferences";
import type {
  DraftCardInstance,
  SeatPublicView,
  SharedStackPileDecision,
  SharedStackPileView,
  SharedStackRefusal,
  SharedStackView,
} from "../../adapter/draft-adapter";
import { CardBackFallback } from "../card/CardBackFallback";
import { menuButtonClass } from "../menu/buttonStyles";
import {
  DRAFT_PACK_CARD_BASE_WIDTH_PX,
  DRAFT_WORKSPACE_PILE_SCALE_DEFAULT,
  DRAFT_WORKSPACE_PILE_SCALE_MAX,
  DRAFT_WORKSPACE_PILE_SCALE_MIN,
  DRAFT_WORKSPACE_PILE_SCALE_STEP,
  repairDraftWorkspacePileScale,
} from "./workspace/workspacePreferences";
import { useDraftCardFace } from "./DraftCardFace.tsx";

/**
 * The card face's own dimensions, in the one ratio every face-up card, card
 * back and empty slot on this surface is drawn at. Both forms below are derived
 * from this pair, so the string handed to `aspect-ratio` and the number the
 * overlap arithmetic uses cannot state different shapes.
 */
const CARD_FACE_WIDTH = 488;
const CARD_FACE_HEIGHT = 680;
/** Card aspect, shared by every face-up card, card back and empty slot here. */
const CARD_ASPECT = `${CARD_FACE_WIDTH} / ${CARD_FACE_HEIGHT}`;
/** The same aspect as a bare number, for the overlap arithmetic below. */
const CARD_HEIGHT_TO_WIDTH = CARD_FACE_HEIGHT / CARD_FACE_WIDTH;

/**
 * How many card backs a face-down stack draws before it stops adding them.
 *
 * A display cap and nothing else: the pile's real height is published as
 * `total` and is always rendered as a number in the pile's header. Fanning one
 * back per card would run a 20-card pile nineteen strips down its column while
 * saying nothing a reader can count at a glance.
 */
const FACE_DOWN_STACK_MAX_BACKS = 5;

/**
 * Fraction of a stacked card's WIDTH that stays visible above the card covering
 * it. The pool's own exposure ratio — see the LAYOUT note at the top of this
 * file for what does and does not keep the two in step.
 */
const STACK_EXPOSED_WIDTH_RATIO = 0.16;

/**
 * How far a stacked card is pulled up over the one before it, as a percentage
 * of the stack's own width: one card height less one exposed strip, both in
 * units of card width. See the LAYOUT note for why this is a percentage.
 */
const STACK_OVERLAP_PERCENT = (CARD_HEIGHT_TO_WIDTH - STACK_EXPOSED_WIDTH_RATIO) * 100;

/**
 * The gap between a spread pile's un-stacked cards: the same `0.5rem` this
 * column already puts between its own header, stack and actions block
 * (`git grep -n 'gap-2' -- client/src/components/draft/WinstonPileTable.tsx`),
 * so a spread column's inter-card gap is the column's own gap, not a new
 * number.
 */
const SPREAD_GAP = "0.5rem";

/**
 * The stacking margin one card deep into a stack, or none at its top.
 * `spread` swaps the negative overlap for the plain gap above, which is what
 * `Pile`'s per-pile spread toggle un-stacks. `FaceDownStack` never passes it —
 * every call there stays one argument, so the fan of card BACKS keeps
 * overlapping even in a spread pile: a back is a height, never contents (see
 * `FaceDownStack`'s own doc), and spreading it would buy card-heights of
 * nothing.
 */
function stackMarginTop(stackIndex: number, spread = false): string | undefined {
  if (stackIndex === 0) return undefined;
  return spread ? SPREAD_GAP : `-${STACK_OVERLAP_PERCENT}%`;
}

export interface WinstonPileTableProps {
  /** The engine's projection of the live turn FOR THIS VIEWER. */
  sharedStack: SharedStackView;
  /** Seat list from the same view, for naming the seat whose turn it is. */
  seats: readonly SeatPublicView[];
  /**
   * This viewer's own seat, from the transport handshake. `null` before a seat
   * is assigned, which renders as "not your turn" — the safe direction, since
   * every control is gated on a positive match.
   */
  viewerSeat: number | null;
  /** `DraftPlayerView.play_first_chooser` — advisory, rendered as a sentence. */
  playFirstChooser?: number | null;
  /** A decision is in flight, or the pod is paused. Not a legality statement: it
   *  suppresses a second dispatch and carries no refusal reason. */
  interactionLocked: boolean;
  onDecide: (pile: number, decision: SharedStackPileDecision) => void;
  /** The player's stored pile scale, and the setter that persists it. Owned by
   *  the page for the same reason `packScale` is: it outlives this surface. */
  pileScale: number;
  setPileScale: (next: number) => void;
  /**
   * The page's own layout band, passed for ONE reason: whether this surface
   * owns its height. It selects nothing about the column layout, which is the
   * same `repeat(${piles.length}, minmax(0, 1fr))` in every band.
   *
   * On desktop the drafting column is `flex-col` in a page that scrolls, so the
   * columns can be as tall as the scale makes them. Every other band puts the
   * surface inside a fixed-height `overflow-hidden` box — and "every other" is
   * literally every viewport under 1200px wide, not just phones
   * (`getResponsiveDraftLayout` returns "desktop" at `viewportWidth >= 1200`
   * and something else at every width below it). A pile column has no bounded
   * height to fit into one: it is a full card plus one strip for every further
   * card, and the pile itself gains a card every time a seat declines it
   * (`shared_stack::apply_shared_stack_decision`'s `Decline` arm pushes the top
   * of the main stack onto the declined pile). A column that overflows an
   * `overflow-hidden` parent takes its Take/Decline buttons off-screen with no
   * scrollbar to reach them — reasoning inherited from the row layout this
   * replaced, and no test here measures it. So off desktop the pile list
   * becomes this component's own scroller.
   */
  responsiveLayout: ResponsiveDraftLayout;
}

/**
 * The engine's verdict for one decision on one pile.
 *
 * Looked up BY `decision` rather than by position: the published vector is built
 * from `SharedStackPileDecision::ALL`, so a widened axis must not silently shift
 * which verdict a button reads. `undefined` — no verdict published for this
 * decision at all — is deliberately distinct from `null` (published, and legal).
 */
function verdictFor(
  pile: SharedStackPileView,
  decision: SharedStackPileDecision,
): SharedStackRefusal | null | undefined {
  const entry = pile.legality.find((candidate) => candidate.decision === decision);
  return entry === undefined ? undefined : entry.refusal;
}

// ── Revealed card ───────────────────────────────────────────────────────

function RevealedCard({
  card,
  width,
  stackIndex = 0,
  spread = false,
  lifted,
  onLiftChange,
}: {
  card: DraftCardInstance;
  width: number;
  /** Position in the pile's vertical stack. 0 sits flush; every later card is
   *  pulled up over the one before it, leaving one strip of it showing.
   *  Omitted by `ForcedDrawNotice`, which draws one card and no stack. */
  stackIndex?: number;
  /** Un-stack this card from the one before it. Owned by `Pile`; see
   *  `stackMarginTop`. Omitted by `ForcedDrawNotice`, which draws a lone card
   *  with nothing to spread. */
  spread?: boolean;
  /** Whether THIS card is the one raised clear of its neighbour. Controlled
   *  by the caller (`Pile`, or `ForcedDrawNotice`'s own local flag) rather
   *  than owned here, because a pile must keep at most one card lifted at a
   *  time — a lifted card takes `z-10` and paints over the one after it, so a
   *  second stale lift would make that card untappable. Pinned by "lifts one
   *  card at a time in a pile". */
  lifted: boolean;
  onLiftChange: (next: boolean) => void;
}) {
  const sourcePrinting = { setCode: card.set_code, collectorNumber: card.collector_number };
  const { src, isLoading, displayName } = useDraftCardFace(card.name, sourcePrinting);
  // A covered card shows one strip of itself, so hovering over that strip or
  // tabbing to the card has to raise it out of the stack. A `z-index` lift and
  // not a transform, because off desktop `[data-winston-pile-list]` takes
  // `overflow-y-auto` and a card that MOVED could be clipped by it — CSS
  // reasoning, and no test here measures it. The `ownsHeight` that switches
  // that class on is the same one that publishes `data-winston-scrolls-piles`,
  // but the attribute sits a level up, on the `[data-winston-pile-table]`
  // section: "scrolls its own columns wherever the page will not scroll for
  // it" asserts the attribute and the class on different elements.
  //
  // Records what a TAP needs to know before its own side effects run: the
  // pointer type off a genuine `PointerEvent` (`onPointerDown` always gets
  // one, unlike the compatibility `click` that follows it, whose OWN
  // `pointerType` field is inconsistently populated across browsers) and the
  // lift this card held before this tap started. A browser that focuses a
  // `tabIndex={0}` div on tap delivers that focus BETWEEN `pointerdown` and
  // `click`, so reading the live `lifted` prop at click time would read a
  // value this same tap's own focus just wrote and toggle the wrong way on
  // the first tap. Reading the pre-tap snapshot instead is what survives that
  // write — pinned by "survives a browser that focuses the card on tap before
  // the click fires", the one row here where something writes `lifted`
  // between `pointerdown` and `click`. MEASURED: reading the live `lifted`
  // prop in `onClick` instead of this snapshot reddens that row alone, across
  // this file and `DraftPodPage.winston.test.tsx`.
  const tapRef = useRef<{ pointerType: string; wasLifted: boolean } | null>(null);
  // The card pool's own band predicate, against this file's own
  // `STACK_EXPOSED_WIDTH_RATIO` — see the LAYOUT note at the top for the grep
  // that checks the two declarations still agree, and
  // `WorkspaceCard.tsx::WorkspaceCard`'s `updateHoverReveal` for the pool's
  // copy of these three lines.
  //
  // It is bound to `onPointerMove` as well as `onPointerEnter` below, and the
  // move binding is the one this commit adds. REASONING for why it is needed,
  // about browser hit-testing, which happy-dom does not perform: while a card
  // holds `z-10` its box covers the cards below it, so no `pointerenter` can
  // reach them and no `pointerleave` reaches this card until the cursor leaves
  // its WHOLE box — a move over the lifted card is the only event left, and
  // the band going false is what releases it. The halves this file CAN observe
  // are pinned by "releases a lifted card when the pointer drops below its
  // exposed strip, and lifts the one under it instead", which deleting the
  // move binding reddens.
  //
  // `currentTarget` here IS the element that takes `z-10`, which is why that
  // row stubs the rect and asserts the class on one node. The pool measures
  // the same box one level down, on the button inside the wrapper that takes
  // the class — `git grep -n "block w-full" --
  // client/src/components/draft/workspace/WorkspaceCard.tsx`.
  //
  // Touch is excluded rather than mouse required, because a pen genuinely
  // hovers. That is this repo's idiom for a hover affordance:
  // `git grep -n 'pointerType === "touch") return' -- client/src/hooks
  // client/src/components/draft/workspace` prints the identical guard in
  // `WorkspaceCard.tsx::WorkspaceCard`, `useCardHover.ts::useCardHover` and
  // `useInspectHoverProps.ts::useInspectHoverProps`, and
  // `useCardHover.test.tsx` runs its hover rows over `["mouse", "pen"]` on the
  // stated ground that only touch synthesizes its enter from a tap. Both
  // halves are pinned here, by "gives a touch pointer no hover lift" and
  // "lifts a covered card for a pen pointer too". Touch gets its OWN lift, off
  // a tap rather than a hover — see `onPointerDown`/`onClick` below.
  const updateLift = (event: PointerEvent<HTMLDivElement>) => {
    if (event.pointerType === "touch") return;
    const rect = event.currentTarget.getBoundingClientRect();
    onLiftChange(event.clientY - rect.top <= rect.width * STACK_EXPOSED_WIDTH_RATIO);
  };

  return (
    <div
      data-winston-revealed-card={card.instance_id}
      // `relative` and `max-w-full` are load-bearing rather than decoration,
      // and both are asserted: `relative` puts this card in the same positioned
      // group as the card backs above it, and `max-w-full` is the cap a narrow
      // column applies. What each one then DOES — tree order deciding who
      // paints on top, a used width following the column down — is CSS
      // behaviour this repo cannot observe. `shrink-0` is for
      // `ForcedDrawNotice`, whose parent is a flex row; in the block stack it
      // is inert.
      className={`relative shrink-0 max-w-full overflow-hidden rounded-md ring-1 ring-white/15 focus-visible:outline focus-visible:outline-2 focus-visible:outline-amber-300/70 ${
        lifted ? "z-10" : ""
      }`}
      style={{ width, aspectRatio: CARD_ASPECT, marginTop: stackMarginTop(stackIndex, spread) }}
      // Keyboard parity with the decision buttons below, which are real
      // `<button>`s: a player who tabs to Take must be able to reach what they
      // are taking. `role="button"` and the Enter/Space handler below are for
      // a DIFFERENT reason than keyboard reach, though — REASONING, not
      // measured; no lane here drives a real tap against a real browser's
      // click-synthesis rules (see the module doc's LAYOUT paragraph): iOS
      // Safari only synthesises its compatibility `click` on an element it
      // considers clickable — a link, a form control, `cursor: pointer`, `role="button"`
      // or an own `onclick` — and this card had none of those, so a tap could
      // silently never fire `onClick` there at all. Every other clickable
      // focusable `div` in this client already carries this same
      // `role="button"` plus Enter/Space pair (`MyDecks.tsx`,
      // `HomeDashboard.tsx`), and `WorkspaceCard.tsx::WorkspaceCard` — the
      // real `<button>` this `onClick` was modelled on — gets Enter and Space
      // for free from being a native button rather than needing them written
      // out. Enter/Space toggle the SAME lift a tap does, but read the live
      // `lifted` prop directly rather than going through `tapRef`: the ref
      // exists to survive a browser inserting its own focus-write BETWEEN
      // `pointerdown` and `click`, and no such write can land between a
      // keydown and its own handler on an element that is already focused by
      // the time the key is pressed — there is nothing here for a snapshot to
      // protect against. Pinned by "toggles the lift when the focused card
      // takes Enter" and "toggles the lift when the focused card takes
      // Space".
      role="button"
      tabIndex={0}
      aria-label={card.name}
      aria-pressed={lifted}
      onKeyDown={(event) => {
        if (event.key !== "Enter" && event.key !== " ") return;
        event.preventDefault();
        onLiftChange(!lifted);
      }}
      onPointerEnter={updateLift}
      onPointerMove={updateLift}
      onPointerDown={(event) => {
        tapRef.current = { pointerType: event.pointerType, wasLifted: lifted };
      }}
      // The leave clears the lift for non-touch pointers, focused or not, and
      // each half has its own row: "lifts a covered card clear of its
      // neighbour when the pointer is on its exposed strip" enters here off a
      // hover lift, and "drops a focus lift when the pointer leaves the card"
      // off a focus lift. Deleting this binding reds both; guarding it with a
      // second focus-only flag reds only the second.
      //
      // GUARDED against touch. The finger-lift a tap fires between
      // `pointerup` and `click` (the same event this repo already documents
      // at `useCardHover.ts`/`useInspectHoverProps.ts`) would otherwise clear
      // whatever this card held mid-tap; `onClick` below computes the tap's
      // own final state from `tapRef`'s pre-tap snapshot rather than this
      // card's live `lifted`, which already covers every ordering this repo
      // CAN drive (pointerdown, then this leave, then click). Whether some
      // browser could ever deliver that leave AFTER the click instead is a
      // claim about BROWSER EVENT ORDERING — REASONING, not measured; no lane
      // here drives a real tap (see the module doc's LAYOUT paragraph). If it
      // did, the snapshot would not help — the click has already computed and
      // applied its toggle by then — and the first tap's lift would be wiped,
      // restoring the original bug. The guard closes that gap directly, at
      // zero measured cost: MEASURED, adding it reddens nothing in this file
      // or `DraftPodPage.winston.test.tsx`. Behavioural consequence: a touch
      // PAN that starts on a lifted card no longer clears it through this
      // leave.
      onPointerLeave={(event) => {
        if (event.pointerType === "touch") return;
        onLiftChange(false);
      }}
      // A focus has no cursor, so there is no `clientY` to band-test and the
      // lift is unconditional. ONE flag serves focus, hover and tap, so a
      // pointer move below the strip drops a lift a focus put there — pinned
      // by "drops a focus lift once the pointer moves below the card's exposed
      // strip", and deliberate rather than incidental. REASONING about browser
      // hit-testing again: a focus-lifted card's `z-10` box covers the card
      // under it, so separate focus and hover flags would leave that card
      // unreachable by pointer for as long as the focus held. The flag itself
      // now lives one level up, in `Pile`'s `liftedCard`, so at most one card
      // in a pile holds it at a time — pinned by "lifts one card at a time in
      // a pile".
      onFocus={() => onLiftChange(true)}
      onBlur={() => onLiftChange(false)}
      // Allowlisted on the PRECEDING `pointerdown`'s own `pointerType`, never
      // on this click's — a compatibility `click`'s own `pointerType` field is
      // populated inconsistently across browsers (Chromium and WebKit dispatch
      // it as a `PointerEvent`; Firefox has historically dispatched a bare
      // `MouseEvent`), while a genuine `pointerdown` always carries one. Fails
      // CLOSED on anything but touch: a mouse tap gets nothing here, because
      // the hover band above already serves it, and a click toggle would fight
      // `updateLift` on the very next `pointermove`. `tapRef.current` is
      // cleared after reading so a later bare `click` with no preceding
      // `pointerdown` (a mouse click synthesized without one, or a bare test
      // `fireEvent.click`) reads `null` and does nothing — pinned by "clears
      // a completed tap's own record before a later bare click reads it".
      onClick={() => {
        const tap = tapRef.current;
        tapRef.current = null;
        if (tap?.pointerType !== "touch") return;
        onLiftChange(!tap.wasLifted);
      }}
    >
      {isLoading || src === null ? (
        <span className="flex h-full items-center justify-center bg-white/5 px-1 text-center text-[10px] leading-tight text-white/60">
          {card.name}
        </span>
      ) : (
        <img src={src} alt={displayName} draggable={false} className="h-full w-full object-contain" />
      )}
    </div>
  );
}

// ── Face-down stack ─────────────────────────────────────────────────────

/**
 * Whether `FaceDownStack` draws anything for this pile.
 *
 * The single authority for that question, called by the component itself and by
 * the caller, which needs to know whether the first revealed card is the top of
 * the column or is stacking onto a fan.
 *
 * `false` is the one case where nothing is face down and cards are face up: the
 * seat is looking at the WHOLE pile, which is the active seat's state at the
 * cursor on every turn — every engine write site of the `inspected` contract
 * sets the cursor pile's entry to that pile's own length. There are three:
 * `session::apply_start_draft` at draft start, and, in
 * `shared_stack::apply_shared_stack_decision`, the decline cursor-advance and
 * the turn-end reset (which zeroes the whole vector first, then sets index 0).
 * `git grep -n 'inspected\[' -- crates/draft-core/src` is the enumeration: it
 * spans `session.rs`, `shared_stack.rs`, `types.rs` and `view.rs`, and those
 * three are the only assignments among its hits that are neither a doc comment
 * nor inside a `#[cfg(test)]` module. Drawing an empty slot there would claim a
 * card nobody has seen, on the one pile the screen is about, and cost it a card
 * of height.
 *
 * The remaining quadrant, `(count > 0, total === 0)`, is unreachable rather
 * than handled. Both call sites (`git grep -n drawsFaceDownStack -- client/src`
 * — `FaceDownStack`'s own guard and `Pile`'s `stackIndex`) pass the same
 * derived pair, and `count` in it is only ever
 * `Math.max(0, pile.total - shownRevealed.length)`, which is 0 whenever `total`
 * is. The `||` is written for the three quadrants that occur.
 */
function drawsFaceDownStack(count: number, total: number): boolean {
  return count > 0 || total === 0;
}

/**
 * A pile's face-down cards, drawn as overlapping card backs.
 *
 * A HEIGHT, never contents — the same contract the number it replaces had. The
 * stack's order is published to nobody and a pile's unlooked-at cards to
 * nobody, so every back here is the same public card back and none of them
 * stands for a particular card: `count` is the truth, the backs are how a
 * player reads it without counting digits.
 */
function FaceDownStack({ count, total }: { count: number; total: number }) {
  const { t } = useTranslation("draft");
  const backs = Math.min(count, FACE_DOWN_STACK_MAX_BACKS);

  if (!drawsFaceDownStack(count, total)) return null;

  if (count === 0) {
    return (
      <div
        data-winston-pile-facedown
        data-winston-pile-facedown-count={count}
        role="img"
        aria-label={t("winston.pileTotal", { count })}
        className="w-full rounded-md border border-dashed border-white/12"
        style={{ aspectRatio: CARD_ASPECT }}
      />
    );
  }

  // No height of its own: the backs are in flow and their negative top margins
  // are what set it. GEOMETRY, worked through on paper and not measured here —
  // the fan runs a card tall plus one strip per further back, and the first
  // revealed card pulls up by a card less a strip, which leaves the LAST back
  // showing exactly one strip — the same strip every other covered card in the
  // column shows — and so makes the column one uniform stack from the deepest
  // back to the newest card.
  return (
    <div
      data-winston-pile-facedown
      data-winston-pile-facedown-count={count}
      role="img"
      aria-label={t("winston.faceDownStack", { count })}
      className="w-full"
    >
      {Array.from({ length: backs }, (_, index) => (
        <CardBackFallback
          key={index}
          // Positioned, in flow, and carrying no `z-index`. The first back is
          // the bottom of the stack and later backs come after it in the
          // document, and every card in the column — backs and revealed cards
          // alike — is positioned and unlayered. That every element is
          // positioned is the precondition, and the part that is asserted; what
          // follows from it (tree order alone deciding who paints on top, and
          // an explicit `z-index` here painting a back over the revealed card
          // that overlaps it) is CSS behaviour no test here observes.
          className="relative block rounded-md ring-1 ring-white/12"
          style={{
            width: "100%",
            aspectRatio: CARD_ASPECT,
            marginTop: stackMarginTop(index),
          }}
        />
      ))}
    </div>
  );
}

// ── One pile ────────────────────────────────────────────────────────────

function Pile({
  pile,
  isCursor,
  canDecide,
  interactionLocked,
  cardWidth,
  onDecide,
}: {
  pile: SharedStackPileView;
  /** This is the pile being decided on. PUBLIC: every viewer sees the
   *  highlight, because the cursor is open information at the table. */
  isCursor: boolean;
  /** This viewer is the active seat AND this is the cursor pile, so the
   *  controls belong to them. Strictly narrower than `isCursor` — an onlooker
   *  must never be offered a button the reducer would refuse. */
  canDecide: boolean;
  interactionLocked: boolean;
  cardWidth: number;
  onDecide: (pile: number, decision: SharedStackPileDecision) => void;
}) {
  const { t } = useTranslation("draft");
  // Piles are 0-indexed on the wire and 1-indexed in copy. A display offset, and
  // the only number this component derives at all.
  const label = pile.index + 1;

  // At most one revealed card in THIS pile is lifted at a time — `null` when
  // none is. Owned here rather than by each `RevealedCard` because a lifted
  // card takes `z-10` over the one after it: two lifted at once in the same
  // stack would make the covered one untappable. Pinned by "lifts one card at
  // a time in a pile".
  const [liftedCard, setLiftedCard] = useState<string | null>(null);
  // Un-stacks this pile's revealed faces, per the header button below.
  // Ephemeral and NOT persisted: a spread is about the cards CURRENTLY in this
  // pile, and piles turn over every turn, so restoring "this pile was spread"
  // next session would restore a state whose subject no longer exists (unlike
  // pile SCALE, which has no such subject and is why that one persists).
  const [spread, setSpread] = useState(false);

  // Same device as `PICK_STATUS_KEY` in the spectator dashboard, for the same
  // reason: interpolating a refusal into a translation key builds that key out
  // of a serialized engine value, and a refusal the engine grows later reaches
  // the screen as its own key text instead of failing the build. MEASURED on the
  // sibling case -- adding a variant to the pick-status union broke exhaustive
  // `Record`s elsewhere and did NOT break the interpolated call. A total
  // `Record` keyed on `SharedStackRefusal` makes it a compile error here.
  const REFUSAL_KEY = {
    PileNotActive: "winston.refusal.PileNotActive",
    PileEmpty: "winston.refusal.PileEmpty",
    NoGuaranteedCard: "winston.refusal.NoGuaranteedCard",
  } as const satisfies Record<SharedStackRefusal, string>;

  const decisionButton = (decision: SharedStackPileDecision, tone: "emerald" | "neutral") => {
    const refusal = verdictFor(pile, decision);
    // Disabled unless the engine published "legal". An unpublished verdict is NOT
    // treated as permission: no verdict, no control.
    const refused = refusal !== null;
    const reason = refusal === undefined
      ? t("winston.refusalUnpublished")
      : refusal === null ? undefined : t(REFUSAL_KEY[refusal]);
    const disabled = refused || interactionLocked;
    return (
      <button
        type="button"
        data-winston-decision={decision}
        disabled={disabled}
        title={reason}
        aria-label={t(decision === "Take" ? "winston.takeAria" : "winston.declineAria", { index: label })}
        aria-describedby={reason === undefined ? undefined : `winston-refusal-${pile.index}-${decision}`}
        onClick={() => onDecide(pile.index, decision)}
        // `w-full` in place of the row layout's `min-w-[6rem]`: in a
        // one-card-wide column a per-button floor is a floor the column cannot
        // meet, so the buttons fill instead. `menuButtonClass`'s `sm` size is
        // unchanged and still carries `min-h-11`, the touch target that had to
        // survive (`buttonStyles.ts::menuButtonClass`).
        //
        // And no padding override, which could not work here anyway: `className`
        // is appended to the class ATTRIBUTE, and attribute order does not
        // decide a Tailwind conflict — stylesheet source order does. The
        // compiled sheet emits `.px-4` (which `sm` already carries) AFTER
        // `.px-2`, in the same `@layer utilities` and at the same specificity,
        // so a `px-2` passed here would be inert. Against a running dev server:
        // `curl -s 'http://[::1]:5173/src/index.css?direct' | grep -n '\.px-2 {\|\.px-4 {'`.
        className={menuButtonClass({ tone, size: "sm", disabled, className: "w-full" })}
      >
        {t(decision === "Take" ? "winston.take" : "winston.decline")}
      </button>
    );
  };

  const refusalNotes = (["Take", "Decline"] as const).flatMap((decision) => {
    const refusal = verdictFor(pile, decision);
    if (refusal === null) return [];
    return [{
      decision,
      text: refusal === undefined ? t("winston.refusalUnpublished") : t(REFUSAL_KEY[refusal]),
    }];
  });

  // RENDER THE PROJECTION. `revealed` is not "what is face up on the table" --
  // it is what the ENGINE has decided this seat is entitled to know, and the
  // engine computes it to the Winston rule exactly. A decline APPENDS the card
  // it draws (`shared_stack::apply_shared_stack_decision`) and the view slices
  // `pile[..inspected[i]]` and never by `pile.len()`, so the card a seat just
  // buried sits beyond the prefix STRUCTURALLY. The entitlement also lapses on
  // its own: `inspected` is zeroed for every pile at the start of each turn.
  //
  // So the paper rule is already enforced, and enforced better than paper. The
  // reason you may not re-examine a declined pile at a table is that you would
  // see the new card too; here you cannot see it, and the cards you did see are
  // yours to keep for the rest of your turn.
  //
  // This surface USED TO re-hide declined piles on top of that, which was a
  // second visibility authority in the display layer and, worse, a one-sided
  // one: `bot_ai::opponent_read` joins these same prefixes against the public
  // decline history to read which colours are open, so hiding them took that
  // inference away from the human and left it with the bot. Deleted. A declined
  // pile is de-emphasised below, not blanked.
  const shownRevealed = pile.revealed;
  // The face-down remainder of THIS pile: a presentation split of one published
  // number into the part drawn face up and the part that is not. It answers no
  // legality question — those come from `pile.legality` — which is the property
  // that matters, not that it happens to be arithmetic. Clamped at zero so a
  // projection this component has not anticipated shrinks the stack rather than
  // asking for a negative fan.
  const faceDownCount = Math.max(0, pile.total - shownRevealed.length);

  return (
    <div
      data-winston-pile={pile.index}
      data-winston-pile-active={isCursor ? "true" : "false"}
      // `p-2` where the row layout had `p-3`: a pile is now a column about one
      // card wide, so its padding is taken out of the card's own drawn width
      // rather than out of a full row's slack. A sizing judgement about a
      // layout nothing in this repo measures; the declaration itself is pinned
      // by "centres each pile's stack in its column and keeps the column's
      // padding thin".
      className={`flex min-w-0 flex-col gap-2 rounded-[16px] border p-2 ${
        isCursor
          ? "border-amber-300/40 bg-amber-400/[0.06] shadow-[inset_0_-1px_0_rgba(0,0,0,0.28)]"
          : "border-hairline bg-white/[0.035]"
      }`}
    >
      <div className="flex min-w-0 flex-wrap items-baseline gap-x-3 gap-y-1">
        <span className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-white/60">
          {t("winston.pileLabel", { index: label })}
        </span>
        <span data-winston-pile-total className="shrink-0 text-xs tabular-nums text-white/45">
          {t("winston.pileTotal", { count: pile.total })}
        </span>
        {isCursor && (
          <span className="text-[0.6rem] font-semibold uppercase tracking-[0.18em] text-amber-200/80">
            {t("winston.deciding")}
          </span>
        )}
        {/* Rendered on every pile, including one with nothing to spread: a
            `shownRevealed.length > 1` gate would make a LENGTH COMPARISON feed
            a control. The file already ships a control that is a no-op
            at an extreme on purpose: `PileScaleControls` stays `disabled={false}`
            and the "-" button does nothing at the scale floor.
            Never disabled on `interactionLocked`, same rule and the same
            reason as `PileScaleControls`: resizing which cards you can see is
            not a game action, and a player waiting out an opponent's turn is
            exactly who wants to use it. Pinned by "offers the spread toggle on
            every pile, including one with nothing to spread". `self-center`
            overrides the header's own `items-baseline` — a 44px button
            baseline-aligned against the uppercase labels beside it would sit
            low against them. */}
        <button
          type="button"
          data-winston-pile-spread={spread ? "true" : "false"}
          aria-expanded={spread}
          aria-label={t(spread ? "winston.stackPile" : "winston.spreadPile", { index: label })}
          title={t(spread ? "winston.stackPile" : "winston.spreadPile", { index: label })}
          onClick={() => setSpread((current) => !current)}
          className={menuButtonClass({
            tone: "neutral",
            size: "sm",
            disabled: false,
            className: "ml-auto self-center",
          })}
        >
          <span aria-hidden="true">{spread ? "▲" : "▼"}</span>
        </button>
      </div>

      {/* A pile this seat already declined stays READABLE but is visibly spent:
          the decision has moved on, and the cards are here as the memory the
          engine says this seat is entitled to, not as a live choice.

          `width: cardWidth` is load-bearing and not a cosmetic size — it is the
          containing block the stacked children's percentage top margins resolve
          against, so the overlap comes out of a card's width and not the grid
          column's. Its presence is asserted; that consequence is the CSS
          box-model rule the LAYOUT note flags as unmeasurable here, and the
          same goes for `maxWidth: 100%` capping the stack in a narrow column.

          `mx-auto` is new with the columns and not carried over: a fixed
          `cardWidth` inside a `minmax(0, 1fr)` track is narrower than its track
          at most scales, and without the auto margins it would sit against the
          track's leading edge instead of under the pile's own header. A sizing
          judgement of the same kind as `p-2` above — the declaration is pinned
          by "centres each pile's stack in its column and keeps the column's
          padding thin", what it then does is not measured here.

          No `overflow-x-auto` here, and none should come back: a scroll
          container would clip the card a Tab, or a hover on its exposed strip,
          lifts. That argument also rules out bounding a tall cursor pile with
          `max-height` + `overflow-y-auto`, so the trade taken here is that a
          long pile is a long column. Both halves are CSS reasoning, not
          measurement. */}
      <div
        data-winston-pile-stack
        data-winston-pile-spent={!isCursor && shownRevealed.length > 0 ? "true" : undefined}
        className={`mx-auto ${isCursor ? "" : "opacity-60 saturate-75"}`}
        style={{ width: cardWidth, maxWidth: "100%" }}
      >
        <FaceDownStack count={faceDownCount} total={pile.total} />
        {shownRevealed.map((card, index) => (
          <RevealedCard
            key={card.instance_id}
            card={card}
            width={cardWidth}
            // The fan, when there is one, is the top of the column, so the
            // first revealed card stacks onto it rather than sitting flush.
            stackIndex={(drawsFaceDownStack(faceDownCount, pile.total) ? 1 : 0) + index}
            spread={spread}
            lifted={liftedCard === card.instance_id}
            // Identity-guarded: a card's OWN "off" call (leave, blur, or a
            // hover band going false) only clears `liftedCard` when THIS card
            // is the one currently holding it. Without the guard, card A's
            // `onBlur` firing while card B is lifted by a live mouse hover
            // would wipe B's lift out from under the cursor still sitting on
            // it — pinned by "leaves a hovered card's lift alone when a
            // different card loses focus". A card's "on" call (`next === true`)
            // stays unconditional: focusing or hovering onto a card always
            // takes over the pile's one lift slot.
            onLiftChange={(next) =>
              setLiftedCard((current) =>
                next ? card.instance_id : current === card.instance_id ? null : current
              )
            }
          />
        ))}
        {/* No "you have not looked at this pile yet" placeholder, and its
            absence is load-bearing rather than an omission. ALL THREE engine
            write sites of the `inspected` contract set the cursor pile's entry
            to that pile's own length — see `drawsFaceDownStack` above for the
            grep that enumerates them — so at the cursor
            `revealed.length === total` ALWAYS. An empty `shownRevealed` AT THE
            CURSOR therefore means the pile is empty, never "unlooked-at" — and
            an empty pile is already stated twice over, by the empty slot
            `FaceDownStack` draws for a pile whose own total is zero and by the
            engine's `PileEmpty` refusal note below. Away from the cursor it means "face down", which is
            exactly what the stack of backs above it says. */}
      </div>

      {/* Under the stack rather than on the header line, and stacked rather
          than side by side, because a pile column is one card wide and the
          header's pair carried a `min-w-[6rem]` floor each — a sizing judgement
          about a layout nothing here measures. What IS pinned is that both
          buttons stay inside this pile's own element. The PILE-SCOPED queries
          are `WinstonPileTable.test.tsx`'s `decisionButton` helper and the two
          `[data-winston-pile='1'] [data-winston-decision=…]` selectors in
          `DraftPodPage.winston.test.tsx`; the suite's other four
          `data-winston-decision` queries count the buttons document-wide, where
          the count is the same wherever they sit
          (`grep -rn data-winston-decision client/src`). MEASURED: rendering the
          actions block as a sibling of `[data-winston-pile]` instead of a child
          reddens exactly the rows that call the helper and no others, plus
          both of `DraftPodPage.winston.test.tsx`'s scoped rows. */}
      {canDecide && (
        <div data-winston-pile-actions className="flex flex-col gap-1">
          {decisionButton("Take", "emerald")}
          {decisionButton("Decline", "neutral")}
        </div>
      )}

      {canDecide && refusalNotes.length > 0 && (
        <div className="flex flex-col gap-1">
          {refusalNotes.map((note) => (
            <p
              key={note.decision}
              id={`winston-refusal-${pile.index}-${note.decision}`}
              className="text-xs text-amber-200/70"
            >
              {note.text}
            </p>
          ))}
        </div>
      )}
    </div>
  );
}

// ── Forced-draw notice ──────────────────────────────────────────────────

/**
 * What the viewer's own final-pile decline drew off the main stack.
 *
 * The engine publishes `forced_draw` to the drawing seat alone, so this is
 * already private when it arrives. It is rendered for as long as the engine
 * keeps sending it — until this seat decides again — because the draw happens
 * at the END of a turn and the player's attention is on the board, not on a
 * flash they may have missed.
 */
function ForcedDrawNotice({
  card,
  width,
  gridColumn,
}: {
  card: DraftCardInstance;
  width: number;
  /** Track span inside the pile list's grid, supplied by the caller that owns
   *  the track count. The notice is not a pile and wants the whole row. */
  gridColumn: string;
}) {
  const { t } = useTranslation("draft");
  // Its own flag rather than a share of any pile's `liftedCard`: this is not a
  // pile and stacks nothing (no `stackIndex`, no `spread`), so there is no
  // sibling card to hand the slot off to.
  const [lifted, setLifted] = useState(false);

  return (
    <div
      data-winston-forced-draw={card.instance_id}
      role="status"
      className="flex items-center gap-3 rounded-[16px] border border-sky-300/30 bg-sky-400/[0.07] p-3"
      style={{ gridColumn }}
    >
      <RevealedCard card={card} width={width} lifted={lifted} onLiftChange={setLifted} />
      <div className="flex min-w-0 flex-col gap-0.5">
        <span className="text-[0.6rem] font-semibold uppercase tracking-[0.18em] text-sky-200/80">
          {t("winston.forcedDrawLabel")}
        </span>
        <p className="text-sm text-white/75">{t("winston.forcedDraw", { name: card.name })}</p>
      </div>
    </div>
  );
}

// ── Scale controls ──────────────────────────────────────────────────────

/**
 * The pack surface's scale control, on the pile surface's own stored value.
 *
 * Deliberately the same three affordances in the same order as `PackDisplay`'s
 * desktop row (slider, −, reset, +): a player who has learned one draft screen
 * has learned this one, and "except ours is pile scale" is the whole difference.
 */
function PileScaleControls({
  pileScale,
  setPileScale,
  disabled,
}: {
  pileScale: number;
  setPileScale: (next: number) => void;
  disabled: boolean;
}) {
  const { t } = useTranslation("draft");

  return (
    <div data-pile-scale-controls className="ml-auto flex shrink-0 items-center gap-2">
      <label className="flex items-center gap-2 text-xs text-white/45">
        {t("winston.scale")}
        <input
          type="range"
          min={DRAFT_WORKSPACE_PILE_SCALE_MIN}
          max={DRAFT_WORKSPACE_PILE_SCALE_MAX}
          step={DRAFT_WORKSPACE_PILE_SCALE_STEP}
          value={pileScale}
          disabled={disabled}
          onChange={(event) => setPileScale(Number(event.target.value))}
          aria-label={t("winston.scale")}
          className="min-w-0 w-[6.5rem] max-w-full"
        />
      </label>
      <button
        type="button"
        disabled={disabled}
        aria-label={t("winston.scaleDecrease")}
        onClick={() => setPileScale(repairDraftWorkspacePileScale(pileScale - 0.1))}
        className={menuButtonClass({ tone: "neutral", size: "icon", disabled })}
      >
        −
      </button>
      <button
        type="button"
        disabled={disabled}
        aria-label={t("winston.scaleReset")}
        onClick={() => setPileScale(DRAFT_WORKSPACE_PILE_SCALE_DEFAULT)}
        className={menuButtonClass({ tone: "neutral", size: "icon", disabled })}
      >
        ⟳
      </button>
      <button
        type="button"
        disabled={disabled}
        aria-label={t("winston.scaleIncrease")}
        onClick={() => setPileScale(repairDraftWorkspacePileScale(pileScale + 0.1))}
        className={menuButtonClass({ tone: "neutral", size: "icon", disabled })}
      >
        +
      </button>
    </div>
  );
}

// ── Component ───────────────────────────────────────────────────────────

export function WinstonPileTable({
  sharedStack,
  seats,
  viewerSeat,
  playFirstChooser,
  interactionLocked,
  onDecide,
  pileScale,
  setPileScale,
  responsiveLayout,
}: WinstonPileTableProps) {
  const { t } = useTranslation("draft");
  // NO PREVIEW OVERLAY OF ITS OWN, and the absence is deliberate rather than an
  // omission: the overlay this surface used to render was reported as covering
  // the piles the player was choosing between. A lifted card at the player's
  // own pile scale is what stands in — see the LAYOUT note at the top of this
  // file for what that costs. Pinned by "renders no preview overlay of its own
  // over the piles", which reddens if the element is put back.
  const { active_pile, active_seat, main_stack_remaining, total_cards, piles, forced_draw } = sharedStack;

  const seatName = (seat: number) =>
    seats.find((entry) => entry.seat_index === seat)?.display_name
    ?? t("winston.seatFallback", { index: seat + 1 });

  // A seat comparison against the engine's published `active_seat`, which is THE
  // authority for whose turn it is. Emphatically not `active_pile !== null`: the
  // cursor is published to every viewer (see `SharedStackView.active_pile`), so
  // that test would answer "your turn" to onlookers and spectators alike and
  // hand them controls the reducer refuses.
  const yourTurn = viewerSeat !== null && viewerSeat === active_seat;
  // A percentage of two published counts, for a bar width only. It answers no
  // question about any control.
  const faceDownPercent = total_cards === 0 ? 0 : (main_stack_remaining / total_cards) * 100;
  // The same base width the pack surface scales, so one notch of pile scale and
  // one notch of pack scale mean the same thing on screen.
  const cardWidth = DRAFT_PACK_CARD_BASE_WIDTH_PX * pileScale;
  // See `responsiveLayout`: off desktop this surface is inside a fixed-height
  // `overflow-hidden` box, so it has to scroll its own columns or the cursor
  // pile's controls become unreachable at a scale the player chose.
  const ownsHeight = responsiveLayout !== "desktop";

  return (
    <section
      data-winston-pile-table
      data-winston-scrolls-piles={ownsHeight ? "true" : "false"}
      aria-label={t("winston.heading")}
      className={`mb-2 flex w-full min-w-0 flex-col gap-2 ${
        ownsHeight ? "h-full min-h-0" : ""
      }`}
    >
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-[16px] border border-hairline bg-white/[0.035] px-4 py-2 shadow-[inset_0_-1px_0_rgba(0,0,0,0.28)]">
        <span data-winston-turn role="status" aria-live="polite" className="text-sm font-semibold text-fg">
          {yourTurn
            ? t("winston.yourTurn", { index: active_pile + 1 })
            : t("winston.otherSeatTurn", { name: seatName(active_seat) })}
        </span>
        <span className="text-xs text-white/45">
          {t("winston.mainStackRemaining", { count: main_stack_remaining })}
        </span>
        <span className="shrink-0 text-xs tabular-nums text-white/45">
          {t("winston.cardsLeft", { count: total_cards })}
        </span>
        {/* Never disabled on `interactionLocked`: resizing the cards is not a
            game action, and a player waiting out an opponent's turn is exactly
            who wants to adjust it. */}
        <PileScaleControls pileScale={pileScale} setPileScale={setPileScale} disabled={false} />
      </div>

      <div
        role="img"
        aria-label={t("winston.stackShare")}
        className="h-1.5 w-full overflow-hidden rounded-full bg-white/5"
      >
        <div
          data-winston-stack-share
          className="h-full rounded-full bg-amber-400/50 transition-[width] duration-200"
          style={{ width: `${faceDownPercent}%` }}
        />
      </div>

      <p className="text-xs text-white/40">
        {yourTurn ? t("winston.turnHint") : t("winston.hiddenPiles")}
      </p>

      {/* One grid column per PUBLISHED pile, never a hard-coded three:
          `pile_count` is draft-core data, not this layer's
          (`PackDistribution::SharedStackPiles { pile_count }` in
          `crates/draft-core/src/types.rs`), and a layout that assumed its
          current value would be a second authority for a number the projection
          already carries. */}
      <div
        data-winston-pile-list
        className={`grid min-w-0 gap-2 ${
          ownsHeight ? "min-h-0 flex-1 overflow-y-auto pr-1" : ""
        }`}
        style={{ gridTemplateColumns: `repeat(${piles.length}, minmax(0, 1fr))` }}
      >
        {/* INSIDE THE SCROLLER, not above it. The notice holds a full-size card
            with a fixed width and aspect ratio, so its min-content height is
            definite and flexbox cannot shrink it. As a sibling ABOVE the pile
            list it was therefore an unshrinkable block competing with the only
            `flex-1 min-h-0` item in a fixed-height box — on a short viewport it
            took the whole box and collapsed the list to zero, where
            `overflow-y-auto` scrolls nothing and Take/Decline become
            unreachable. And it is on screen for the whole of the seat's NEXT
            turn (the engine keeps `forced_draw` until they decide again), which
            is exactly when those buttons are needed. Scrolling with the piles
            costs nothing: it is a notice, not a control.

            Own-seat only, restated here rather than inferred from the field's
            presence — though see the note on `viewerSeat` below for what that
            restatement does and does not buy. Deliberately NOT gated on
            `yourTurn`: the notice describes the turn that just ENDED, so by the
            time it matters the active seat is the opponent.

            A grid item now, so it is given the whole row rather than a third of
            it: the notice is a card beside a sentence, not a pile. */}
        {forced_draw !== null && viewerSeat !== null && (
          <ForcedDrawNotice
            card={forced_draw}
            width={cardWidth}
            gridColumn="1 / -1"
          />
        )}
        {piles.map((pile) => (
          <Pile
            key={pile.index}
            pile={pile}
            isCursor={pile.index === active_pile}
            canDecide={yourTurn && pile.index === active_pile}
            interactionLocked={interactionLocked}
            cardWidth={cardWidth}
            onDecide={onDecide}
          />
        ))}
      </div>

      {playFirstChooser !== null && playFirstChooser !== undefined && (
        <p data-winston-play-first className="text-xs text-white/40">
          {t("winston.playFirstChooser", { name: seatName(playFirstChooser) })}
        </p>
      )}
    </section>
  );
}
