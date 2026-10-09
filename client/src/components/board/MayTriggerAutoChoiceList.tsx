import { useTranslation } from "react-i18next";

import type { MayTriggerAutoChoiceSelector } from "../../adapter/types.ts";
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
 * actor scoping on the write, and each remove echoes the stored selector verbatim.
 */
export function MayTriggerAutoChoiceList() {
  const { t } = useTranslation("game");
  const choices = useGameStore((s) => s.gameState?.may_trigger_auto_choices) ?? [];
  const replacementChoices = useGameStore((s) => s.gameState?.replacement_auto_choices) ?? [];
  const objects = useGameStore((s) => s.gameState?.objects);

  if (choices.length + replacementChoices.length === 0) return null;

  const rowKey = (selector: MayTriggerAutoChoiceSelector) => JSON.stringify(selector);

  return (
    <PopoverMenu
      ariaLabel={t("mayTriggerAutoChoice.listHeader")}
      variant="dialog"
      menuWidthPx={480}
      renderTrigger={({ ref, open, toggle }) => (
        <button
          ref={ref}
          type="button"
          aria-haspopup="dialog"
          aria-expanded={open}
          onClick={toggle}
          className={`pointer-events-auto flex items-center gap-1.5 rounded-full px-2.5 py-1 text-[11px] font-semibold shadow-sm ring-1 transition-colors ${
            open
              ? "bg-sky-400 text-black ring-sky-300"
              : "bg-sky-500/90 text-black ring-sky-300/80 hover:bg-sky-400"
          }`}
        >
          <span>{t("mayTriggerAutoChoice.menuButtonShortActive")}</span>
          <span className="rounded-full bg-black/25 px-1.5 leading-tight">{choices.length + replacementChoices.length}</span>
        </button>
      )}
    >
      {(close) => (
        <>
          <div className="sticky top-0 z-10 flex shrink-0 items-center justify-between gap-2 border-b border-white/10 bg-[#0a0f1b] px-3 pb-1.5 pt-1">
            <span className="text-sm font-bold text-white">
              {t("mayTriggerAutoChoice.listHeader")}
            </span>
            <div className="flex shrink-0 items-center gap-2">
              <button
                type="button"
                className="rounded px-1.5 py-0.5 text-xs font-semibold text-sky-200 transition-colors hover:bg-white/10"
                onClick={() => {
                  dispatchAction({
                    type: "SetMayTriggerAutoChoice",
                    data: { op: { type: "ClearAll" } },
                  });
                  dispatchAction({ type: "SetReplacementAutoChoice", data: { selector: null } });
                  close();
                }}
              >
                {t("mayTriggerAutoChoice.clearAll")}
              </button>
              <button
                type="button"
                aria-label={t("common:actions.close")}
                className="rounded px-1.5 py-0.5 text-sm text-gray-200 transition-colors hover:bg-white/10"
                onClick={close}
              >
                <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true" className="h-4 w-4">
                  <path d="m4 4 8 8M12 4l-8 8" />
                </svg>
              </button>
            </div>
          </div>
          {choices.length > 0 && (
            <section className="shrink-0">
              <h3 className="px-3 pb-1 pt-2 text-xs font-semibold text-sky-200">
                {t("mayTriggerAutoChoice.optionalTriggers", { count: choices.length })}
              </h3>
              <ul className="flex flex-col">
                {choices.map((record) => {
                  const sourceName =
                    (record.selector.type === "ExactInstance"
                      ? objects?.[record.selector.data.source_id]?.name
                      : record.selector.data.printed_ref.face_name) ??
                    t("mayTriggerAutoChoice.sourceFallback");
                  const decision =
                    record.choice.type === "Accept"
                      ? t("mayTriggerAutoChoice.accept")
                      : t("mayTriggerAutoChoice.decline");
                  return (
                    <li
                      key={rowKey(record.selector)}
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
                            data: { op: { type: "Remove", data: { selector: record.selector } } },
                          })
                        }
                      >
                        {t("mayTriggerAutoChoice.remove")}
                      </button>
                    </li>
                  );
                })}
              </ul>
            </section>
          )}
          {replacementChoices.length > 0 && (
            <section className="shrink-0">
              <h3 className="px-3 pb-1 pt-2 text-xs font-semibold text-sky-200">
                {t("mayTriggerAutoChoice.replacementChoices", { count: replacementChoices.length })}
              </h3>
              <ul className="flex flex-col">
                {replacementChoices.map((record) => (
                  <li key={record.id} className="flex items-start gap-2 px-3 py-1.5">
                    <details className="group min-w-0 flex-1">
                      <summary className="flex cursor-pointer list-none items-start gap-1.5 text-sm text-gray-200">
                        <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true" className="mt-0.5 h-4 w-4 shrink-0 group-open:rotate-90">
                          <path d="m6 3 5 5-5 5" />
                        </svg>
                        <span className="line-clamp-2 min-w-0 break-words">
                          {t("replacement.savedChoice", { choice: record.descriptions.join(" → ") })}
                        </span>
                      </summary>
                      <ol className="ml-5 list-decimal space-y-1 break-words py-2 text-sm text-gray-200">
                        {record.descriptions.map((description, index) => (
                          <li key={index}>{description}</li>
                        ))}
                      </ol>
                    </details>
                    <button
                      type="button"
                      className="shrink-0 rounded px-1.5 py-0.5 text-xs font-semibold text-sky-200 transition-colors hover:bg-white/10"
                      onClick={() => dispatchAction({ type: "SetReplacementAutoChoice", data: { selector: record.id } })}
                    >
                      {t("mayTriggerAutoChoice.remove")}
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          )}
        </>
      )}
    </PopoverMenu>
  );
}
