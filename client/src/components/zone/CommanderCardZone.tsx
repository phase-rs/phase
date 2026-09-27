import { useCallback, useMemo, useRef } from "react";
import { motion, useReducedMotion } from "framer-motion";
import type { PanInfo } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { GameObject, PlayerId } from "../../adapter/types.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { previewAutomaticManaPayment } from "../../game/manaPaymentPreview.ts";
import { useCardHover } from "../../hooks/useCardHover.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useLocalizedCardName } from "../../hooks/useEngineCardData.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useIsMobile } from "../../hooks/useIsMobile.ts";
import { getPlayerId, useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useDragToCast } from "../../hooks/useDragToCast.ts";
import { objectImageProps } from "../../services/cardImageLookup.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import {
  collectObjectActions,
  deriveActivationAffordances,
  resolveSingleActionDispatch,
} from "../../viewmodel/cardActionChoice.ts";
import { CASTABLE_AFFORDANCE_ACTIVE } from "../../viewmodel/castableAffordance.ts";
import { spellCostDisplay } from "../../viewmodel/costLabel.ts";
import { commandZoneLeaders } from "../../viewmodel/commanderColumn.ts";
import { TabletopCardFace } from "../tabletop3d/TabletopCardFace.tsx";
import { CardArtFallback } from "../card/CardArtFallback.tsx";
import { getCardImageSrcSetProps } from "../card/cardImageSrcSet.ts";
import { ManaCostPips } from "../mana/ManaCostPips.tsx";

interface CommanderCardZoneProps {
  playerId: PlayerId;
  /** Split multiplayer overview pane: the card is ~40px wide, so the centered
   *  "Commander" wordmark spans the whole card and hides the cost pips. Drop
   *  the wordmark (the amber frame + dock position + tooltip still mark the
   *  commander) and shrink the pips so the cost reads instead. */
  splitOverview?: boolean;
  /** Uses the same live card renderer, scale, and motion as the Tabletop hand. */
  handPresentation?: boolean;
}

/**
 * Renders commander cards in the command zone. The standard command dock uses
 * printing images; the Tabletop hand dock uses the same live composed face and
 * motion as hand cards. Both share the interaction and commander-tax behavior.
 */
export function CommanderCardZone({
  playerId,
  splitOverview = false,
  handPresentation = false,
}: CommanderCardZoneProps) {
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
        <CommanderCard
          key={cmd.id}
          commander={cmd}
          splitOverview={splitOverview}
          handPresentation={handPresentation}
        />
      ))}
    </div>
  );
}

