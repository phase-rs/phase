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
 *   * what may be shown face up comes from `pile.revealed` — and this surface
 *     shows LESS than that, deliberately. The engine keeps publishing the prefix
 *     of every pile the active seat has looked at this turn, which is correct:
 *     that seat did look at them, so nothing is being withheld that it does not
 *     already know. But at a physical table a declined pile goes back face down,
 *     and remembering it is the player's job rather than the screen's — so only
 *     the pile under decision is drawn face up, and a pile the seat has passed
 *     goes dark the instant the cursor leaves it.
 *
 *     The DIRECTION of that gap is what makes it safe: rendering fewer cards
 *     than the projection permits can never leak, while rendering more is a bug
 *     no projection could catch. It is the one place this file narrows what the
 *     engine allows, and it narrows on `active_pile` — the engine's own cursor —
 *     never on a rule of its own. The remainder, including the card a decline
 *     just added, is face down and drawn as card BACKS: a real height, still no
 *     contents;
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
 * LAYOUT. The piles are ROWS, not columns, and the scale control is the pack
 * surface's control on its own stored value. A Winston turn is a decision about
 * one pile's cards, so that pile gets the full width of the surface to show them
 * side by side at a readable size; the two piles that are not being decided on
 * are heights, and a height reads better as a fanned stack of card backs than as
 * a number in a box. Three equal columns gave the cards being decided on a third
 * of the width and the two face-down piles the other two thirds, which is
 * backwards.
 */

import { useState } from "react";
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
import type { CardHoverInfo } from "../card/CardPreview";
import { HoverCardPreview } from "../card/HoverCardPreview";
import { mouseHoverPreview, type CardHoverHandler } from "../deck-builder/hoverPreview";
import { menuButtonClass } from "../menu/buttonStyles";
import { usePreferencesStore } from "../../stores/preferencesStore";
import {
  DRAFT_PACK_CARD_BASE_WIDTH_PX,
  DRAFT_WORKSPACE_PILE_SCALE_DEFAULT,
  DRAFT_WORKSPACE_PILE_SCALE_MAX,
  DRAFT_WORKSPACE_PILE_SCALE_MIN,
  DRAFT_WORKSPACE_PILE_SCALE_STEP,
  repairDraftWorkspacePileScale,
} from "./workspace/workspacePreferences";
import { useDraftCardFace } from "./DraftCardFace.tsx";

/** Card aspect, shared by every face-up card, card back and empty slot here. */
const CARD_ASPECT = "488 / 680";

/**
 * How many card backs a face-down stack draws before it stops adding them.
 *
 * A display cap and nothing else: the pile's real height is published as
 * `total` and is always rendered as a number beside the stack. Fanning one back
 * per card would make a 20-card pile wider than the surface while saying
 * nothing a reader can count at a glance.
 */
const FACE_DOWN_STACK_MAX_BACKS = 5;

