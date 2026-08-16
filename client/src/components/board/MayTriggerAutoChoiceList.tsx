import { useTranslation } from "react-i18next";

import type { MayTriggerAutoChoiceKey } from "../../adapter/types.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { PopoverMenu } from "../menu/PopoverMenu.tsx";

/**
 * CR 603.5: the viewer's stored "don't ask again" auto-choices for optional
 * ("may") triggered abilities. Presented as a single fixed-footprint summary
 * chip ("Auto-deciding ×N") that opens a portaled PopoverMenu holding the
 * scrollable, per-row remove list plus a clear-all — mirroring
 * `PriorityYieldList` so the action rail height stays constant no matter how
 * many auto-choices accumulate. Purely a display + dispatch surface: the engine
 * owns the state (redacted per-viewer in `may_trigger_auto_choices`), enforces
 * actor scoping on the write, and each remove echoes the stored key verbatim.
 */
export function MayTriggerAutoChoiceList() {
  const { t } = useTranslation("game");
  const choices = useGameStore((s) => s.gameState?.may_trigger_auto_choices);
  const objects = useGameStore((s) => s.gameState?.objects);

  if (!choices || choices.length === 0) return null;

  const rowKey = (key: MayTriggerAutoChoiceKey) => {
    switch (key.origin.type) {
      case "Printed":
        return `${key.player}-${key.source_id}-p${key.origin.trigger_index}`;
      case "Keyword":
        return `${key.player}-${key.source_id}-k${key.origin.keyword}`;
      case "Definition":
        return `${key.player}-${key.source_id}-d${JSON.stringify(key.origin.definition_ref)}`;
    }
  };

  return (
    <PopoverMenu
      ariaLabel={t("mayTriggerAutoChoice.listHeader")}
      menuWidthPx={260}
      renderTrigger={({ ref, open, toggle }) => (
        <button
          ref={ref}
          type="button"
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={toggle}
          className={`pointer-events-auto flex items-center gap-1.5 rounded-full px-2.5 py-1 text-[11px] font-semibold shadow-sm ring-1 transition-colors ${
            open
              ? "bg-sky-400 text-black ring-sky-300"
              : "bg-sky-500/90 text-black ring-sky-300/80 hover:bg-sky-400"
          }`}
        >
          <span>{t("mayTriggerAutoChoice.menuButtonShortActive")}</span>
          <span className="rounded-full bg-black/25 px-1.5 leading-tight">{choices.length}</span>
        </button>
      )}
    >
      {(close) => (
        <>
          <div className="flex items-center justify-between px-3 pb-1.5 pt-1">
            <span className="text-sm font-bold text-white">
              {t("mayTriggerAutoChoice.listHeader")}
            </span>
            <button
              type="button"
              className="rounded px-1.5 py-0.5 text-xs font-semibold text-sky-200 transition-colors hover:bg-white/10"
              onClick={() => {
                dispatchAction({
                  type: "SetMayTriggerAutoChoice",
                  data: { op: { type: "ClearAll" } },
                });
                close();
              }}
            >
              {t("mayTriggerAutoChoice.clearAll")}
            </button>
          </div>
          <div className="mx-2 mb-1 border-t border-white/10" />
          <ul className="flex flex-col">
            {choices.map((record) => {
              const sourceName =
                objects?.[record.key.source_id]?.name ??
                t("mayTriggerAutoChoice.sourceFallback");
              const decision =
                record.choice.type === "Accept"
                  ? t("mayTriggerAutoChoice.accept")
                  : t("mayTriggerAutoChoice.decline");
              return (
                <li
                  key={rowKey(record.key)}
                  className="flex items-center justify-between gap-2 px-3 py-1.5"
                >
                  <span className="truncate text-sm text-gray-200">
                    {t("mayTriggerAutoChoice.entryLabel", {
                      source: sourceName,
                      decision,
                    })}
                  </span>
                  <button
                    type="button"
                    className="shrink-0 rounded px-1.5 py-0.5 text-xs font-semibold text-sky-200 transition-colors hover:bg-white/10"
                    onClick={() =>
                      dispatchAction({
                        type: "SetMayTriggerAutoChoice",
                        data: { op: { type: "Remove", data: { key: record.key } } },
                      })
                    }
                  >
                    {t("mayTriggerAutoChoice.remove")}
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
