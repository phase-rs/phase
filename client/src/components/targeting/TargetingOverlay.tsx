import { AnimatePresence, motion } from "framer-motion";
import { Fragment, useCallback, useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

import { useCanActForWaitingState, usePlayerId } from "../../hooks/usePlayerId.ts";
import { getSeatColor } from "../../hooks/useSeatColor.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getOpponentDisplayName } from "../../stores/multiplayerStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import {
  boardChoiceSelectedPower,
  buildBoardChoiceAction,
  canConfirmBoardChoice,
  getBoardChoiceView,
  isBoardChoiceImmediate,
  type BoardChoiceView,
} from "../../viewmodel/gameStateView.ts";
import { renderDescription } from "../../utils/description.ts";
import type { GameEvent, GameObject, TargetChoiceKind, TargetObjectCategory } from "../../adapter/types.ts";
import { GAME_Z_LAYER } from "../../constants/ui.ts";
import { flattenRichLabel, RichLabel } from "../mana/RichLabel.tsx";

/** Ties the disclosure button to the panel it opens (`aria-controls`). */
const DESCRIPTION_PANEL_ID = "targeting-description-panel";

/**
 * The two frames the prompt renders in, selected by whether the slot is a
 * single optional pick. A frame is NOT a wrapper around a noun: each phrase is
 * authored whole, per locale, per frame, because the "up to one" marker
 * REPLACES the article in en, replaces it AND re-inflects the noun in pl, and
 * precedes a RETAINED article in de/es/fr/it/pt. There is no prefix rule.
 */
type TargetFrame = "one" | "upToOne";

/**
 * The seven noun slugs a targeting phrase key may end in. Defined as a literal
 * union so a typo in ANY row of the map is a compile error at the map literal.
 * It cannot be caught at the `t()` call: `t()` accepts a plain `string` here
 * (see PermanentCard.tsx's shipped `Record<AbilityBlockKind, string>` ->
 * `t(MAP[k])`), so a mistyped key would otherwise render a raw i18n key.
 *
 * Six of the seven come from the map below. `player` is the exception, because
 * `TargetChoiceKind::Players` carries no category to key on — `targetPhrase`
 * names it directly, and it is one of the rows the catalog gate hand-writes.
 */
type TargetNounSlug =
  | "player"
  | "spell"
  | "creature"
  | "planeswalker"
  | "nonlandPermanent"
  | "targetPermanent"
  | "target";

/**
 * The two key unions `targetPhrase` builds — 14 noun phrases plus the 2
 * conjunctions below, which is the whole 16-key catalog product. Typing each
 * key as a template literal rather than a bare `string` is what makes drift in
 * EITHER half — frame or slug — a compile error at the construction site,
 * since `t()` would accept any `string` and render it raw.
 *
 * `orPlayer` gets its OWN union rather than a row in `TargetNounSlug`, because
 * it is a conjunction template carrying a `{{noun}}` placeholder, not a noun.
 * Folding it in would make `phrase("orPlayer")` type-check as though it named
 * one, and that call passes no `noun`, so the placeholder would render raw.
 */
type TargetPhraseKey = `targeting.${TargetFrame}.${TargetNounSlug}`;
type TargetOrPlayerKey = `targeting.${TargetFrame}.orPlayer`;

/**
 * CR 109.1: every engine object category gets a noun. TOTAL over the mirror
 * union — a `Record`, so widening the union without adding a slug here is a
 * type error at `pnpm run type-check`, not a runtime fallback.
 *
 * That totality stops at the language boundary, and this comment used to claim
 * otherwise. The union above is hand-written: `derived_views.rs` carries no
 * `ts_rs` binding (the crate does have one, on `types/interaction.rs` —
 * `DerivedViews` deliberately does not use it), so adding a
 * `TargetObjectCategory` variant engine-side compiles, leaves the union
 * untouched, keeps this `Record` total, and ships green while the engine emits
 * a category no key covers. Widening the enum means widening the union in the
 * same change; nothing mechanical will remind you.
 *
 * The VALUE type is `TargetNounSlug`, not `string`: `t()` accepts a plain
 * `string` in this repo, so a mistyped slug would otherwise ship green and
 * render a raw i18n key to the user. The union catches it at THIS literal.
 *
 * `Object` maps to the generic `target` slug ON PURPOSE: it is the offer the
 * engine declined to classify further, so the phrase must not name a card type.
 * The article hazard this note used to carry is GONE — the article is no longer
 * hard-coded in a frame template, it is part of each authored phrase — so a
 * vowel-initial noun is now the translator's problem, not a silent en defect.
 *
 * What a new category still costs: 14 authored strings, two per locale, and one
 * grammatical hazard no type can catch. Polish `upToOne.*` opens with "do
 * jednego", whose form agrees with the noun's gender, and all seven current
 * Polish nouns are masculine. A FEMININE category needs "do jednej", and
 * nothing here or in the locale gates compares values, so it would ship green.
 *
 * EXPORTED for the catalog gate in TargetingOverlay.test.tsx. The union above
 * is hand-written and so is checked against itself; only a test that iterates
 * THIS object can catch a typo duplicated into both. Exporting is what makes
 * the list under test the list that ships.
 */
export const TARGET_NOUN_SLUG: Record<TargetObjectCategory, TargetNounSlug> = {
  Spell: "spell",
  Creature: "creature",
  Planeswalker: "planeswalker",
  NonlandPermanent: "nonlandPermanent",
  Permanent: "targetPermanent",
  Object: "target",
};

export function TargetingOverlay() {
  const { t } = useTranslation("game");
  const canActForWaitingState = useCanActForWaitingState();
  const localPlayerId = usePlayerId();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const objects = useGameStore((s) => s.gameState?.objects);
  const stack = useGameStore((s) => s.gameState?.stack);
  const seatOrder = useGameStore((s) => s.gameState?.seat_order);
  const targetKind = useGameStore((s) => s.gameState?.derived?.current_target_kind);
  const selectedCardIds = useUiStore((s) => s.selectedCardIds);
  const clearSelectedCards = useUiStore((s) => s.clearSelectedCards);

  const isTargetSelection = waitingFor?.type === "TargetSelection" || waitingFor?.type === "TriggerTargetSelection";
  const isCopyTargetChoice = waitingFor?.type === "CopyTargetChoice";
  const isCopyRetarget = waitingFor?.type === "CopyRetarget";
  const canKeepCurrentTargets = isCopyRetarget && waitingFor.data.target_slots.every((slot) => slot.current != null);
  const isExploreChoice = waitingFor?.type === "ExploreChoice";
  // CR 701.36a: Populate — choose a creature token you control to copy.
  const isPopulateChoice = waitingFor?.type === "PopulateChoice";
  // CR 303.4 + CR 303.4g + CR 115.1: Return-as-Aura attach pick. Picker is a
  // CHOICE (not a target), but the action shape mirrors ExploreChoice
  // (`GameAction::ChooseTarget` with the chosen ObjectId).
  const isReturnAsAuraTarget = waitingFor?.type === "ReturnAsAuraTarget";
  // CR 115.7: Single-target retargets (Bolt Bend, Misdirection) are picked on the
  // board through this overlay; multi-target retargets keep the dialog.
  const isRetargetChoice = waitingFor?.type === "RetargetChoice" && waitingFor.data.scope.type === "Single";
  // CR 115.7: Name the spell/ability being retargeted (the entry the redirect
  // resolved onto), so the player knows what they are choosing a new target for.
  const retargetSpellName = isRetargetChoice
    ? objects?.[stack?.[waitingFor.data.stack_entry_index]?.source_id ?? -1]?.name
    : undefined;
  const isTapCreatureChoice =
    waitingFor?.type === "PayCost" && waitingFor.data.kind.type === "TapCreatures";
  const boardChoice = getBoardChoiceView(waitingFor, objects);
  const isBoardChoice = boardChoice != null;
  const selectedBoardChoiceIds = useMemo(
    () => boardChoice
      ? selectedCardIds.filter((id) => boardChoice.objectIds.includes(id))
      : [],
    [boardChoice, selectedCardIds],
  );
  const targetSlots = isTargetSelection ? waitingFor.data.target_slots : [];
  const selection = isTargetSelection ? waitingFor.data.selection : null;
  const currentTargetSlot = isCopyRetarget
    ? (waitingFor.data.current_slot ?? 0)
    : (selection?.current_slot ?? 0);
  const activeSlot = targetSlots[currentTargetSlot];
  const isOptionalCurrentSlot = activeSlot?.optional === true;
  // CR 601.2c: display-only hint that this slot is announced by a non-controller
  // ("of an opponent's choice", e.g. Volcanic Offering). The engine routes the
  // prompt's `WaitingFor.player` to that announcer — who is exactly the viewer of
  // this overlay — so the slot is labelled whenever it carries any `chooser`.
  // This only labels the slot; no game logic in the client.
  const isOpponentChosenSlot = activeSlot?.chooser != null;
  // CR 115.1: a player is a legal target like any other. The board has no
  // clickable object for a player, so the overlay lists every legal player
  // target as its own control instead of relying on the seat chrome alone.
  const legalPlayerTargets = useMemo(
    () => (selection?.current_legal_targets ?? []).flatMap((target) =>
      "Player" in target ? [target.Player] : [],
    ),
    [selection],
  );
  const sourceId = boardChoice?.sourceId ?? (
    waitingFor?.type === "TriggerTargetSelection"
      ? waitingFor.data.source_id
      : waitingFor?.type === "TargetSelection"
        ? waitingFor.data.pending_cast?.object_id
        : waitingFor?.type === "ExploreChoice"
          ? waitingFor.data.source_id
        : waitingFor?.type === "PopulateChoice"
          ? waitingFor.data.source_id
        : waitingFor?.type === "ReturnAsAuraTarget"
          ? waitingFor.data.source_id
        : undefined
  );
  const sourceName = sourceId != null ? objects?.[sourceId]?.name : undefined;

  const targetPrompt = buildTargetPrompt({
    waitingFor: isTargetSelection ? waitingFor : null,
    targetKind,
    activeSlot,
    targetSlots,
    selection,
    t,
  });

  // CR 700.2 / CR 601.2b: for a modal spell or ability the engine attaches a
  // per-slot mode label, so the player knows which chosen mode the current
  // target belongs to. It qualifies the instruction from the caption line
  // rather than sharing the instruction's line — see the bar's comment.
  // A slot with no legal targets is skipped (CR 700.2c): the engine surfaces no
  // target for it, so the instruction is a status message and not a prompt
  // there, and there is nothing for the mode to qualify.
  const activeModeLabel =
    isTargetSelection && selection && activeSlot != null && activeSlot.legal_targets.length > 0
      ? waitingFor.data.mode_labels?.[selection.current_slot] ?? undefined
      : undefined;

  // CR 601.2d + CR 603.3d: both spell target selection and triggered target
  // selection can carry several slots — Inferno Titan's "divided as you choose
  // among one, two, or three targets" surfaces three. Issue #3681 is what this
  // exists for: a prompt naming only the kind let players commit one target
  // and stop, not knowing more were required. It qualifies from the caption
  // line rather than the instruction because appended to the longest localized
  // nouns it forced a second line at phone widths — see the bar's comment.
  const slotProgress = selection && targetSlots.length > 1
    ? t("targeting.slotProgress", {
        current: Math.min(selection.current_slot + 1, targetSlots.length),
        total: targetSlots.length,
      })
    : undefined;

  const triggerDescription = waitingFor?.type === "TriggerTargetSelection" && waitingFor.data.description
    ? renderDescription(waitingFor.data.description, sourceName ?? "this")
    : undefined;
  const triggerDamageAmount = waitingFor?.type === "TriggerTargetSelection"
    ? triggerDamageAmountForPrompt(waitingFor.data.trigger_event, waitingFor.data.trigger_events)
    : null;
  const spellTargetDescription = waitingFor?.type === "TargetSelection" && waitingFor.data.pending_cast.ability.description
    ? renderDescription(waitingFor.data.pending_cast.ability.description, sourceName ?? "this")
    : undefined;
  const enginePrompt = triggerDescription ?? spellTargetDescription;
  const overlayPrompt = isCopyTargetChoice
    ? t("targeting.choosePermanentToCopy")
    : isCopyRetarget
      ? (() => {
          const slots = waitingFor.data.target_slots;
          const hasCurrent = slots.every((slot) => slot.current != null);
          return slots.length > 1
            ? (hasCurrent
                ? t("targeting.retargetCopySlot", { current: Math.min(currentTargetSlot + 1, slots.length), total: slots.length })
                : t("targeting.chooseTargetForCopySlot", { current: Math.min(currentTargetSlot + 1, slots.length), total: slots.length }))
            : hasCurrent ? t("targeting.chooseNewTargetForCopy") : t("targeting.chooseTargetForCopy");
        })()
      : isExploreChoice
        ? t("targeting.chooseCreatureToExplore")
        : isPopulateChoice
          ? t("targeting.chooseCreatureTokenToPopulate")
          : isReturnAsAuraTarget
            ? t("targeting.chooseReturnAsAuraTarget")
            : isRetargetChoice
              ? (retargetSpellName
                  ? t("targeting.chooseNewTargetForSpell", { spell: retargetSpellName })
                  : t("targeting.chooseNewTarget"))
              : boardChoice
                ? boardChoicePrompt(boardChoice, selectedBoardChoiceIds, objects, t)
                : isTapCreatureChoice
                  ? t("targeting.tapUntappedCreatures", { count: waitingFor.data.count })
                  : targetPrompt ?? (
                    targetSlots.length > 1
                      ? t("targeting.chooseTargetOf", { current: Math.min(currentTargetSlot + 1, targetSlots.length), total: targetSlots.length })
                      : t("targeting.chooseTarget")
                  );

  // The engine description is free-form text of unbounded length. Collapsed it
  // is the tail of a single caption line, so the line's ellipsis is what bounds
  // it; expanded it opens a panel below the bar that scrolls inside its own
  // max-height rather than growing without bound. Resetting on a new
  // description keeps a prompt the player never asked to expand from opening
  // expanded.
  const [descriptionExpanded, setDescriptionExpanded] = useState(false);
  useEffect(() => {
    setDescriptionExpanded(false);
  }, [enginePrompt]);
  const descriptionToggleAction = descriptionExpanded
    ? t("targeting.collapseDescription")
    : t("targeting.expandDescription");

  const handleCancel = useCallback(() => {
    dispatch({ type: "CancelCast" });
  }, [dispatch]);

  const handlePlayerTarget = useCallback(
    (targetPlayerId: number) => {
      dispatch({ type: "ChooseTarget", data: { target: { Player: targetPlayerId } } });
    },
    [dispatch],
  );

  const handleSkip = useCallback(() => {
    dispatch({ type: "ChooseTarget", data: { target: null } });
  }, [dispatch]);

  const handleConfirmTap = useCallback(() => {
    dispatch({ type: "SelectCards", data: { cards: selectedCardIds } });
  }, [dispatch, selectedCardIds]);

  const handleConfirmBoardChoice = useCallback(() => {
    if (!boardChoice) return;
    dispatch(buildBoardChoiceAction(boardChoice, selectedBoardChoiceIds));
  }, [boardChoice, dispatch, selectedBoardChoiceIds]);

  const handleSkipBoardChoice = useCallback(() => {
    if (!boardChoice?.skipAction) return;
    dispatch(boardChoice.skipAction);
  }, [boardChoice, dispatch]);

  const handleCancelBoardChoice = useCallback(() => {
    if (!boardChoice?.cancelAction) return;
    dispatch(boardChoice.cancelAction);
  }, [boardChoice, dispatch]);

  useEffect(() => {
    if (!isBoardChoice) {
      clearSelectedCards();
      return;
    }
    clearSelectedCards();
    return () => clearSelectedCards();
  }, [clearSelectedCards, isBoardChoice, waitingFor]);

  if (!isTargetSelection && !isCopyTargetChoice && !isCopyRetarget && !isExploreChoice && !isPopulateChoice && !isReturnAsAuraTarget && !isRetargetChoice && !isTapCreatureChoice && !isBoardChoice) return null;

  // Only show targeting UI for the human player
  if (!canActForWaitingState) return null;

  // Everything that qualifies the instruction without being the instruction.
  // These share one caption line instead of each taking a row of their own —
  // see the bar's comment. The line truncates from the tail at narrow widths,
  // so the order is what survives first. The slot progress leads, and first is
  // the one position that never elides: it is the shortest entry, the only one
  // that is never recoverable from anything else on screen, and the only one
  // whose absence is a shipped regression (#3681). Then the rest of the short,
  // bounded entries: the source names the thing asking, the damage amount is
  // the stake of the choice, the chooser names who announces the slot. The two
  // entries of unbounded engine prose come last, mode label before description
  // — the description is the whole ability text and the disclosure beside it
  // opens that in full anyway, whereas nothing else on screen restates which
  // mode the current slot belongs to.
  const promptMeta: { key: string; node: ReactNode }[] = [];
  if (slotProgress) {
    promptMeta.push({
      key: "slot",
      // The instruction's own hue, one step lighter so 12px holds the weight
      // 18px holds at cyan-400: this is the instruction continuing in a quieter
      // voice, not another piece of ambient metadata.
      node: <span className="font-semibold text-cyan-300">{slotProgress}</span>,
    });
  }
  if (sourceName) {
    promptMeta.push({
      key: "source",
      node: <span className="font-medium text-amber-300">{sourceName}</span>,
    });
  }
  if (triggerDamageAmount != null) {
    promptMeta.push({
      key: "damage",
      node: (
        <span className="font-semibold text-red-300">
          {t("targeting.triggerDamageAmount", { amount: triggerDamageAmount })}
        </span>
      ),
    });
  }
  if (isOpponentChosenSlot) {
    promptMeta.push({
      key: "chooser",
      node: <span className="font-medium text-amber-300">{t("targeting.opponentChoice")}</span>,
    });
  }
  if (activeModeLabel) {
    promptMeta.push({
      key: "mode",
      node: (
        <RichLabel
          text={renderDescription(activeModeLabel, sourceName ?? "this")}
          size="xs"
          className="text-gray-300"
        />
      ),
    });
  }
  if (enginePrompt) {
    promptMeta.push({
      key: "description",
      node: <RichLabel text={enginePrompt} size="xs" className="text-gray-400" />,
    });
  }

  return (
    <AnimatePresence>
      <motion.div
        className={`pointer-events-none fixed inset-0 ${GAME_Z_LAYER.dialogHost}`}
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: 0.2 }}
      >
        {/* Semi-transparent overlay (click-through so board cards remain clickable) */}
        <div className="absolute inset-0 bg-black/30" />

        {/* The instruction bar. Pinned to the top so it overlaps only the
            opponent's face-down hand (low-value space).

            One surface, two lines: the instruction, and one caption line
            holding every qualifier (slot progress, source name, the trigger's
            damage amount, "opponent's choice", the active mode label, the
            engine description). Those used to be separate pills stacked under the
            instruction, and the stack is what grew tall enough to cover the
            opponent-HUD rail and hide a seat: five rows summed past the rail
            even though every row was short. A sum of rows cannot be bounded
            without deleting a row, so they share a row instead.

            Collapsed in the focused layout, a ONE-line instruction puts the
            block bottom at 58.75px — `top` 4 + `py-1.5` 12 + one 24.75px
            instruction line + `gap-0.5` 2 + a 16px caption line — or 59.25px
            when a mana symbol lands on the caption line, since a 14px inline
            image sat on `align-[-0.125em]` grows that line box to 16.5px. The
            clamp still allows 83.50/84.00px when the instruction takes two
            lines. One line is therefore the whole geometry, and it is why BOTH
            the mode label and the slot counter qualify from the caption line
            instead of joining the instruction. The mode label was the larger
            cause: mode labels are full engine sentences, so while one shared
            this line every modal prompt measured 83.50px at every width, in
            every locale. The counter was the smaller one, and English cannot
            see it — measured with each locale's real strings, the longest
            `targeting.one.*` phrase plus the counter wraps at 390px and 360px
            in de, es and pt and fits at both without it, while en stays on one
            line at every width. Note the second line is worth removing on a
            phone even where it collides with nothing: 84px instead of 59px eats
            a quarter more of a small screen and pushes the board down, which is
            the thing the original report was actually about.

            What is left is the phrase alone, and it measures ONE line for all
            seven locales at every viewport 844px wide and up. Below that it
            depends on the frame. Multi-slot (`targeting.one.*`): one line
            at 390 and 360 in all seven, wrapping only at 320 (de, es, pt).
            Single-slot optional (`targeting.upToOne.*`) is the wider case and
            is NOT improved by any of this, because it never carried a counter
            to begin with — it requires exactly one slot and the counter
            requires more than one, so the two can never co-occur, and reading
            one as evidence about the other is how this comment previously came
            to claim the counter was not what decides the wrap. Measured, it
            wraps at 390px in es, at 360px in de/es/pt, and at 320px in every
            locale but en and it. A wrapped instruction is 83.50/84.00px again.

            THOSE WIDTHS WERE MEASURED AGAINST THE PRE-WHOLE-PHRASE STRINGS,
            when an article was interpolated onto a bare noun. Authoring each
            phrase whole changed the values, so say plainly which figures bind.
            Longest rendered phrase per locale, `one` / `upToOne`, before ->
            after: en 29/37 and fr 37/45 and it 38/45 UNCHANGED; de 45->44 and
            52->51; es 47->44 and 53->50; pt 51->43 and 55->47; pl 33->34 and
            44->49. For six locales the string is identical or SHORTER. That is
            only a statement about DIRECTION — character length is not pixel
            width, so a shorter string can still be the wider one and none of
            the widths above survives as a bound on that basis. Polish is the one
            exception, in BOTH frames: it grew, so no figure above is a bound
            for pl and only a browser re-measure can replace them. There is no
            automated check behind any of this; jsdom has no layout engine.

            The split layout offsets `--game-targeting-prompt-top` to
            4.25rem/4.75rem, so these are the focused layout's figures.

            Bounded: the instruction clamps to two lines, the caption line is
            one line with an ellipsis, and the expanded description panel is
            `max-h-24` with its own scroll.

            NOT bounded: the bar's clearance over the opponent-HUD rail. No
            literal can be written for the rail's position, because
            `gridBands.top.pct` and `.pxCap` are user-editable preferences
            (`preferencesStore.defaultFlexLayout`, and the shipped presets
            already differ — 12%/100px vs 10%/80px), and the rail itself is a
            draggable widget with a persisted per-table-size offset below that
            band. Measured rail tops, default band / layout2 band: 1024x768
            92.16 / 76.80, 1280x720 86.39 / 72.00, 1440x900 and 1920x1080 and
            390x844 100.00 / 80.00, 1024x600 72.00 / 60.00, 844x499 59.88 /
            49.89 — `useResolvedGridRows` → `band()` drops `pxCap` entirely
            below 500px of viewport height (`minmax(0,${pct}%)`), which is what
            makes that last pair so short. A 59.25px block clears all of them
            by 12.75px or more EXCEPT three cells, none of which this bar can
            fix by getting shorter:

              - layout2 at 844x499 (49.89): overlaps by 9.36px. Unreachable —
                59.25px IS the floor for a two-row bar at this type size.
              - layout2 at 1024x600 (60.00): +0.75px, and the default band at
                844x499 (59.88): +0.63px. Do NOT read these as clearances. A
                sub-2px margin is inside the variation of font metrics across
                platforms and device pixel ratios, so the honest statement is
                that the bar does not reliably clear there.
              - phone portrait under layout2 (80.00) whenever the instruction
                wraps to two lines: 84.00px overlaps by 4.00px.

            All three are tracked as issue #7699. Keeping the collapsed
            instruction to ONE line is what keeps it clear where it clears, not
            a number. */}
        <div
          className="absolute left-0 right-0 flex flex-col items-center gap-1 px-2"
          style={{ top: "var(--game-targeting-prompt-top, 0.25rem)" }}
        >
          {/* `min(48rem, 100%)` rather than a plain `max-w-3xl`: the caption
              line does not wrap, so the bar's min-content width runs past any
              width where 48rem does not fit, and a px-only cap is then the
              size the bar settles on — it overflows its centred column instead
              of shrinking. The `100%` term is what keeps it on screen; the
              `48rem` term is the reading width. Measured at a 500px viewport:
              768px wide starting at x = -141 without the `100%` term. */}
          <div className="flex max-w-[min(48rem,100%)] flex-col items-center gap-0.5 rounded-lg bg-gray-900/90 px-4 py-1.5 shadow-lg ring-1 ring-white/10">
            {/* `leading-snug` rather than the `text-lg` default: 18px semibold
                set at 1.556 gives a 28px line box, and 1.375 gives 24.75px.
                That 3.25px is spent line spacing, not type size — the size
                that makes the instruction the loudest thing here is
                unchanged — and it is the last of it: this is the floor a label
                this size reads at. */}
            <div className="text-center text-lg font-semibold text-cyan-400 leading-snug line-clamp-2">
              <RichLabel text={overlayPrompt} />
            </div>
            {promptMeta.length > 0 && (
              <div className="flex w-full items-center justify-center gap-x-2 text-xs">
                <div className="min-w-0 truncate">
                  {promptMeta.map(({ key, node }, index) => (
                    <Fragment key={key}>
                      {index > 0 && <span aria-hidden="true" className="px-1 font-normal text-gray-500">·</span>}
                      {node}
                    </Fragment>
                  ))}
                </div>
                {enginePrompt && (
                  // The visible label names the action, and `aria-label`
                  // repeats it with the description appended: without the
                  // explicit name a screen reader would hear only "show the
                  // full description" and never the text it reveals, and the
                  // visible words stay a prefix of the accessible name so
                  // speech input can still address the control. `aria-controls`
                  // is set only while expanded, because the panel it names does
                  // not exist in the collapsed state and a dangling IDREF is
                  // worse than none — `aria-expanded` alone is correct there.
                  <button
                    type="button"
                    aria-expanded={descriptionExpanded}
                    aria-controls={descriptionExpanded ? DESCRIPTION_PANEL_ID : undefined}
                    aria-label={t("targeting.descriptionDisclosure", {
                      action: descriptionToggleAction,
                      description: flattenRichLabel(enginePrompt),
                    })}
                    onClick={() => setDescriptionExpanded((expanded) => !expanded)}
                    className="pointer-events-auto flex shrink-0 items-center gap-1 whitespace-nowrap rounded-sm px-1 text-gray-300 underline-offset-2 transition hover:text-gray-100 hover:underline"
                  >
                    {/* Below `sm` the words would cost a third of the caption
                        line. The line is `truncate`, so it elides from the
                        tail, and the description sits last in `promptMeta` —
                        so what these words push off the end is the description
                        itself, the one entry this very control opens in full.
                        Spending that width to label a chevron that already
                        reads as a disclosure is the worse trade. `aria-label`
                        carries the action either way, so the icon-only state
                        is named for assistive tech. */}
                    <span className="hidden sm:inline">{descriptionToggleAction}</span>
                    <svg
                      aria-hidden="true"
                      viewBox="0 0 12 12"
                      className={`h-3 w-3 transition-transform duration-200 ${descriptionExpanded ? "rotate-180" : ""}`}
                    >
                      <path
                        d="M2.5 4.5 6 8l3.5-3.5"
                        fill="none"
                        stroke="currentColor"
                        strokeWidth="1.5"
                        strokeLinecap="round"
                        strokeLinejoin="round"
                      />
                    </svg>
                  </button>
                )}
              </div>
            )}
          </div>
          {enginePrompt && descriptionExpanded && (
            // Its own scroll container, and the only part of the block besides
            // the disclosure that takes pointer events, so the scrollbar is
            // operable without the block eating board clicks around it.
            <div
              id={DESCRIPTION_PANEL_ID}
              className="pointer-events-auto max-h-24 max-w-md overflow-y-auto rounded-md bg-gray-800/90 px-4 py-1 text-center text-xs text-gray-300 shadow"
            >
              <RichLabel text={enginePrompt} size="xs" />
            </div>
          )}
        </div>

        {/* Player targets used to be committed ONLY through the PlayerHud /
            OpponentHud seat glow. That left a player asked for a player target
            with nothing to click inside the overlay and no statement of where
            to click, so the overlay now commits them directly (the seat chrome
            stays as the second, equivalent path). Seat colour is the anchor
            that ties each control to its seat.

            Pointer events sit on each control, never on this container: it
            spans `left-0 right-0`, and with `flex-wrap` a 4-player "any
            target" prompt (four `Choose:` controls plus Cancel) wraps to
            several rows at narrow widths, so an interactive container would
            block board clicks across a strip that grows with the number of
            seats. The top block does the same. */}
        <div className="absolute bottom-6 left-0 right-0 flex flex-wrap justify-center gap-4">
          {legalPlayerTargets.map((targetPlayerId) => {
            const seatColor = getSeatColor(targetPlayerId, seatOrder);
            return (
              <button
                key={targetPlayerId}
                onClick={() => handlePlayerTarget(targetPlayerId)}
                style={{ borderColor: seatColor, color: seatColor }}
                className="pointer-events-auto rounded-lg border bg-gray-900/90 px-6 py-2 font-semibold shadow-lg transition hover:bg-gray-800"
              >
                {t("targeting.choosePlayerTarget", {
                  name: targetPlayerId === localPlayerId
                    ? t("player.you")
                    : getOpponentDisplayName(targetPlayerId),
                })}
              </button>
            );
          })}
          {(waitingFor?.type === "TargetSelection" ||
            (!boardChoice &&
              waitingFor?.type === "PayCost" &&
              waitingFor.data.kind.type === "TapCreatures" &&
              waitingFor.data.resume.type === "Spell")) && (
            <button
              onClick={handleCancel}
              className="pointer-events-auto rounded-lg bg-gray-700 px-6 py-2 font-semibold text-gray-200 shadow-lg transition hover:bg-gray-600"
            >
              {t("common:actions.cancel")}
            </button>
          )}
          {!boardChoice && isTapCreatureChoice && (
            <button
              onClick={handleConfirmTap}
              // The engine supplies the legal payment range; an X-style tap cost has
              // min_count < count, so any in-range selection confirms.
              disabled={
                selectedCardIds.length < waitingFor.data.min_count ||
                selectedCardIds.length > waitingFor.data.count
              }
              className="pointer-events-auto rounded-lg bg-emerald-700 px-6 py-2 font-semibold text-white shadow-lg transition hover:bg-emerald-600 disabled:cursor-not-allowed disabled:bg-slate-700 disabled:text-white/50"
            >
              {t("targeting.confirmTap", { selected: selectedCardIds.length, count: waitingFor.data.count })}
            </button>
          )}
          {boardChoice?.cancelAction && (
            <button
              onClick={handleCancelBoardChoice}
              className="pointer-events-auto rounded-lg bg-gray-700 px-6 py-2 font-semibold text-gray-200 shadow-lg transition hover:bg-gray-600"
            >
              {t("common:actions.cancel")}
            </button>
          )}
          {boardChoice && !isBoardChoiceImmediate(boardChoice) && (
            <button
              onClick={handleConfirmBoardChoice}
              disabled={!canConfirmBoardChoice(boardChoice, selectedBoardChoiceIds, objects)}
              className={`pointer-events-auto ${boardChoiceConfirmClass(boardChoice)} rounded-lg px-6 py-2 font-semibold text-white shadow-lg transition disabled:cursor-not-allowed disabled:bg-slate-700 disabled:text-white/50`}
            >
              {boardChoiceConfirmLabel(boardChoice, selectedBoardChoiceIds, objects, t)}
            </button>
          )}
          {boardChoice?.skipAction && (
            <button
              onClick={handleSkipBoardChoice}
              className="pointer-events-auto rounded-lg bg-amber-700 px-6 py-2 font-semibold text-white shadow-lg transition hover:bg-amber-600"
            >
              {boardChoiceSkipLabel(boardChoice, t)}
            </button>
          )}
          {canKeepCurrentTargets && (
            <button
              onClick={() =>
                dispatch({
                  type: "KeepAllCopyTargets",
                })
              }
              className="pointer-events-auto rounded-lg bg-emerald-700 px-6 py-2 font-semibold text-white shadow-lg transition hover:bg-emerald-600"
            >
              {t("targeting.keepCurrentTargets")}
            </button>
          )}
          {isOptionalCurrentSlot && (
            <button
              onClick={handleSkip}
              className="pointer-events-auto rounded-lg bg-amber-700 px-6 py-2 font-semibold text-white shadow-lg transition hover:bg-amber-600"
            >
              {t("targeting.skip")}
            </button>
          )}
        </div>
      </motion.div>
    </AnimatePresence>
  );
}