/** Fraction of a card's width each further back in a fanned stack advances by. */
const FACE_DOWN_STACK_STEP = 0.16;

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
   * owns its height.
   *
   * On desktop the drafting column is `flex-col` in a page that scrolls, so the
   * rows can be as tall as the scale makes them. Every other band puts the
   * surface inside a fixed-height `overflow-hidden` box — and "every other" is
   * literally every viewport under 1200px wide, not just phones
   * (`getResponsiveDraftLayout`). Three full-card rows do not fit in a 56%
   * slice of `100dvh`, and a row that overflows an `overflow-hidden` parent
   * takes its Take/Decline buttons off-screen with no scrollbar to reach them.
   * So off desktop the pile list becomes this component's own scroller.
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
  onCardHover,
}: {
  card: DraftCardInstance;
  width: number;
  onCardHover: CardHoverHandler;
}) {
  const sourcePrinting = { setCode: card.set_code, collectorNumber: card.collector_number };
  const { src, isLoading, displayName } = useDraftCardFace(card.name, sourcePrinting);
  const preview = { name: card.name, sourcePrinting };

  return (
    // `mouseHoverPreview` rather than bare mouse handlers: it carries the
    // `data-deck-card-hover` marker the preview's own stale-hover sweep looks
    // for, and the pointerleave rule that keeps a narrow-viewport overlay from
    // closing the moment it opens over the card that opened it. A pile is
    // exactly the surface both were written for — its cards are replaced under
    // a stationary pointer every time the turn advances.
    <div
      data-winston-revealed-card={card.instance_id}
      className="shrink-0 overflow-hidden rounded-md ring-1 ring-white/15 focus-visible:outline focus-visible:outline-2 focus-visible:outline-amber-300/70"
      style={{ width, aspectRatio: CARD_ASPECT }}
      // Keyboard parity with the decision buttons below, which are real
      // `<button>`s: a player who tabs to Take must be able to reach what they
      // are taking. The card is not itself a control, so it takes focus and
      // publishes its name — it does not take Enter.
      tabIndex={0}
      aria-label={card.name}
      onFocus={() => onCardHover(preview)}
      onBlur={() => onCardHover(null)}
      {...mouseHoverPreview(onCardHover, preview)}
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
 * A pile's face-down cards, drawn as overlapping card backs.
 *
 * A HEIGHT, never contents — the same contract the number it replaces had. The
 * stack's order is published to nobody and a pile's unlooked-at cards to
 * nobody, so every back here is the same public card back and none of them
 * stands for a particular card: `count` is the truth, the backs are how a
 * player reads it without counting digits.
 */
function FaceDownStack({ count, total, width }: { count: number; total: number; width: number }) {
  const { t } = useTranslation("draft");
  const backs = Math.min(count, FACE_DOWN_STACK_MAX_BACKS);
  // Width of the fan: one full card plus a step for every back after the first.
  const spread = width * (1 + Math.max(0, backs - 1) * FACE_DOWN_STACK_STEP);

  if (count === 0) {
    // Nothing face down while cards are face up: the seat is looking at the
    // WHOLE pile, which is the active seat's state at the cursor on every turn
    // (both engine write sites of the `inspected` contract set
    // `inspected[cursor] = piles[cursor].len()`). Drawing an empty slot there
    // would claim a card nobody has seen, on the one pile the screen is about,
    // and cost it a card of width.
    if (total > 0) return null;
    return (
      <div
        data-winston-pile-facedown
        data-winston-pile-facedown-count={count}
        role="img"
        aria-label={t("winston.pileTotal", { count })}
        className="relative shrink-0 rounded-md border border-dashed border-white/12"
        style={{ width, aspectRatio: CARD_ASPECT }}
      />
    );
  }

  return (
    <div
      data-winston-pile-facedown
      data-winston-pile-facedown-count={count}
      role="img"
      aria-label={t("winston.faceDownStack", { count })}
      className="relative shrink-0"
      style={{ width: spread, aspectRatio: `${spread} / ${width * 680 / 488}` }}
    >
      {Array.from({ length: backs }, (_, index) => (
        <CardBackFallback
          key={index}
          className="absolute top-0 rounded-md ring-1 ring-white/12"
          style={{
            width,
            aspectRatio: CARD_ASPECT,
            left: width * index * FACE_DOWN_STACK_STEP,
            // The leftmost back is the bottom of the fan, so later backs sit on
            // top and the stack reads as one pile rather than a row of cards.
            zIndex: index,
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
  onCardHover,
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
  onCardHover: CardHoverHandler;
}) {
  const { t } = useTranslation("draft");
  // Piles are 0-indexed on the wire and 1-indexed in copy. A display offset, and
  // the only number this component derives at all.
  const label = pile.index + 1;

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
        className={menuButtonClass({ tone, size: "sm", disabled, className: "min-w-[6rem]" })}
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
      className={`flex min-w-0 flex-col gap-2 rounded-[16px] border p-3 ${
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
        {canDecide && (
          <span className="ml-auto flex shrink-0 gap-2">
            {decisionButton("Take", "emerald")}
            {decisionButton("Decline", "neutral")}
          </span>
        )}
      </div>

      {/* A pile this seat already declined stays READABLE but is visibly spent:
          the decision has moved on, and the cards are here as the memory the
          engine says this seat is entitled to, not as a live choice. */}
      <div
        data-winston-pile-spent={!isCursor && shownRevealed.length > 0 ? "true" : undefined}
        className={`flex min-w-0 items-start gap-2 overflow-x-auto pb-1 [scrollbar-width:thin] ${
          isCursor ? "" : "opacity-60 saturate-75"
        }`}
      >
        <FaceDownStack count={faceDownCount} total={pile.total} width={cardWidth} />
        {shownRevealed.map((card) => (
          <RevealedCard
            key={card.instance_id}
            card={card}
            width={cardWidth}
            onCardHover={onCardHover}
          />
        ))}
        {/* No "you have not looked at this pile yet" placeholder, and its
            absence is load-bearing rather than an omission. BOTH engine write
            sites of the `inspected` contract set `inspected[cursor] =
            piles[cursor].len()` (the turn-end reset and the decline's
            cursor-advance), so at the cursor `revealed.length === total`
            ALWAYS. An empty `shownRevealed` AT THE CURSOR therefore means the
            pile is empty, never "unlooked-at" — and an empty pile is already
            stated twice over, by the empty slot `FaceDownStack` draws for a
            pile whose own total is zero and by the engine's `PileEmpty` refusal
            note below. Away from the cursor it means "face down", which is
            exactly what the stack of backs beside it says. */}
      </div>

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
  onCardHover,
}: {
  card: DraftCardInstance;
  width: number;
  onCardHover: CardHoverHandler;
}) {
  const { t } = useTranslation("draft");

  return (
    <div
      data-winston-forced-draw={card.instance_id}
      role="status"
      className="flex items-center gap-3 rounded-[16px] border border-sky-300/30 bg-sky-400/[0.07] p-3"
    >
      <RevealedCard card={card} width={width} onCardHover={onCardHover} />
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
  // THIS SURFACE OWNS ITS PREVIEW, rather than reporting hovers to the page.
  //
  // A pack renders its cards at the player's own `packScale`, large enough to
  // read where they sit, so `draftCardPreviewMode` — which ships as "none" —
  // is a fair default there. A pile row is read at a glance and decided on, so
  // here an enlarged preview is part of the surface and "none" would leave the
  // decision harder than it needs to be. Only "none" is overridden: a mode the
  // player actually chose is theirs, "follow" and "shift" included. The side
  // dock is the substitute because it is the one placement that cannot cover
  // the piles the pointer is moving between.
  //
  // Owning it locally is what keeps that override scoped to these cards. The
  // page's own preview still serves the pool workspace on this same screen,
  // and still reads the preference straight.
  const draftCardPreviewMode = usePreferencesStore((s) => s.draftCardPreviewMode);
  const [hoveredCard, setHoveredCard] = useState<CardHoverInfo | null>(null);
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
  // `overflow-hidden` box, so it has to scroll its own rows or the cursor pile's
  // controls become unreachable at a scale the player chose.
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

      <div
        data-winston-pile-list
        className={`flex min-w-0 flex-col gap-2 ${
          ownsHeight ? "min-h-0 flex-1 overflow-y-auto pr-1" : ""
        }`}
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
            time it matters the active seat is the opponent. */}
        {forced_draw !== null && viewerSeat !== null && (
          <ForcedDrawNotice card={forced_draw} width={cardWidth} onCardHover={setHoveredCard} />
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
            onCardHover={setHoveredCard}
          />
        ))}
      </div>

      {playFirstChooser !== null && playFirstChooser !== undefined && (
        <p data-winston-play-first className="text-xs text-white/40">
          {t("winston.playFirstChooser", { name: seatName(playFirstChooser) })}
        </p>
      )}

      {/* `onDismiss` clears THIS component's state, which is the only state the
          overlay is showing — the default would clear the in-game inspector
          instead and leave a narrow-viewport overlay undismissable. `compact`
          for the same reason the pool review uses it: a blocking full-screen
          modal over a live turn is not a preview. */}
      <HoverCardPreview
        card={hoveredCard}
        mode={draftCardPreviewMode === "none" ? "side" : draftCardPreviewMode}
        hoverDelayMs={0}
        mobileLayout="compact"
        onDismiss={() => setHoveredCard(null)}
      />
    </section>
  );
}
