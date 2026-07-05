import { useTranslation } from "react-i18next";

import { dispatchAction } from "../../game/dispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";

/**
 * CR 117.3d: Compact list of the viewer's standing priority yields, with a
 * per-row revoke and a clear-all. Purely a display + dispatch surface — the
 * engine owns the yield state (redacted per-viewer in `priority_yields`), and
 * each revoke echoes the stored `YieldTarget` verbatim.
 */
export function PriorityYieldList() {
  const { t } = useTranslation("game");
  const yields = useGameStore((s) => s.gameState?.priority_yields);
  const objects = useGameStore((s) => s.gameState?.objects);

  if (!yields || yields.length === 0) return null;

  return (
    <div className="rounded-lg bg-gray-900/90 p-2 text-[11px] ring-1 ring-white/10">
      <div className="mb-1 flex items-center justify-between">
        <span className="font-semibold text-purple-200">{t("priorityYield.listHeader")}</span>
        <button
          className="rounded px-1.5 py-0.5 text-amber-200 hover:bg-white/10"
          onClick={() => dispatchAction({ type: "SetPriorityYield", data: { op: { type: "ClearAll" } } })}
        >
          {t("priorityYield.clearAll")}
        </button>
      </div>
      <ul className="flex flex-col gap-0.5">
        {yields.map((y) => {
          const key =
            "ThisObject" in y.target
              ? `${y.player}-obj-${y.target.ThisObject.source_id}-${y.target.ThisObject.incarnation}`
              : `${y.player}-all-${y.target.AllCopies.card_id}`;
          const label =
            "ThisObject" in y.target
              ? objects?.[y.target.ThisObject.source_id]?.name ?? t("priorityYield.yieldThis")
              : t("priorityYield.yieldAllCopies");
          return (
            <li key={key} className="flex items-center justify-between gap-2">
              <span className="truncate text-gray-200">{label}</span>
              <button
                className="shrink-0 rounded px-1.5 py-0.5 text-amber-200 hover:bg-white/10"
                onClick={() =>
                  dispatchAction({
                    type: "SetPriorityYield",
                    data: { op: { type: "Remove", data: { target: y.target } } },
                  })
                }
              >
                {t("priorityYield.revoke")}
              </button>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