function triggerDamageAmountForPrompt(
  triggerEvent: GameEvent | undefined,
  triggerEvents: GameEvent[] | undefined,
): number | null {
  const event = triggerEvent ?? (triggerEvents?.length === 1 ? triggerEvents[0] : undefined);
  if (!event) return null;

  switch (event.type) {
    case "DamageDealt":
      return event.data.amount;
    case "CombatDamageDealtToPlayer":
      return event.data.total_damage;
    default:
      return null;
  }
}

type TargetingPromptParams = {
  waitingFor: {
    type: "TargetSelection" | "TriggerTargetSelection" | "ExploreChoice" | "CopyTargetChoice" | "PayCost";
    data: {
      target_slots?: { legal_targets: { Object?: number; Player?: number }[]; optional?: boolean }[];
      mode_labels?: (string | null)[];
      selection?: { current_slot: number };
      player?: number;
    };
  } | null;
  targetKind: TargetChoiceKind | undefined;
  activeSlot: { legal_targets: { Object?: number; Player?: number }[]; optional?: boolean } | undefined;
  targetSlots: { legal_targets: { Object?: number; Player?: number }[]; optional?: boolean }[];
  selection: { current_slot: number } | null;
  t: TFunction<"game">;
};

function buildTargetPrompt({
  waitingFor,
  targetKind,
  activeSlot,
  targetSlots,
  selection,
  t,
}: TargetingPromptParams): string | null {
  if (!waitingFor) return null;
  if (waitingFor.type !== "TargetSelection" && waitingFor.type !== "TriggerTargetSelection") return null;
  if (!selection) return null;

  if (!activeSlot) return null;
  if (activeSlot.legal_targets.length === 0) {
    return t("targeting.noLegalTargets");
  }

  // The engine classifies the offer (CR 115.1); absent means no live
  // announcement to name. Fall back to the generic caption rather than
  // re-inferring — re-inference is the defect #7692 removed.
  if (!targetKind) return null;
  const frame: TargetFrame = selection && targetSlots.length === 1 && activeSlot.optional ? "upToOne" : "one";

  // The phrase alone: it is the only part that says WHAT the player has to pick,
  // and it is the whole of this line. Nothing else joins it — not the mode
  // label, not the slot counter — because both pushed the longest localized
  // phrases onto a second line, and a second line is both what put the block
  // over the opponent-HUD rail and what makes it eat a quarter more of a phone
  // screen. Both qualify from the caption line instead. The bar's comment
  // carries the measurements.
  return targetPhrase(targetKind, frame, t);
}

