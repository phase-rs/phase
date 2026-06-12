import { useCallback, useMemo } from "react";
import { useTranslation } from "react-i18next";

import type { GameAction, GameObject } from "../../adapter/types.ts";
import { CardImage } from "../card/CardImage.tsx";
import { objectImageProps } from "../../services/cardImageLookup.ts";
import { ModalPanelShell } from "../ui/ModalPanelShell.tsx";
import { ScrollableCardStrip } from "../modal/ChoiceOverlay.tsx";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useCanActForWaitingState, usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import {
  getPlayerZoneIds,
  getWaitingForObjectChoiceIds,
  isLibraryCardRevealedToViewer,
} from "../../viewmodel/gameStateView.ts";
import { CASTABLE_AFFORDANCE_ACTIVE } from "../../viewmodel/castableAffordance.ts";
import { playOrCastActionsForObject, resolveSingleActionDispatch } from "../../viewmodel/cardActionChoice.ts";

interface ZoneViewerProps {
  zone: "graveyard" | "exile" | "library";
  playerId: number;
  onClose: () => void;
}

const ZONE_TITLE_KEYS: Record<string, string> = {
  graveyard: "zone.graveyard",
  exile: "zone.exile",
  library: "zone.library",
};

const ZONE_TITLE_LOWER_KEYS: Record<string, string> = {
  graveyard: "zone.graveyardLower",
  exile: "zone.exileLower",
  library: "zone.libraryLower",
};

export function ZoneViewer({ zone, playerId, onClose }: ZoneViewerProps) {
  const { t } = useTranslation("game");
  const objects = useGameStore((s) => s.gameState?.objects);
  const gameState = useGameStore((s) => s.gameState);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);
  const dispatchAction = useGameDispatch();
  const canActForWaitingState = useCanActForWaitingState();
  const viewerId = usePlayerId();
  const zoneIds = useMemo(
    () => getPlayerZoneIds(gameState, zone, playerId),
    [gameState, playerId, zone],
  );

  const cards = useMemo(() => {
    if (!objects) return [];
    const resolved = zoneIds.map((id) => objects[id]).filter(Boolean) as GameObject[];
    // CR 701.20: the library viewer shows only the cards the engine has revealed
    // to this viewer (top-of-library reveals + private looks), top-first.
    // Unrevealed cards are omitted entirely — visibility is gated on the engine's
    // reveal sets, never inferred from name redaction (single-player renders the
    // raw, unredacted state).
    if (zone === "library") {
      // The "look at the top card of your library" capability (Future Sight,
      // Bolas's Citadel, Oracle of Mul Daya) is a continuous static that exposes
      // the OWNER's own top card without adding it to revealed_cards/private_look
      // — mirror LibraryPile's `peek` clause so that top still shows (and stays
      // castable) through the modal.
      const ownTopId =
        viewerId === playerId &&
        (gameState?.players[playerId]?.can_look_at_top_of_library ?? false)
          ? gameState?.players[playerId]?.library?.[0]
          : undefined;
      return resolved.filter(
        (obj) =>
          isLibraryCardRevealedToViewer(gameState, obj.id, viewerId) ||
          obj.id === ownTopId,
      );
    }
    return resolved;
  }, [objects, zoneIds, zone, gameState, viewerId, playerId]);

  const hasPriority = waitingFor?.type === "Priority" && canActForWaitingState;

  const currentLegalTargets = useMemo(() => {
    const targets = new Set<number>();
    if (!canActForWaitingState) return targets;
    for (const objectId of getWaitingForObjectChoiceIds(waitingFor)) {
      targets.add(objectId);
    }
    return targets;
  }, [canActForWaitingState, waitingFor]);

  // Click-to-cast mirrors ZoneHand: a lone non-confirming action dispatches
  // immediately, otherwise the shared ability-choice modal opens. Closing the
  // viewer surfaces that modal (DialogHost z-40) which would otherwise sit
  // behind the ZoneViewer panel (z-50), and matches Arena dismissing the zone
  // view once a cast begins. resolveSingleActionDispatch is the single
  // auto-vs-confirm authority — never re-decided inline here.
  const handleCast = useCallback(
    (target: GameObject, actions: GameAction[]) => {
      inspectObject(null);
      const auto = resolveSingleActionDispatch(actions, target);
      if (auto) {
        dispatch(auto);
      } else {
        setPendingAbilityChoice({ objectId: target.id, actions });
      }
      onClose();
    },
    [dispatch, inspectObject, setPendingAbilityChoice, onClose],
  );

  const zoneLabel = t(ZONE_TITLE_KEYS[zone]);

  return (
    <ModalPanelShell
      title={t("zone.zoneTitle", { zone: t(ZONE_TITLE_KEYS[zone]), count: cards.length })}
      onClose={onClose}
      maxWidthClassName="max-w-5xl"
      bodyClassName="flex min-h-0 flex-col"
      overlayClassName="z-[60]"
    >
      <div className="min-h-0 flex-1 px-2 pb-2 lg:px-6 lg:pb-6">
        {cards.length === 0 ? (
          <p className="py-8 text-center text-sm italic text-gray-600">
            {t("zone.noCardsIn", { zone: t(ZONE_TITLE_LOWER_KEYS[zone]) })}
          </p>
        ) : (
          <ScrollableCardStrip
            stripClassName="zone-viewer-strip"
            innerClassName="flex items-center gap-2 lg:gap-3"
          >
            {cards.map((obj) => {
              // CR 702.81a + CR 702.143a + CR 715.3a + CR 702.62a + CR 702.170d + CR 702.185a:
              // Engine surfaces a CastSpell-family action for every legally
              // castable graveyard/exile card (Retrace, Adventure, Foretell,
              // Suspend, Plot, Warp, etc.). The zone viewer surfaces whatever
              // the engine reports — no per-mechanic permission inspection.
              //
              // CR 401.5 + CR 118.9: for `library`, the engine only surfaces a
              // play/cast action on the top card when a TopOfLibraryCastPermission
              // (Future Sight, Bolas's Citadel, Mystic Forge, …) is active, so
              // the playable affordance naturally lands on the revealed top.
              //
              // CR 715.3d / CR 400.7i: this includes opponent-OWNED cards in
              // exile the viewer was granted permission to play (Hostage Taker,
              // Gonti, Thief of Sanity). Those live in the owner's exile pile,
              // so castability must NOT be gated on the pile belonging to the
              // viewer — `legalActionsByObject` (engine authority, keyed to the
              // player the permission was granted to) is the sole gate.
              const castActions = hasPriority
                ? playOrCastActionsForObject(legalActionsByObject, obj.id)
                : [];
              const isValidTarget = currentLegalTargets.has(obj.id);
              return (
                <ZoneCard
                  key={obj.id}
                  obj={obj}
                  isValidTarget={isValidTarget}
                  canCast={castActions.length > 0}
                  castTitle={t("zone.castFromZone", { zone: zoneLabel, name: obj.name })}
                  onTarget={() => dispatchAction({ type: "ChooseTarget", data: { target: { Object: obj.id } } })}
                  onCast={() => handleCast(obj, castActions)}
                />
              );
            })}
          </ScrollableCardStrip>
        )}
      </div>
    </ModalPanelShell>
  );
}

