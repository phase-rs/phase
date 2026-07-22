import { type CSSProperties, useCallback, useEffect, useMemo } from "react";
import { createPortal } from "react-dom";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { GameAction, ObjectId } from "../../adapter/types.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { cardImageLookup, tokenFiltersForObject } from "../../services/cardImageLookup.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { collectObjectActions } from "../../viewmodel/cardActionChoice.ts";
import {
  boardChoiceMaxSelection,
  buildBoardChoiceAction,
  canConfirmBoardChoice,
  getBoardChoiceView,
  isBoardChoiceImmediate,
} from "../../viewmodel/gameStateView.ts";
import { CardImage } from "../card/CardImage.tsx";
import { fanGeometry, spreadFactor } from "../card/fanGeometry.ts";

// Card sizing for the centered fan. Cards render at near card-preview size so
// oracle text / P-T are readable at rest (no separate preview needed), and scale
// down ONLY IF NEEDED to fit the viewport. The width is:
//
//   min( clamp(11rem, 26vh, 24rem) ,  ${WIDTH_BUDGET_VW}vw / spreadFactor(n) )
//        └────── desired size ──────┘  └──────── width-fit cap ────────────┘
//
//   • Desired size is height-driven (`26vh`; `* 1.4` is the standard card aspect)
//     so it's generous on a tall screen — but FLOORED at `11rem` so a SHORT screen
//     (landscape phone, where `26vh` collapses to ~100px) still gets a big card.
//     Ceiling keeps a 2-card fan from ballooning on an ultrawide monitor.
//   • Width-fit cap is an OUTER `min`, so it can pull the card BELOW the 11rem
//     floor when the whole overlapped fan (`cardW * spreadFactor`) would otherwise
//     overflow — i.e. a narrow PORTRAIT phone with several attachments. It never
//     binds on a roomy screen, so those sizes are unchanged. Budget is generous
//     (96vw) so the common 1–2 card case is never shrunk.
//
// The floor lives INSIDE the desired term (not as an outer clamp) precisely so it
// rescues short screens without re-inflating a correctly width-shrunk card.
const WIDTH_BUDGET_VW = 96;
function fanCardSizingStyle(cardCount: number): CSSProperties {
  const widthCapVw = (WIDTH_BUDGET_VW / spreadFactor(cardCount)).toFixed(1);
  const cardW = `min(clamp(11rem, 26vh, 24rem), ${widthCapVw}vw)`;
  return {
    "--fan-card-w": cardW,
    "--fan-card-h": `calc(${cardW} * 1.4)`,
  } as CSSProperties;
}

/**
 * Per-object selection state for one card in the fan, derived once by the
 * parent from the live prompt so each card knows exactly which engine action
 * (if any) its click should dispatch. Target > board-choice > activation is
 * the same precedence PermanentCard uses on the battlefield.
 */
interface CardChoice {
  isTarget: boolean;
  boardEligible: boolean;
  isSelected: boolean;
  activationActions: GameAction[];
}

/**
 * Centered spread of a host permanent plus every permanent attached to it
 * (Aura / Equipment / Fortification), fanned out at HAND size using the shared
 * compact `fanGeometry` profile — so it reads as a familiar held hand of large,
 * legible cards rather than a bespoke overlay. Solves the reachability
 * problem where an attached Equipment/Aura renders only as a narrow peek behind
 * its host: during a target or board-choice prompt the overlapping objects are
 * all legal picks (CR 301.5 / 303.4: an attached permanent is its own
 * independent object), and the fan lets the player choose which one without
 * hunting the peek.
 *
 * The fan NEVER invents a choice — each card lights up (cyan) and dispatches
 * only what the engine's live prompt actually offers for that object. Terminal
 * picks (a target, an immediate board-choice, an activation) close the fan;
 * a multi-select board-choice toggles and is finished via the Confirm button.
 * Direct clicking on the battlefield still works — this fan is an opt-in
 * convenience opened from the "⧉" badge, not a forced modal.
 */
