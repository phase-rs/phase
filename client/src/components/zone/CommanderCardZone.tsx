import { useCallback, useMemo, useRef } from "react";
import { motion } from "framer-motion";
import type { PanInfo } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { GameObject, PlayerId } from "../../adapter/types.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { previewAutomaticManaPayment } from "../../game/manaPaymentPreview.ts";
import { useCardHover } from "../../hooks/useCardHover.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { getPlayerId } from "../../hooks/usePlayerId.ts";
import { useDragToCast } from "../../hooks/useDragToCast.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import {
  collectObjectActions,
  resolveSingleActionDispatch,
} from "../../viewmodel/cardActionChoice.ts";
import { CASTABLE_AFFORDANCE_ACTIVE } from "../../viewmodel/castableAffordance.ts";
import { commandZoneLeaders } from "../../viewmodel/commanderColumn.ts";
import { ManaCostPips } from "../mana/ManaCostPips.tsx";

interface CommanderCardZoneProps {
  playerId: PlayerId;
  /** Split multiplayer overview pane: the card is ~40px wide, so the centered
   *  "Commander" wordmark spans the whole card and hides the cost pips. Drop
   *  the wordmark (the amber frame + dock position + tooltip still mark the
   *  commander) and shrink the pips so the cost reads instead. */
  splitOverview?: boolean;
}

/**
 * Renders commander cards in the command zone as full card images in the
 * right-side zone rail. Shows castability glow when legal to cast and
 * displays effective cost (including commander tax).
 */
export function CommanderCardZone({ playerId, splitOverview = false }: CommanderCardZoneProps) {
  const gameState = useGameStore((s) => s.gameState);

  const commanders = useMemo(
    () => (gameState ? commandZoneLeaders(gameState, playerId) : []),
    [gameState, playerId],
  );

  if (commanders.length === 0) return null;

  // Lay the leaders out horizontally (commander(s) + any Oathbreaker signature
  // spell, and partner/background pairs) rather than stacking them. A vertical
  // stack doubles the command dock's height, and the middle row is
  // `items-stretch`, so that height propagates to the whole battlefield row and
  // breaks the layout globally. A row keeps the dock one card tall.
  return (
    <div className="flex flex-row items-end gap-1">
      {commanders.map((cmd) => (
        <CommanderCard key={cmd.id} commander={cmd} splitOverview={splitOverview} />
      ))}
    </div>
  );
}