function ZoneCard({
  obj,
  isValidTarget,
  canCast,
  castTitle,
  onTarget,
  onCast,
}: {
  obj: GameObject;
  isValidTarget: boolean;
  canCast: boolean;
  castTitle: string;
  onTarget: () => void;
  onCast: () => void;
}) {
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setPreviewSticky = useUiStore((s) => s.setPreviewSticky);
  const hoverProps = useInspectHoverProps();
  const { handlers: longPressHandlers, firedRef: longPressFired } = useLongPress(
    useCallback(() => {
      inspectObject(obj.id);
      setPreviewSticky(true);
    }, [inspectObject, setPreviewSticky, obj.id]),
  );

  const handleClick = useCallback((e: React.MouseEvent) => {
    if (longPressFired.current) { longPressFired.current = false; return; }
    if (useUiStore.getState().debugInteractionMode) {
      e.stopPropagation();
      useUiStore.getState().openDebugContextMenu({ objectId: obj.id, x: e.clientX, y: e.clientY });
      return;
    }
    if (isValidTarget) { onTarget(); return; }
    if (canCast) onCast();
  }, [obj.id, isValidTarget, canCast, onTarget, onCast, longPressFired]);

  return (
    <div
      className={`group relative inline-flex shrink-0 cursor-pointer rounded-lg transition-transform ${
        isValidTarget
          ? CASTABLE_AFFORDANCE_ACTIVE
          : canCast
            ? "hover:scale-[1.03]"
            : "hover:ring-1 hover:ring-white/20"
      }`}
      data-card-hover
      title={canCast && !isValidTarget ? castTitle : undefined}
      {...hoverProps(obj.id)}
      onClick={handleClick}
      {...longPressHandlers}
    >
      {/* Resolve the image via the engine's printed_ref (oracle_id + face)
          like every other object-rendering modal — name-only lookup fails for
          DFC / transformed / back-face cards (e.g. a transformed planeswalker),
          which then falls back to the broken-image div. In the zone strip that
          fallback is sized up to ~560px, so a failed image rendered "huge" with
          text instead of art. */}
      <CardImage {...objectImageProps(obj)} size="normal" />
      {canCast && !isValidTarget && (
        <>
          {/* Arena-style purple "playable" affordance — same treatment as the
              ZoneHand castable stack, replacing the per-card "Cast/Play" button
              so castable cards keep their natural size. pointer-events-none lets
              clicks fall through to the card's own onClick (handleCast). */}
          <div className="pointer-events-none absolute inset-0 rounded-lg bg-purple-600/30 transition-colors group-hover:bg-purple-600/10" />
          <div className="pointer-events-none absolute inset-0 rounded-lg ring-2 ring-purple-400/70 shadow-[0_0_12px_3px_rgba(147,51,234,0.5)]" />
        </>
      )}
    </div>
  );
}