export function AttachmentFan() {
  const { t } = useTranslation("game");
  const playerId = usePlayerId();
  const hostId = useUiStore((s) => s.attachmentFanHostId);
  const setAttachmentFanHost = useUiStore((s) => s.setAttachmentFanHost);
  const dismissPreview = useUiStore((s) => s.dismissPreview);
  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);
  const toggleSelectedCard = useUiStore((s) => s.toggleSelectedCard);
  const selectedCardIds = useUiStore((s) => s.selectedCardIds);

  const objects = useGameStore((s) => s.gameState?.objects);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);

  const host = hostId != null ? objects?.[hostId] : undefined;

  const cardIds = host ? [host.id, ...host.attachments] : [];

  const boardChoice = useMemo(() => {
    const choice = getBoardChoiceView(waitingFor, objects);
    return choice && choice.player === playerId ? choice : null;
  }, [waitingFor, objects, playerId]);

  // Engine's live target legal-set for the current prompt, as a plain id set.
  const targetIds = useMemo(() => {
    const set = new Set<ObjectId>();
    if (
      (waitingFor?.type === "TargetSelection" || waitingFor?.type === "TriggerTargetSelection")
      && waitingFor.data.player === playerId
    ) {
      for (const target of waitingFor.data.selection?.current_legal_targets ?? []) {
        if ("Object" in target) set.add(target.Object);
      }
    }
    if (waitingFor?.type === "EquipTarget" && waitingFor.data.player === playerId) {
      for (const id of waitingFor.data.valid_targets) set.add(id);
    }
    return set;
  }, [waitingFor, playerId]);

  const close = useCallback(() => {
    setAttachmentFanHost(null);
    // The fan is opened from a hovered card, so a card preview may still be up
    // (CardPreview `z-[100]`); tear it down so it doesn't linger over the board.
    dismissPreview();
  }, [setAttachmentFanHost, dismissPreview]);

  // Esc closes, mirroring every other dismissible overlay.
  useEffect(() => {
    if (hostId == null) return undefined;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [hostId, close]);

  const choiceFor = useCallback(
    (id: ObjectId): CardChoice => ({
      isTarget: targetIds.has(id),
      boardEligible: boardChoice?.objectIds.includes(id) ?? false,
      isSelected: selectedCardIds.includes(id),
      activationActions: legalActionsByObject ? collectObjectActions(legalActionsByObject, id) : [],
    }),
    [targetIds, boardChoice, selectedCardIds, legalActionsByObject],
  );

  const handlePick = useCallback(
    (id: ObjectId, choice: CardChoice) => {
      if (choice.isTarget) {
        dispatchAction({ type: "ChooseTarget", data: { target: { Object: id } } });
        close();
        return;
      }
      if (choice.boardEligible && boardChoice) {
        if (isBoardChoiceImmediate(boardChoice)) {
          dispatchAction(buildBoardChoiceAction(boardChoice, [id]));
          close();
          return;
        }
        // Multi-select: toggle within the engine's max, then Confirm finishes.
        const max = boardChoiceMaxSelection(boardChoice);
        const selectedForChoice = selectedCardIds.filter((s) => boardChoice.objectIds.includes(s));
        if (choice.isSelected || max == null || selectedForChoice.length < max) {
          toggleSelectedCard(id);
        }
        return;
      }
      if (choice.activationActions.length > 0) {
        if (choice.activationActions.length === 1) {
          dispatchAction(choice.activationActions[0]);
        } else {
          setPendingAbilityChoice({ objectId: id, actions: choice.activationActions });
        }
        close();
      }
    },
    [boardChoice, selectedCardIds, close, toggleSelectedCard, setPendingAbilityChoice],
  );

  const confirmSelection = useMemo(() => {
    if (!boardChoice || isBoardChoiceImmediate(boardChoice)) return null;
    const selectedForChoice = selectedCardIds.filter((s) => boardChoice.objectIds.includes(s));
    if (selectedForChoice.length === 0) return null;
    return {
      enabled: canConfirmBoardChoice(boardChoice, selectedForChoice, objects),
      selected: selectedForChoice,
      choice: boardChoice,
    };
  }, [boardChoice, selectedCardIds, objects]);

  if (hostId == null || !host || cardIds.length === 0) return null;

  // Shared compact whole-row fan — sized by the total card count so the host +
  // its attachments stay within the overlay's viewport budget.
  // The overlap basis is `--fan-card-w` (the fan's larger card width) so the
  // spread stays proportional to the bigger cards.
  const fan = fanGeometry(cardIds.length, "--fan-card-w");

  return createPortal(
    // Portaled above the card preview (`z-[100]`) so the fan and its cyan
    // affordances are never veiled. Backdrop click / Esc dismiss.
    <div
      className="fixed inset-0 z-[120] flex flex-col items-center justify-center bg-black/60 backdrop-blur-[2px]"
      onClick={close}
      data-attachment-fan
    >
      {/* `items-end` + `perspective` mirror the hand container so the cards fan
          up from a shared baseline with the same held-hand feel. The sizing style
          defines `--fan-card-w/h` — big by default, scaling down only if the fan
          would overflow a narrow screen (portrait mobile with several cards). */}
      <div
        className="flex items-end justify-center"
        style={{ perspective: "800px", ...fanCardSizingStyle(cardIds.length) }}
        onClick={(e) => e.stopPropagation()}
      >
        {cardIds.map((id, i) => (
          <FanCard
            key={id}
            objectId={id}
            choice={choiceFor(id)}
            marginLeft={i === 0 ? 0 : fan.overlap}
            rotation={fan.rotation(i)}
            arcOffset={fan.arc(i)}
            zIndex={i}
            onPick={handlePick}
          />
        ))}
      </div>

      {confirmSelection && (
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            dispatchAction(buildBoardChoiceAction(confirmSelection.choice, confirmSelection.selected));
            close();
          }}
          disabled={!confirmSelection.enabled}
          className="mt-8 rounded-full bg-cyan-500 px-5 py-2 text-sm font-bold text-cyan-950 shadow-[0_2px_10px_rgba(34,211,238,0.5)] transition hover:bg-cyan-400 disabled:cursor-not-allowed disabled:bg-slate-600 disabled:text-slate-300 disabled:shadow-none"
        >
          {t("permanent.fanConfirm", { count: confirmSelection.selected.length })}
        </button>
      )}
    </div>,
    document.body,
  );
}

