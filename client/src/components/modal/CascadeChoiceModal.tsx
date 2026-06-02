import { useTranslation } from "react-i18next";

import type { GameAction } from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { DialogShell } from "./DialogShell.tsx";

/**
 * CR 702.85a: Cascade — when a cascade-source spell finds an eligible nonland
 * card with mana value strictly less than the source's mana value, the caster
 * may cast it without paying its mana cost or decline. Declining shuffles the
 * hit and all misses to the bottom of the library in a random order.
 */
export function CascadeChoiceModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  if (waitingFor?.type !== "CastOffer") return null;
  const kind = waitingFor.data.kind;
  if (kind.type !== "Cascade" && kind.type !== "Discover") return null;
  if (!canActForWaitingState) return null;

  if (kind.type === "Discover") {
    return (
      <CascadeChoiceContent
        actionType="DiscoverChoice"
        hitCardId={kind.hit_card}
        missCount={kind.exiled_misses.length}
        promptKind="Discover"
        dispatch={dispatch}
      />
    );
  }

  return (
    <CascadeChoiceContent
      actionType="CascadeChoice"
      hitCardId={kind.hit_card}
      missCount={kind.exiled_misses.length}
      promptKind="Cascade"
      sourceMv={kind.source_mv}
      dispatch={dispatch}
    />
  );
}

function CascadeChoiceContent({
  actionType,
  hitCardId,
  missCount,
  promptKind,
  sourceMv,
  dispatch,
}: {
  actionType: "CascadeChoice" | "DiscoverChoice";
  hitCardId: number;
  missCount: number;
  promptKind: "Cascade" | "Discover";
  sourceMv?: number;
  dispatch: (action: GameAction) => Promise<unknown>;
}) {
  const { t } = useTranslation("game");
  const obj = useGameStore((s) => s.gameState?.objects[hitCardId]);

  if (!obj) return null;

  const subtitle =
    promptKind === "Cascade"
      ? t("cascadeChoice.subtitleCascade", {
          name: obj.name,
          sourceMv,
          total: missCount + 1,
        })
      : t("cascadeChoice.subtitleDiscover", {
          name: obj.name,
          missCount,
        });

  return (
    <DialogShell
      eyebrow={
        promptKind === "Cascade"
          ? t("cascadeChoice.cascadeEyebrow")
          : t("cascadeChoice.discoverEyebrow")
      }
      title={t("cascadeChoice.title", { name: obj.name })}
      subtitle={subtitle}
      previewObjectId={hitCardId}
    >
      <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
        <button
          onClick={() =>
            dispatch({
              type: actionType,
              data: { choice: { type: "Cast" } },
            })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
        >
          <span className="font-semibold text-white">
            {t("cascadeChoice.castNamed", { name: obj.name })}
          </span>
          <span className="ml-2 text-xs text-slate-400">
            {t("cascadeChoice.castSuffix")}
          </span>
        </button>
        <button
          onClick={() =>
            dispatch({
              type: actionType,
              data: { choice: { type: "Decline" } },
            })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-amber-400/30"
        >
          <span className="font-semibold text-white">
            {promptKind === "Discover"
              ? t("cascadeChoice.putIntoHand")
              : t("cascadeChoice.decline")}
          </span>
          <span className="ml-2 text-xs text-slate-400">
            {promptKind === "Discover"
              ? t("cascadeChoice.discoverDeclineSuffix")
              : t("cascadeChoice.cascadeDeclineSuffix")}
          </span>
        </button>
      </div>
    </DialogShell>
  );
}
