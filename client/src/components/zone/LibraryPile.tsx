import { useCallback, useMemo } from "react";
import { useTranslation } from "react-i18next";

import type { ObjectId } from "../../adapter/types.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { useCanActForWaitingState, usePlayerId } from "../../hooks/usePlayerId.ts";
import { CARD_BACK_URL } from "../../services/scryfall.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { CASTABLE_AFFORDANCE_IDLE } from "../../viewmodel/castableAffordance.ts";
import { playOrCastActionsForObject } from "../../viewmodel/cardActionChoice.ts";
import { isLibraryCardRevealedToViewer } from "../../viewmodel/gameStateView.ts";

interface LibraryPileProps {
  playerId: number;
  size?: { width: string; height: string };
  /**
   * Opens the full library viewer (ZoneViewer, zone="library"). When provided
   * and the top of the library is visible to this viewer (a public reveal or a
   * private look — CR 701.20b), a pile click opens the modal instead of
   * casting directly; play-from-top then happens inside the modal, matching the
   * graveyard/exile click→view→cast flow. Without this prop the pile falls back
   * to its standalone click-to-play behavior.
   */
  onView?: () => void;
}

function TopCard({ cardName }: { cardName: string }) {
  const { src } = useCardImage(cardName, { size: "normal" });

  if (!src) {
    return (
      <div
        className="h-full w-full rounded-lg bg-gray-700 border border-gray-600"
      />
    );
  }

  return (
    <img
      src={src}
      alt={cardName}
      className="h-full w-full rounded-lg object-cover"
      draggable={false}
    />
  );
}