function CommanderCard({
  commander,
  splitOverview,
}: {
  commander: GameObject;
  splitOverview: boolean;
}) {
  const { t } = useTranslation("game");
  const isSignatureSpell = commander.signature_spell != null;
  const isCompactHeight = useIsCompactHeight();
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
  const effectiveCost = useGameStore(
    (s) => s.spellCosts[String(commander.id)],
  );
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);
  const { src } = useCardImage(commander.name, { size: "normal" });
  const { handlers: hoverHandlers, firedRef } = useCardHover(commander.id);
  const tax = commander.commander_tax ?? 0;

  // Engine authority (GameAction::source_object): both CastSpell (cast from the
  // command zone) and ActivateNinjutsu (commander ninjutsu, CR 702.49d) anchor
  // to this commander's id, so the map lookup surfaces every action the engine
  // legally offers for it — no client-side legality inference.
  const commanderActions = useMemo(
    () => collectObjectActions(legalActionsByObject, commander.id),
    [legalActionsByObject, commander.id],
  );
  const castAction = useMemo(
    () => commanderActions.find((a) => a.type === "CastSpell") ?? null,
    [commanderActions],
  );
  const ninjutsuActions = useMemo(
    () => commanderActions.filter((a) => a.type === "ActivateNinjutsu"),
    [commanderActions],
  );

  const canCast = castAction !== null;
  const canNinjutsu = ninjutsuActions.length > 0;

  // CR 702.49d: commander ninjutsu returns an unblocked attacker and puts this
  // commander onto the battlefield tapped and attacking. The engine emits one
  // ActivateNinjutsu per returnable attacker; route through the shared dispatch
  // authority so a lone option fires immediately and multiple options surface
  // the choice modal — mirroring hand-zone ninjutsu (PlayerHand.playCard).
  const activateNinjutsu = () => {
    const auto = resolveSingleActionDispatch(ninjutsuActions, commander);
    if (auto) {
      dispatchAction(auto);
    } else {
      setPendingAbilityChoice({ objectId: commander.id, actions: ninjutsuActions });
    }
  };
  const displayCost = effectiveCost ?? commander.mana_cost;
  // canCast is engine-authoritative: the action is in legalActions only when
  // priority + mana + timing all permit the cast. Reuse it as the drag gate
  // rather than threading a separate hasPriority check through.
  const dragCast = useDragToCast({ castAction, hasPriority: canCast, useDistanceThreshold: true });
  const manaPaymentPreviewRequestId = useRef(0);
  const startManaPaymentPreview = useCallback(() => {
    const requestId = ++manaPaymentPreviewRequestId.current;
    if (!castAction) {
      useGameStore.getState().clearManaPaymentPreview();
      return;
    }

    void previewAutomaticManaPayment(castAction, getPlayerId())
      .then((sourceIds) => {
        const store = useGameStore.getState();
        if (manaPaymentPreviewRequestId.current !== requestId) return;
        if (sourceIds === null) {
          store.clearManaPaymentPreview();
        } else {
          store.setManaPaymentPreviewSourceIds(sourceIds);
        }
      })
      .catch(() => {
        if (manaPaymentPreviewRequestId.current === requestId) {
          useGameStore.getState().clearManaPaymentPreview();
        }
      });
  }, [castAction]);
  const stopManaPaymentPreview = useCallback(() => {
    manaPaymentPreviewRequestId.current += 1;
    useGameStore.getState().clearManaPaymentPreview();
  }, []);
  // Framer Motion does not suppress the synthetic click that follows a
  // drag gesture on a <motion.button>. Without this guard, a successful
  // drag-cast would immediately trigger the click handler and open the
  // inspector on top of the newly-cast spell. Set the flag when drag-cast
  // fires and read-reset it on the next click.
  const dragCastedRef = useRef(false);
  const onDragEnd = (event: MouseEvent | TouchEvent | PointerEvent, info: PanInfo) => {
    stopManaPaymentPreview();
    const fired = dragCast(event, info);
    if (fired) dragCastedRef.current = true;
  };

  return (
    <motion.button
      {...hoverHandlers}
      onClick={(e: React.MouseEvent) => {
        if (dragCastedRef.current) {
          dragCastedRef.current = false;
          return;
        }
        if (firedRef.current) return;
        if (useUiStore.getState().debugInteractionMode) {
          e.stopPropagation();
          useUiStore.getState().openDebugContextMenu({ objectId: commander.id, x: e.clientX, y: e.clientY });
          return;
        }
        // Commander ninjutsu is a click affordance (unlike drag-to-cast): a
        // legal ActivateNinjutsu takes precedence over inspecting the card.
        if (canNinjutsu) {
          activateNinjutsu();
          return;
        }
        inspectObject(commander.id);
      }}
      onDoubleClick={canCast ? () => dispatchAction(castAction) : undefined}
      drag={canCast || false}
      dragSnapToOrigin
      onDragStart={startManaPaymentPreview}
      onDragEnd={onDragEnd}
      whileDrag={{ cursor: "grabbing", scale: 1.04 }}
      className={`group relative ${
        canCast ? "cursor-grab" : canNinjutsu ? "cursor-pointer" : "cursor-default"
      }`}
      title={
        canCast
          ? isSignatureSpell
            ? tax > 0
              ? t("zone.castSignatureSpellTax", { name: commander.name, tax })
              : t("zone.castSignatureSpell", { name: commander.name })
            : tax > 0
              ? t("zone.castCommanderTax", { name: commander.name, tax })
              : t("zone.castCommander", { name: commander.name })
          : canNinjutsu
            ? t("zone.ninjutsuCommander", { name: commander.name })
            : isSignatureSpell
              ? tax > 0
                ? t("zone.signatureSpellTitleTax", { name: commander.name, tax })
                : t("zone.signatureSpellTitle", { name: commander.name })
              : tax > 0
                ? t("zone.commanderTitleTax", { name: commander.name, tax })
                : t("zone.commanderTitle", { name: commander.name })
      }
      style={{ width: "var(--card-w)", height: "var(--card-h)" }}
    >
      {/* Card image */}
      <div className="relative h-full w-full overflow-hidden rounded-lg border border-amber-400/60 shadow-md">
        {src ? (
          <img
            src={src}
            alt={commander.name}
            className="h-full w-full object-cover"
            draggable={false}
          />
        ) : (
          <div className="flex h-full w-full items-center justify-center bg-gray-700 text-[10px] text-gray-400">
            {commander.name}
          </div>
        )}

        {/* Translucent overlay — amber tint, lighter when actionable (castable
            or commander-ninjutsu available) */}
        <div
          className={`absolute inset-0 transition-colors ${
            canCast || canNinjutsu
              ? "bg-amber-600/20 group-hover:bg-amber-600/5"
              : "bg-gray-900/50"
          }`}
        />
      </div>

      {/* Commander badge — omitted in split panes where it would blanket the
          card and hide the cost pips. */}
      {!splitOverview && (
        <div className="absolute -top-1 left-1/2 z-10 -translate-x-1/2 whitespace-nowrap rounded-sm bg-amber-700 px-1.5 py-px text-[8px] font-bold text-amber-100 shadow">
          {isSignatureSpell ? t("zone.signatureSpell") : t("zone.commander")}
        </div>
      )}

      {/* Actionable glow ring — castable or commander-ninjutsu available */}
      {(canCast || canNinjutsu) && (
        <div className={`absolute inset-0 rounded-lg ${CASTABLE_AFFORDANCE_ACTIVE}`} />
      )}

      {/* Commander tax badge — nowrap: the absolute box is clamped to the
          card's width, so on narrow cards "Tax: +N" would otherwise break
          into two lines; centered overhang beats a wrapped pill. */}
      {tax > 0 && (
        <div
          className={`absolute -bottom-1 left-1/2 z-10 -translate-x-1/2 whitespace-nowrap rounded-sm bg-amber-900 py-px font-bold text-amber-200 shadow ${
            splitOverview ? "px-1 text-[7px]" : "px-1.5 text-[8px]"
          }`}
        >
          {t("zone.tax", { tax })}
        </div>
      )}

      {/* Effective mana cost (includes tax) */}
      {displayCost && (
        <ManaCostPips
          cost={displayCost}
          isReduced={false}
          size={splitOverview || isCompactHeight ? "2xs" : "xs"}
          className="absolute right-[4%] top-[2%]"
        />
      )}
    </motion.button>
  );
}
