import { useTranslation } from "react-i18next";

import { dispatchAction } from "../../game/dispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { PopoverMenu } from "../menu/PopoverMenu.tsx";
import { YieldMuteIcon } from "../stack/YieldMuteIcon.tsx";

/**
 * CR 117.3d: the viewer's standing priority yields. Presented as a single
 * fixed-footprint summary chip ("Auto-passing ×N") that opens a portaled
 * PopoverMenu holding the scrollable, per-row revoke list plus a clear-all.
 * Collapsing the list into a chip keeps the action rail's height constant no
 * matter how many yields accumulate (an inline list grew unbounded and pushed
 * the surrounding layout). Purely a display + dispatch surface — the engine
 * owns the yield state (redacted per-viewer in `priority_yields`), and each
 * revoke echoes the stored `YieldTarget` verbatim.
 */
export function PriorityYieldList() {
  const { t } = useTranslation("game");
  const yields = useGameStore((s) => s.gameState?.priority_yields);
  const objects = useGameStore((s) => s.gameState?.objects);

  if (!yields || yields.length === 0) return null;

  return (
    <PopoverMenu
      ariaLabel={t("priorityYield.listHeader")}
      menuWidthPx={240}
      renderTrigger={({ ref, open, toggle }) => (
        <button
          ref={ref}
          type="button"
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={toggle}
          className={`pointer-events-auto flex items-center gap-1.5 rounded-full px-2.5 py-1 text-[11px] font-semibold shadow-sm ring-1 transition-colors ${
            open
              ? "bg-amber-400 text-black ring-amber-300"
              : "bg-amber-500/90 text-black ring-amber-300/80 hover:bg-amber-400"
          }`}
        >
          <YieldMuteIcon muted className="h-3.5 w-3.5 shrink-0" />
          <span>{t("priorityYield.menuButtonShortActive")}</span>
          <span className="rounded-full bg-black/25 px-1.5 leading-tight">{yields.length}</span>
        </button>
      )}
    >
      {(close) => (
        <>
          <div className="flex items-center justify-between px-3 pb-1.5 pt-1">
            <span className="text-sm font-bold text-white">{t("priorityYield.listHeader")}</span>
            <button
              type="button"
              className="rounded px-1.5 py-0.5 text-xs font-semibold text-amber-200 transition-colors hover:bg-white/10"
              onClick={() => {
                dispatchAction({ type: "SetPriorityYield", data: { op: { type: "ClearAll" } } });
                close();
              }}
            >
              {t("priorityYield.clearAll")}
            </button>
          </div>
          <div className="mx-2 mb-1 border-t border-white/10" />
          <ul className="flex flex-col">
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
                <li key={key} className="flex items-center justify-between gap-2 px-3 py-1.5">
                  <span className="truncate text-sm text-gray-200">{label}</span>
                  <button
                    type="button"
                    className="shrink-0 rounded px-1.5 py-0.5 text-xs font-semibold text-amber-200 transition-colors hover:bg-white/10"
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
        </>
      )}
    </PopoverMenu>
  );
}