/**
 * CR 115.1 + CR 115.4: render the phrase the ENGINE classified, in the frame
 * the slot's optionality selects. This function inspects no game object and
 * reasons about no grammar — it maps a discriminant and a frame to an i18n key,
 * and the catalog holds the article, the gender and the case. All
 * classification lives in `engine::game::derived_views::target_choice_kind`.
 */
function targetPhrase(kind: TargetChoiceKind, frame: TargetFrame, t: TFunction<"game">): string {
  const phrase = (slug: TargetNounSlug): string => {
    const key: TargetPhraseKey = `targeting.${frame}.${slug}`;
    return t(key);
  };

  switch (kind.type) {
    case "Players":
      return phrase("player");
    case "Objects":
      return phrase(TARGET_NOUN_SLUG[kind.data.category]);
    // ONE `orPlayer` key per frame, not one per noun: the second conjunct
    // depends only on "player"'s gender and the case the frame governs, and
    // both are fixed once the frame is. i18next interpolates; nothing here
    // concatenates.
    //
    // Polish gets away with a single "{{noun}} lub gracza" across BOTH frames
    // only because `gracz` is SYNCRETIC — its genitive and its animate
    // accusative are both `gracza`. One string is carrying two cases by
    // coincidence, and it breaks the moment the second conjunct is any noun
    // but `gracz`.
    //
    // The `upToOne` x `orPlayer` cell — 14 authored strings — is exercised by
    // no test because no printed card appears to reach it: searching the
    // `oracle_text` of all 35,798 cards in data/card-data.json case-folded,
    // /up to one target (\w+ ){0,3}or player/ matches 0 cards and /up to one
    // any target/ matches 0, against live positive controls of 916 cards for
    // /any target/ and 651 for /up to one target/ in that same search — which
    // makes the gap a corpus observation about today's printings rather than
    // an oversight, and NOT a guarantee that the cell is unreachable.
    case "ObjectsAndPlayers": {
      const key: TargetOrPlayerKey = `targeting.${frame}.orPlayer`;
      return t(key, {
        noun: phrase(TARGET_NOUN_SLUG[kind.data.category]),
      });
    }
  }
}

