import { type CSSProperties, useMemo } from "react";
import { useTranslation } from "react-i18next";

import type { ObjectId } from "../../adapter/types.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { useCardHover } from "../../hooks/useCardHover.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { cardImageLookup } from "../../services/cardImageLookup.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { collectObjectActions } from "../../viewmodel/cardActionChoice.ts";
import { COUNTER_COLORS, formatCounterType } from "../../viewmodel/cardProps.ts";
import { CardImage } from "../card/CardImage.tsx";
import { CounterTooltip } from "../ui/CounterTooltip.tsx";

interface Props {
  objectId: ObjectId;
  /** Pixel width to render. Height derives from the standard 1.4 card aspect
   *  ratio. Caller controls so the dialog can pick a size that suits its
   *  available width budget without DialogAttachmentCard guessing. */
  widthPx: number;
  /** Close the enclosing AttachmentsDialog. Called after any interaction that
   *  advances engine state to a prompt the player resolves on the BOARD rather
   *  than in this dialog — target-select (the enchant/equip target is a
   *  battlefield object, not a dialog card) and activation (e.g. Equip
   *  transitions to `EquipTarget`, which needs the board visible). The
   *  multi-ability picker is the one exception: it floats above independently,
   *  so the dialog closes only after the picker itself dispatches.
   *
   *  Optional: the non-interactive `AurasHoverPreview` (a `pointer-events-none`
   *  glance panel) renders these cards purely for display, so no interaction —
   *  hence no dismiss — is ever possible there and it omits the callback. */
  onDismiss?: () => void;
}

/**
 * Full Scryfall card image rendered for the AttachmentsDialog (large enough
 * to read oracle text, mana cost, type line, P/T) with the two interaction
 * carve-outs that matter for player-attached Auras: click-to-target when the
 * engine is asking for an enchantment target, and click-to-activate when the
 * Aura has activated abilities. Counter badges overlay the top-right corner
 * for Auras that accumulate counters (rare but exists).
 *
 * Deliberately NOT using `<PermanentCard>`:
 *   - PermanentCard hardcodes `size="small"` for the image (battlefield-
 *     compact); the dialog needs Scryfall `"normal"` for readability.
 *   - PermanentCard layers a lot of battlefield-specific chrome (attachment
 *     peek-out, exile ghosts, glow rings for combat / mana-cost / undo-tap,
 *     P/T box, layoutId animation, tap rotation) — none of it is meaningful
 *     for an Aura sitting in a viewer modal.
 *   - PermanentCard reads `useBoardInteractionState`, a React context only
 *     populated under `<GameBoard>`. The dialog mounts under `<DialogHost>`
 *     where that context returns empty sets, so target/activatable signals
 *     would be blank anyway. Reading directly from gameStore.waitingFor /
 *     legalActionsByObject is the right source here.
 */