function FanCard({
  objectId,
  choice,
  marginLeft,
  rotation,
  arcOffset,
  zIndex,
  onPick,
}: {
  objectId: ObjectId;
  choice: CardChoice;
  marginLeft: string | number;
  rotation: number;
  arcOffset: number;
  zIndex: number;
  onPick: (id: ObjectId, choice: CardChoice) => void;
}) {
  const { t } = useTranslation("game");
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);
  if (!obj) return null;

  const lookup = cardImageLookup(obj);
  const isToken = obj.display_source === "Token";
  const selectable = choice.isTarget || choice.boardEligible || choice.activationActions.length > 0;

  // The whole fan speaks one "pick me" color — cyan — so a spread of a host and
  // its attachments reads as a single chooser regardless of whether the engine
  // is asking for a target, a board choice, or an activation. Selected (a
  // toggled multi-select board choice) brightens and adds a check.
  const ring = choice.isSelected
    ? "ring-4 ring-cyan-300 shadow-[0_0_22px_7px_rgba(34,211,238,0.7),inset_0_0_18px_5px_rgba(34,211,238,0.35)]"
    : selectable
      ? "ring-2 ring-cyan-400 shadow-[0_0_16px_5px_rgba(34,211,238,0.55)]"
      : "";

  // Mirror the hand card's resting animation (arc + tilt) and hover lift so the
  // attachment fan feels identical to picking a card out of hand.
  return (
    <motion.div
      layout
      initial={{ opacity: 0, y: 40 }}
      animate={{ opacity: 1, y: 30 + arcOffset, rotate: rotation }}
      whileHover={{ y: arcOffset - 12, scale: 1.12, zIndex: 999 }}
      transition={{ duration: 0.2 }}
      onClick={(e) => {
        e.stopPropagation();
        if (selectable) onPick(objectId, choice);
      }}
      aria-label={obj.name}
      className={`relative leading-[0] select-none ${selectable ? "cursor-pointer" : "cursor-default"}`}
      style={{ marginLeft, zIndex }}
    >
      <div className={`relative overflow-hidden rounded-lg ${ring} ${selectable ? "" : "opacity-60"}`}>
        <CardImage
          cardName={lookup.name}
          faceIndex={lookup.faceIndex}
          oracleId={lookup.oracleId}
          faceName={lookup.faceName}
          size="large"
          isToken={isToken}
          tokenFilters={isToken ? tokenFiltersForObject(obj) : undefined}
          tokenImageRef={isToken ? obj.token_image_ref : undefined}
          oracleText={isToken ? obj.token_rules_text : undefined}
          faceDown={obj.face_down}
          className="!w-[var(--fan-card-w)] !h-[var(--fan-card-h)]"
        />
      </div>
      {choice.isSelected && (
        <span className="pointer-events-none absolute left-1 top-1 z-10 flex h-6 w-6 items-center justify-center rounded-full bg-cyan-400 text-sm font-black text-cyan-950 ring-2 ring-cyan-950/60 shadow">
          ✓
        </span>
      )}
      {selectable && !choice.isSelected && (
        <span className="pointer-events-none absolute left-1 top-1 z-10 rounded bg-cyan-400 px-1.5 py-0.5 text-[9px] font-black uppercase leading-none tracking-normal text-cyan-950 ring-1 ring-cyan-950/50 shadow">
          {t("permanent.fanPick")}
        </span>
      )}
    </motion.div>
  );
}
