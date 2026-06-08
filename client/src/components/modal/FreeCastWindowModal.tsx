import { useTranslation } from "react-i18next";

import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { DialogShell } from "./DialogShell.tsx";

/**
 * CR 608.2g + CR 601.2 + CR 202.3: Invoke Calamity's free-cast window. The
 * controller may cast one of the offered instant/sorcery candidates (from their
 * graveyard and/or hand) without paying its mana cost, up to the remaining cast
 * count and within the running total-mana-value budget, or finish the window.
 *
 * Display-only: the engine owns candidate eligibility, the MV budget, and the
 * re-offer loop. This modal renders the engine-provided candidates and dispatches
 * `FreeCastWindowChoice`.
 */
export function FreeCastWindowModal() {
  const { t } = useTranslation("game");
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const objects = useGameStore((s) => s.gameState?.objects);
  const dispatch = useGameStore((s) => s.dispatch);

  if (waitingFor?.type !== "CastOffer") return null;
  const kind = waitingFor.data.kind;
  if (kind.type !== "FreeCastWindow") return null;
  if (!canActForWaitingState) return null;

  const subtitle =
    kind.remaining_mv_budget !== undefined && kind.remaining_mv_budget !== null
      ? t("freeCastWindow.subtitle", {
          remaining: kind.remaining_casts,
          budget: kind.remaining_mv_budget,
        })
      : t("freeCastWindow.subtitleNoBudget", { remaining: kind.remaining_casts });

  return (
    <DialogShell
      eyebrow={t("freeCastWindow.eyebrow")}
      title={t("freeCastWindow.title")}
      subtitle={subtitle}
    >
      <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
        {kind.candidates.map((id) => {
          const name = objects?.[id]?.name ?? `#${id}`;
          return (
            <button
              key={id}
              onClick={() =>
                dispatch({ type: "FreeCastWindowChoice", data: { selection: id } })
              }
              className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
            >
              <span className="font-semibold text-white">
                {t("freeCastWindow.castNamed", { name })}
              </span>
              <span className="ml-2 text-xs text-slate-400">
                {t("freeCastWindow.castSuffix")}
              </span>
            </button>
          );
        })}
        <button
          onClick={() =>
            dispatch({ type: "FreeCastWindowChoice", data: { selection: undefined } })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-amber-400/30"
        >
          <span className="font-semibold text-white">{t("freeCastWindow.done")}</span>
          <span className="ml-2 text-xs text-slate-400">
            {t("freeCastWindow.doneSuffix")}
          </span>
        </button>
      </div>
    </DialogShell>
  );
}
