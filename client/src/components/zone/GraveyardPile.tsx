import { useTranslation } from "react-i18next";

import type { GameObject } from "../../adapter/types.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { getWaitingForObjectChoiceIds } from "../../viewmodel/gameStateView.ts";
import { collectObjectActions, isManaObjectAction } from "../../viewmodel/cardActionChoice.ts";
import { objectImageProps } from "../../services/cardImageLookup.ts";
import { CardArtFallback } from "../card/CardArtFallback.tsx";
import { getCardImageSrcSetProps } from "../card/cardImageSrcSet.ts";

const EMPTY: readonly number[] = [];

interface GraveyardPileProps {
  playerId: number;
  onClick: () => void;
  size?: { width: string; height: string };
}

function TopCard({ object }: { object: GameObject }) {
  // Resolve via the engine's printed_ref (oracle_id + face) like every other
  // object-rendering surface — name-only lookup fails for DFC / transformed /
  // back-face cards (e.g. a transformed planeswalker) and would show the empty
  // placeholder instead of the card art.
  const imageProps = objectImageProps(object);
  const { src, isLoading, rungs, advanceFailedSource } = useCardImage(imageProps.cardName, {
    size: "normal",
    faceIndex: imageProps.faceIndex,
    isToken: imageProps.isToken,
    tokenFilters: imageProps.tokenFilters,
    tokenImageRef: imageProps.tokenImageRef,
    oracleId: imageProps.oracleId,
    faceName: imageProps.faceName,
  });

  if (isLoading) {
    return (
      <div className="h-full w-full animate-pulse rounded-lg border border-gray-600 bg-gray-700" />
    );
  }
  if (!src) {
    return <CardArtFallback name={object.name} className="h-full w-full rounded-lg" />;
  }

  return (
    <img
      src={src}
      {...getCardImageSrcSetProps(src, rungs)}
      alt={object.name}
      className="h-full w-full rounded-lg object-cover"
      draggable={false}
      onError={() => advanceFailedSource?.(src)}
    />
  );
}

export function GraveyardPile({ playerId, onClick, size }: GraveyardPileProps) {
  const { t } = useTranslation("game");
  const graveyard = useGameStore(
    (s) => s.gameState?.players[playerId]?.graveyard ?? EMPTY,
  );
  const topObject = useGameStore((s) => {
    const gy = s.gameState?.players[playerId]?.graveyard;
    const id = gy && gy.length > 0 ? gy[gy.length - 1] : null;
    return id != null ? (s.gameState?.objects[id] ?? null) : null;
  });

  // Check if any graveyard card is selectable for the current engine prompt.
  const canActForWaitingState = useCanActForWaitingState();
  const hasTargetableCards = useGameStore((s) => {
    if (!canActForWaitingState) return false;
    const objectChoiceIds = new Set(getWaitingForObjectChoiceIds(s.waitingFor));
    const gy = s.gameState?.players[playerId]?.graveyard ?? [];
    return gy.some((id) => objectChoiceIds.has(id));
  });

  // CR 702.66: during a Delve payment the graveyard cards aren't shown in the
  // hand fan (they carry only a mana-payment tap, excluded by
  // useCastableZoneObjects), so glow the pile to invite the player to open the
  // delve modal (ZoneViewer's `canDelveFromGraveyard`). Engine-authoritative:
  // glow exactly when a graveyard card has a mana-payment (delve) action.
  const hasDelveableCards = useGameStore((s) => {
    if (!canActForWaitingState) return false;
    if (s.waitingFor?.type !== "ManaPayment" || s.waitingFor.data.convoke_mode !== "Delve") {
      return false;
    }
    const objects = s.gameState?.objects;
    const gy = s.gameState?.players[playerId]?.graveyard ?? [];
    return gy.some((id) => {
      const obj = objects?.[id];
      return (
        Boolean(obj) &&
        collectObjectActions(s.legalActionsByObject, id).some((action) =>
          isManaObjectAction(action, obj),
        )
      );
    });
  });

  const count = graveyard.length;
  if (count === 0) return null;

  const stackDepth = Math.min(count - 1, 3);
  const w = size?.width ?? "var(--card-w)";
  const h = size?.height ?? "var(--card-h)";

  return (
    <button
      onClick={onClick}
      className={`group relative cursor-pointer ${hasTargetableCards || hasDelveableCards ? "ring-2 ring-amber-400/60 rounded-lg shadow-[0_0_12px_3px_rgba(201,176,55,0.8)]" : ""}`}
      title={t("zone.graveyardTitle", { count })}
      data-graveyard-pile={playerId}
      data-grouped-ids={graveyard.join(" ")}
      style={{ width: w, height: h }}
    >
      {/* Shadow stack layers */}
      {Array.from({ length: stackDepth }).map((_, i) => (
        <div
          key={i}
          className="absolute rounded-lg border border-gray-600 bg-gray-800"
          style={{
            width: w,
            height: h,
            bottom: (i + 1) * 3,
            left: (i + 1) * -1,
          }}
        />
      ))}

      {/* Top card — full card image */}
      <div className="relative h-full w-full overflow-hidden rounded-lg border border-gray-500 shadow-md group-hover:border-gray-300 transition-colors">
        {topObject && <TopCard object={topObject} />}
        <div className="absolute inset-0 bg-black/20 group-hover:bg-black/0 transition-colors" />
      </div>

      {/* Count badge */}
      <div className="absolute -bottom-1 -right-1 z-10 flex h-5 w-5 items-center justify-center rounded-full bg-gray-900 text-[9px] font-bold text-gray-300 ring-1 ring-gray-600">
        {count}
      </div>
    </button>
  );
}
