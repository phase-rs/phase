import { useTranslation } from "react-i18next";

import { dispatchAction } from "../../game/dispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { CardImage } from "../card/CardImage.tsx";
import { ManaCostSymbols } from "../mana/ManaCostSymbols.tsx";

export function PlanechasePanel() {
  const { t } = useTranslation("game");
  const gameState = useGameStore((s) => s.gameState);
  const legalActions = useGameStore((s) => s.legalActions);
  const planechase = gameState?.derived?.planechase;
  const activePlane =
    planechase?.active_plane != null ? gameState?.objects[planechase.active_plane] : undefined;
  const canRoll =
    planechase?.can_roll === true && legalActions.some((action) => action.type === "RollPlanarDie");

  if (!planechase) return null;

  return (
    <div className="pointer-events-auto absolute left-1/2 top-2 z-30 flex -translate-x-1/2 items-center gap-3 rounded-md border border-slate-700 bg-slate-950/90 px-3 py-2 shadow-lg backdrop-blur">
      <div className="h-[112px] w-[80px] shrink-0 [--card-h:112px] [--card-w:80px]">
        {activePlane ? (
          <CardImage
            cardName={activePlane.name}
            oracleId={activePlane.printed_ref?.oracle_id ?? undefined}
            faceName={activePlane.printed_ref?.face_name ?? undefined}
            faceDown={activePlane.face_down}
            size="small"
          />
        ) : (
          <div className="flex h-full w-full items-center justify-center rounded-md border border-slate-700 bg-slate-900 text-xs text-slate-400">
            {t("planechase.noPlane")}
          </div>
        )}
      </div>
      <div className="flex min-w-32 flex-col gap-2 text-sm text-slate-100">
        <div>
          <div className="text-xs uppercase text-slate-400">{t("planechase.active")}</div>
          <div className="max-w-44 truncate font-semibold">{activePlane?.name ?? t("planechase.none")}</div>
        </div>
        <div className="flex items-center justify-between gap-4 text-xs text-slate-300">
          <span>{t("planechase.deckCount", { count: planechase.planar_deck_count })}</span>
          <span className="flex items-center gap-1">
            {t("planechase.cost")}
            <ManaCostSymbols cost={planechase.current_roll_cost} size="sm" />
          </span>
        </div>
        {canRoll && (
          <button
            type="button"
            className="rounded-md bg-amber-500 px-3 py-1.5 text-sm font-semibold text-slate-950 hover:bg-amber-400"
            onClick={() => dispatchAction({ type: "RollPlanarDie" })}
          >
            {t("planechase.roll")}
          </button>
        )}
      </div>
    </div>
  );
}