export function DialogAttachmentCard({ objectId, widthPx, onDismiss }: Props) {
  const { t } = useTranslation("game");
  const playerId = usePlayerId();
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);

  // Target eligibility: pulled directly from the engine's WaitingFor state.
  // Mirrors the same predicate `PlayerHud` uses for the player-as-target
  // case (PlayerHud.tsx isValidTarget calc), specialized to Object targets.
  const isValidTarget = useGameStore((s) => {
    const wf = s.waitingFor;
    if (
      wf?.type !== "TargetSelection"
      && wf?.type !== "TriggerTargetSelection"
    ) return false;
    if (wf.data.player !== playerId) return false;
    return (wf.data.selection?.current_legal_targets ?? []).some(
      (t) => "Object" in t && t.Object === objectId,
    );
  });

  // Activation: collect every legal action the engine has registered against
  // this Aura. Auras with activated abilities are rare (most are static or
  // triggered) but the engine surfaces them through the same per-object
  // legal-action map that PermanentCard consumes on the battlefield.
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
  const objectActions = useMemo(
    () => (legalActionsByObject ? collectObjectActions(legalActionsByObject, objectId) : []),
    [legalActionsByObject, objectId],
  );
  const isActivatable = objectActions.length > 0;

  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);

  const { handlers, firedRef } = useCardHover(objectId);

  if (!obj) return null;

  const lookup = cardImageLookup(obj);
  // Non-loyalty counters only — loyalty applies to planeswalkers, never Auras.
  const counters = Object.entries(obj.counters).filter(
    (entry): entry is [string, number] =>
      entry[0] !== "loyalty" && entry[1] != null && entry[1] > 0,
  );

  const sizeVars: CSSProperties = {
    "--card-w": `${widthPx}px`,
    "--card-h": `${Math.round(widthPx * 1.4)}px`,
  } as CSSProperties;

  const onClick = () => {
    if (firedRef.current) {
      firedRef.current = false;
      return;
    }
    if (isValidTarget) {
      dispatchAction({ type: "ChooseTarget", data: { target: { Object: objectId } } });
      // Auto-close on target select: the user's intent was decisive ("I
      // picked this target"). The engine moves to the next prompt; leaving
      // the dialog mounted would obscure that next prompt for no reason.
      onDismiss?.();
      return;
    }
    if (isActivatable) {
      if (objectActions.length === 1) {
        dispatchAction(objectActions[0]);
        // Close so the board is reachable for whatever the activation asks
        // next — e.g. Equip transitions to `EquipTarget`, whose creature
        // target is picked on the battlefield, not in this dialog.
        onDismiss?.();
      } else {
        // Multi-ability picker takes over. Hand off to it and close this
        // dialog: the picker floats independently (DialogHost z-ordering)
        // and its own dispatch will drive the next board prompt, so keeping
        // the attachments dialog mounted behind it only obscures the board.
        setPendingAbilityChoice({ objectId, actions: objectActions });
        onDismiss?.();
      }
    }
  };

  // Glow ring conveys actionability — same color vocabulary PermanentCard
  // uses (lime for valid target, cyan for activatable) so the dialog reads
  // visually consistent with the battlefield.
  const glowClass = isValidTarget
    ? "outline outline-2 outline-black/80 ring-4 ring-lime-300 shadow-[0_0_18px_6px_rgba(190,242,100,0.72),inset_0_0_18px_4px_rgba(190,242,100,0.22)]"
    : isActivatable
      ? "ring-2 ring-cyan-400 shadow-[0_0_14px_4px_rgba(34,211,238,0.55)]"
      : "";

  const interactive = isValidTarget || isActivatable;

  return (
    <div
      {...handlers}
      onClick={interactive ? onClick : undefined}
      role={interactive ? "button" : undefined}
      tabIndex={interactive ? 0 : undefined}
      onKeyDown={
        interactive
          ? (e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                onClick();
              }
            }
          : undefined
      }
      style={sizeVars}
      className={`relative inline-block rounded-lg ${glowClass} ${interactive ? "cursor-pointer" : "cursor-default"}`}
    >
      <CardImage
        cardName={lookup.name}
        faceIndex={lookup.faceIndex}
        oracleId={lookup.oracleId}
        faceName={lookup.faceName}
        size="normal"
      />
      {isValidTarget && (
        <div className="pointer-events-none absolute left-1 top-1 z-30 rounded bg-lime-300 px-1.5 py-0.5 text-[9px] font-black uppercase leading-none tracking-normal text-black ring-1 ring-black/70 shadow-[0_1px_4px_rgba(0,0,0,0.75)]">
          {t("attachments.target")}
        </div>
      )}
      {counters.length > 0 && (
        <div className="absolute right-1 top-1 z-[60] flex flex-col gap-0.5">
          {counters.map(([type, count]) => (
            <CounterTooltip key={type} type={type} count={count}>
              <span
                className={`rounded px-1 text-[10px] font-bold text-white ${COUNTER_COLORS[type] ?? "bg-purple-600"}`}
              >
                {formatCounterType(type)} x{count}
              </span>
            </CounterTooltip>
          ))}
        </div>
      )}
    </div>
  );
}