function CommanderCard({
  commander,
  splitOverview,
  handPresentation,
}: {
  commander: GameObject;
  splitOverview: boolean;
  handPresentation: boolean;
}) {
  const { t } = useTranslation("game");
  const isSignatureSpell = commander.signature_spell != null;
  const displayName = useLocalizedCardName(commander.name) ?? commander.name;
  const isCompactHeight = useIsCompactHeight();
  const isMobile = useIsMobile();
  const usesMobileHandPlacement = handPresentation && isMobile;
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
  const effectiveCost = useGameStore(
    (s) => s.spellCosts[String(commander.id)],
  );
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);
  const { handlers: hoverHandlers, firedRef } = useCardHover(commander.id);
  const tax = commander.commander_tax ?? 0;
  const shouldReduceMotion = useReducedMotion();
  const animationSpeedMultiplier = usePreferencesStore(
    (state) => state.animationSpeedMultiplier,
  );
  const animateSteam = !shouldReduceMotion && animationSpeedMultiplier > 0;
  const steamStyle = animateSteam
    ? { animationDuration: `${2.7 * animationSpeedMultiplier}s` }
    : {
        animation: "none",
        opacity: 0.16,
        transform: "translate3d(0, -8%, 0)",
      };

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

  // THE single authority — the same one the emblem chip adopts one file over.
  // `castAction !== null` / `ninjutsuActions.length > 0` were raw-bucket tests
  // with NEITHER a `WaitingFor` gate NOR a seat gate, and `PlayerArea` renders a
  // `<CommanderCardZone playerId={opponentId}/>` from the same `CommandDock`
  // subtree for every seat. In local/AI mode `legalActionsByObject` is computed
  // for the STATE's priority player rather than the viewer, so an opponent's
  // commander chip was clickable — and dispatched — from this seat.
  //
  // Only the non-mana ring is consulted: CastSpell and ActivateNinjutsu are both
  // non-mana, so CR 113.3b ("whenever they have priority") is the whole gate.
  // The mana ring would be dead weight here — a command-zone card publishes no
  // mana action — and consulting it would re-open the cast affordance during a
  // cost-payment prompt.
  const waitingFor = useGameStore((s) => s.waitingFor);
  const objects = useGameStore((s) => s.gameState?.objects);
  const canActForWaitingState = useCanActForWaitingState();
  const affordances = useMemo(
    () =>
      deriveActivationAffordances(waitingFor, canActForWaitingState, legalActionsByObject, objects),
    [waitingFor, canActForWaitingState, legalActionsByObject, objects],
  );
  const activationOffered = affordances.activatableObjectIds.has(commander.id);

  const canCast = activationOffered && castAction !== null;
  const canNinjutsu = activationOffered && ninjutsuActions.length > 0;

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
  const { displayCost, isReduced } = spellCostDisplay(
    effectiveCost,
    commander.mana_cost,
  );
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
          useUiStore.getState().openDebugContextMenu({
            objectId: commander.id,
            x: e.clientX,
            y: e.clientY,
            surface: "game",
          });
          return;
        }
        // Commander ninjutsu is a click affordance (unlike drag-to-cast): a
        // legal ActivateNinjutsu takes precedence over inspecting the card.
        if (canNinjutsu) {
          activateNinjutsu();
          return;
        }
        if (!isMobile) inspectObject(commander.id);
      }}
      onDoubleClick={canCast ? () => dispatchAction(castAction) : undefined}
      drag={canCast || false}
      dragSnapToOrigin
      onDragStart={startManaPaymentPreview}
      onDragEnd={onDragEnd}
      initial={handPresentation ? { opacity: 0, y: usesMobileHandPlacement ? 10 : 58 } : undefined}
      animate={handPresentation ? { opacity: 1, y: usesMobileHandPlacement ? 0 : 48 } : undefined}
      whileHover={
        handPresentation && !usesMobileHandPlacement
          ? { y: 38, scale: 1.08, zIndex: 30 }
          : undefined
      }
      whileDrag={{ cursor: "grabbing", scale: 1.04 }}
      data-object-id={commander.id}
      className={`group pointer-events-auto relative isolate ${
        canCast ? "cursor-grab" : canNinjutsu ? "cursor-pointer" : "cursor-default"
      }`}
      title={
        canCast
          ? isSignatureSpell
            ? tax > 0
              ? t("zone.castSignatureSpellTax", { name: displayName, tax })
              : t("zone.castSignatureSpell", { name: displayName })
            : tax > 0
              ? t("zone.castCommanderTax", { name: displayName, tax })
              : t("zone.castCommander", { name: displayName })
          : canNinjutsu
            ? t("zone.ninjutsuCommander", { name: displayName })
            : isSignatureSpell
              ? tax > 0
                ? t("zone.signatureSpellTitleTax", { name: displayName, tax })
                : t("zone.signatureSpellTitle", { name: displayName })
              : tax > 0
                ? t("zone.commanderTitleTax", { name: displayName, tax })
                : t("zone.commanderTitle", { name: displayName })
      }
      style={{
        width: handPresentation ? "var(--hand-card-w)" : "var(--card-w)",
        height: handPresentation ? "var(--hand-card-h)" : "var(--card-h)",
      }}
      data-hand-command-card={handPresentation || undefined}
    >
      {canCast && (
        <div
          aria-hidden
          data-commander-cast-aura
          className="tabletop-command-castable-aura pointer-events-none absolute -inset-px z-20 rounded-[4.4%/3.2%]"
        >
          <span
            className="tabletop-command-castable-steam tabletop-command-castable-steam--one"
            style={steamStyle}
          />
          <span
            className="tabletop-command-castable-steam tabletop-command-castable-steam--two"
            style={steamStyle}
          />
          <span
            className="tabletop-command-castable-steam tabletop-command-castable-steam--three"
            style={steamStyle}
          />
        </div>
      )}

      {handPresentation ? (
        <TabletopCardFace
          as="div"
          objectId={commander.id}
          displayCost={displayCost}
          isCostReduced={isReduced}
          className="relative z-10 h-full w-full shadow-[0_10px_22px_rgba(0,0,0,0.46)]"
          style={{ height: "100%", width: "100%" }}
        />
      ) : (
        <LegacyCommanderFace
          commander={commander}
          displayName={displayName}
          actionable={canCast || canNinjutsu}
        />
      )}

      {/* Commander badge — omitted in split panes where it would blanket the
          card and hide the cost pips. */}
      {!handPresentation && !splitOverview && (
        <div className="absolute -top-1 left-1/2 z-10 -translate-x-1/2 whitespace-nowrap rounded-sm bg-amber-700 px-1.5 py-px text-[8px] font-bold text-amber-100 shadow">
          {isSignatureSpell ? t("zone.signatureSpell") : t("zone.commander")}
        </div>
      )}

      {/* Actionable glow ring — castable or commander-ninjutsu available */}
      {!handPresentation && canNinjutsu && (
        <div className={`absolute inset-0 rounded-lg ${CASTABLE_AFFORDANCE_ACTIVE}`} />
      )}

      {/* Commander tax badge — nowrap: the absolute box is clamped to the
          card's width, so on narrow cards "Tax: +N" would otherwise break
          into two lines; centered overhang beats a wrapped pill. */}
      {tax > 0 && (
        <div
          data-hand-command-tax
          className={`absolute -bottom-1 left-1/2 z-10 -translate-x-1/2 whitespace-nowrap rounded-sm bg-amber-900 py-px font-bold text-amber-200 shadow ${
            splitOverview ? "px-1 text-[7px]" : "px-1.5 text-[8px]"
          }`}
        >
          {t("zone.tax", { tax })}
        </div>
      )}

      {/* Effective mana cost (includes tax) */}
      {!handPresentation && displayCost && (
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

function LegacyCommanderFace({
  commander,
  displayName,
  actionable,
}: {
  commander: GameObject;
  displayName: string;
  actionable: boolean;
}) {
  // Use the canonical printed-face identity so double-faced commanders resolve
  // the same art as battlefield, stack, and preview surfaces.
  const imageLookup = objectImageProps(commander);
  const { src, isLoading, rungs, advanceFailedSource } = useCardImage(imageLookup.cardName, {
    size: "normal",
    faceIndex: imageLookup.faceIndex,
    isToken: imageLookup.isToken,
    tokenFilters: imageLookup.tokenFilters,
    tokenImageRef: imageLookup.tokenImageRef,
    oracleId: imageLookup.oracleId,
    faceName: imageLookup.faceName,
  });

  return (
    <div className="relative h-full w-full overflow-hidden rounded-lg border border-amber-400/60 shadow-md">
      {isLoading ? (
        <div className="h-full w-full animate-pulse bg-gray-700" />
      ) : src ? (
        <img
          src={src}
          {...getCardImageSrcSetProps(src, rungs)}
          alt={displayName}
          className="h-full w-full object-cover"
          draggable={false}
          onError={() => advanceFailedSource?.(src)}
        />
      ) : (
        <CardArtFallback name={displayName} variant="artCrop" className="h-full w-full" />
      )}

      <div
        className={`absolute inset-0 transition-colors ${
          actionable
            ? "bg-amber-600/20 group-hover:bg-amber-600/5"
            : "bg-gray-900/50"
        }`}
      />
    </div>
  );
}