function boardChoicePrompt(
  choice: BoardChoiceView,
  selectedIds: number[],
  objects: Record<number, GameObject> | undefined,
  t: TFunction<"game">,
): string {
  const action = t(`boardChoice.actions.${choice.intent}`);
  switch (choice.selection.type) {
    case "single":
      return t("boardChoice.prompt.single", { action });
    case "exactCount":
      return t("boardChoice.prompt.exactCount", {
        action,
        count: choice.selection.count,
      });
    case "rangeCount":
      return choice.selection.min > 0
        ? t("boardChoice.prompt.rangeCount", {
            action,
            min: choice.selection.min,
            count: choice.selection.max,
          })
        : t("boardChoice.prompt.upToCount", {
            action,
            count: choice.selection.max,
          });
    case "totalPowerAtLeast":
      return t("boardChoice.prompt.totalPower", {
        action,
        selected: boardChoiceSelectedPower(choice, selectedIds, objects),
        required: choice.selection.power,
      });
    case "totalPowerAtMost":
      return t("boardChoice.prompt.totalPowerAtMost", {
        action,
        selected: boardChoiceSelectedPower(choice, selectedIds, objects),
        max: choice.selection.power,
      });
  }
}

function boardChoiceConfirmLabel(
  choice: BoardChoiceView,
  selectedIds: number[],
  objects: Record<number, GameObject> | undefined,
  t: TFunction<"game">,
): string {
  switch (choice.selection.type) {
    case "single":
      return t("boardChoice.confirm");
    case "exactCount":
      if (choice.intent === "tap") {
        return t("targeting.confirmTap", {
          selected: selectedIds.length,
          count: choice.selection.count,
        });
      }
      if (choice.intent === "sacrifice") {
        return t("targeting.confirmSacrifice", {
          selected: selectedIds.length,
          count: choice.selection.count,
        });
      }
      return t("boardChoice.confirmCount", {
        selected: selectedIds.length,
        count: choice.selection.count,
      });
    case "rangeCount":
      if (selectedIds.length === 0 && choice.selection.min === 0) {
        return t("boardChoice.skip");
      }
      if (choice.intent === "sacrifice") {
        return t("targeting.confirmSacrifice", {
          selected: selectedIds.length,
          count: choice.selection.max,
        });
      }
      return t("boardChoice.confirmCount", {
        selected: selectedIds.length,
        count: choice.selection.max,
      });
    case "totalPowerAtLeast":
      return t("boardChoice.confirmPower", {
        selected: boardChoiceSelectedPower(choice, selectedIds, objects),
        required: choice.selection.power,
      });
    case "totalPowerAtMost":
      return t("boardChoice.confirmPowerAtMost", {
        selected: boardChoiceSelectedPower(choice, selectedIds, objects),
        max: choice.selection.power,
      });
  }
}

function boardChoiceConfirmClass(choice: BoardChoiceView): string {
  switch (choice.intent) {
    case "sacrifice":
      return "bg-red-700 hover:bg-red-600";
    case "tap":
      return "bg-emerald-700 hover:bg-emerald-600";
    case "untap":
      return "bg-emerald-700 hover:bg-emerald-600";
    case "blight":
      return "bg-purple-700 hover:bg-purple-600";
    case "ringBearer":
      return "bg-amber-700 hover:bg-amber-600";
    case "return":
    case "exile":
    case "crew":
    case "saddle":
    case "station":
    case "keep":
      return "bg-sky-700 hover:bg-sky-600";
  }
}

function boardChoiceSkipLabel(choice: BoardChoiceView, t: TFunction<"game">): string {
  switch (choice.skipLabel) {
    case "keepTapped":
      return t("gamePage.untap.keepTapped");
    case undefined:
      return t("boardChoice.skip");
  }
}