export function LibraryPile({ playerId, size, onView }: LibraryPileProps) {
  const { t } = useTranslation("game");
  const myId = usePlayerId();
  const count = useGameStore(
    (s) => s.gameState?.players[playerId]?.library?.length ?? 0,
  );
  const topObjectId = useGameStore((s) => {
    const lib = s.gameState?.players[playerId]?.library;
    if (!lib || lib.length === 0) return null;
    // library[0] = top of library (engine convention from zones.rs)
    return lib[0];
  });
  const isRevealed = useGameStore((s) => {
    if (topObjectId == null) return false;
    return s.gameState?.revealed_cards?.includes(topObjectId) ?? false;
  });
  const topCardName = useGameStore((s) => {
    if (topObjectId == null) return null;
    const peek =
      playerId === myId &&
      (s.gameState?.players[playerId]?.can_look_at_top_of_library ?? false);
    // Gate visibility on the engine's reveal sets (mirrors OpponentHand), never
    // on name redaction: single-player renders the raw, unredacted state, so the
    // top card name is present even for an opponent's hidden top. CR 701.20b
    // (public reveal) and CR 701.20e (private look, e.g. Mishra's Bauble) are
    // the only windows that expose an opponent's top.
    const revealedToMe = isLibraryCardRevealedToViewer(s.gameState ?? null, topObjectId, myId);
    if (!peek && !revealedToMe) return null;
    return s.gameState?.objects[topObjectId]?.name ?? null;
  });

  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const canActForWaitingState = useCanActForWaitingState();
  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setPreviewSticky = useUiStore((s) => s.setPreviewSticky);
  const hoverProps = useInspectHoverProps();
  const dispatchAction = useGameDispatch();

  const isMyLibrary = playerId === myId;
  const hasPriority = waitingFor?.type === "Priority" && canActForWaitingState;

  // CR 401.5 + CR 118.9 + CR 305.9: cast/play-action surfacing is engine-
  // authoritative — the entry exists in `legalActionsByObject` only when the
  // engine has already validated the TopOfLibraryCastPermission filter, mana,
  // timing, and (for `PlayLand`) the land-drop slot. The frontend renders
  // the reported actions, never computes them. Future Sight / Bolas's
  // Citadel / Magus of the Future surface `PlayLand` here; Mystic Forge /
  // Realmwalker surface the `CastSpell` family.
  const playActions = useMemo(() => {
    if (!isMyLibrary || !hasPriority || topObjectId == null) return [];
    return playOrCastActionsForObject(legalActionsByObject, topObjectId);
  }, [isMyLibrary, hasPriority, topObjectId, legalActionsByObject]);

  const canPlay = playActions.length > 0;

  const handlePlay = useCallback(() => {
    if (playActions.length === 0 || topObjectId == null) return;
    if (playActions.length === 1) {
      void dispatchAction(playActions[0]);
    } else {
      // Multiple options (e.g., cast normal + alt-cost) — defer to the shared
      // ability-choice modal so the player can pick.
      setPendingAbilityChoice({ objectId: topObjectId as ObjectId, actions: playActions });
    }
  }, [playActions, topObjectId, dispatchAction, setPendingAbilityChoice]);

  const { handlers: longPressHandlers, firedRef: longPressFired } = useLongPress(
    useCallback(() => {
      if (topObjectId == null || topCardName == null) return;
      inspectObject(topObjectId as ObjectId);
      setPreviewSticky(true);
    }, [inspectObject, setPreviewSticky, topObjectId, topCardName]),
  );

  if (count === 0) return null;

  const stackDepth = Math.min(count - 1, 4);
  const isPeeking = topCardName != null;
  // A visible top (public reveal or private look) means there's something to
  // see in the library viewer; clicking opens it (when the parent wires onView)
  // rather than casting directly. Cast-from-top then lives inside the modal.
  const canView = isPeeking && onView != null;
  // Desktop hover preview for the revealed top card (skipped on mobile by the
  // hook, where a tap would spuriously open the full-screen preview overlay).
  const topHoverProps =
    isPeeking && topObjectId != null ? hoverProps(topObjectId as ObjectId) : undefined;
  const libraryLabel = t("zone.libraryCount", { count });
  const playLabel = t("zone.playFromTop", { name: topCardName ?? t("zone.topOfLibrary") });
  const w = size?.width ?? "var(--card-w)";
  const h = size?.height ?? "var(--card-h)";

  return (
    <div
      className="relative"
      title={canPlay ? playLabel : libraryLabel}
      data-library-pile={playerId}
      style={{ width: w, height: h }}
    >
      {/* Stack layers */}
      {Array.from({ length: stackDepth }).map((_, i) => (
        <div
          key={i}
          className="pointer-events-none absolute rounded-lg border border-gray-700 bg-gray-800"
          style={{
            width: w,
            height: h,
            bottom: (i + 1) * 3,
            left: (i + 1) * 1,
          }}
        />
      ))}

      {/* Top card */}
      <button
        type="button"
        onClick={() => {
          if (longPressFired.current) {
            longPressFired.current = false;
            return;
          }
          // Prefer opening the viewer when the top is visible — the modal is
          // where play-from-top happens (mirrors graveyard/exile). Fall back to
          // direct cast only when no viewer is wired.
          if (canView) {
            onView();
            return;
          }
          if (canPlay) handlePlay();
        }}
        disabled={!canView && !canPlay && topCardName == null}
        aria-label={canPlay ? playLabel : libraryLabel}
        data-library-top-cast={canPlay ? "true" : "false"}
        {...topHoverProps}
        {...longPressHandlers}
        className={`relative block h-full w-full overflow-hidden rounded-lg border shadow-md ${
          canPlay
            ? `border-amber-400 ${CASTABLE_AFFORDANCE_IDLE} cursor-pointer`
            : isRevealed
              ? `border-amber-500 ${canView ? "cursor-pointer" : "cursor-default"}`
              : isPeeking
                ? `border-cyan-600 ${canView ? "cursor-pointer" : "cursor-default"}`
                : "border-gray-600 cursor-default"
        }`}
      >
        {isPeeking ? (
          <TopCard cardName={topCardName} />
        ) : (
          <img
            src={CARD_BACK_URL}
            alt={t("zone.libraryAlt")}
            className="h-full w-full rounded-lg object-cover"
            draggable={false}
          />
        )}
      </button>

      {/* Count badge */}
      <div className="absolute -bottom-1 -right-1 z-10 flex h-5 w-5 items-center justify-center rounded-full bg-gray-900 text-[9px] font-bold text-gray-300 ring-1 ring-gray-600">
        {count}
      </div>
    </div>
  );
}
