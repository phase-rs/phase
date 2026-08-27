import { useTranslation } from "react-i18next";

import type { OperationStatus, ProgressEvent } from "../../../services/visualPacks/types.ts";

interface OperationProgressProps {
  operation: OperationStatus;
  progressPhase: ProgressEvent["phase"] | null;
  pendingActions: ReadonlySet<string>;
  onCancel(): void;
  onResume(): void;
}

export function OperationProgress({ operation, progressPhase, pendingActions, onCancel, onResume }: OperationProgressProps) {
  const { t } = useTranslation("settings");
  const startPending = pendingActions.has("install") || pendingActions.has("repair") || pendingActions.has("resume");
  const canCancel = operation.state === "downloading" && progressPhase !== "failed";
  const canResume = operation.state === "downloading" && progressPhase === "failed";
  const displayedState = progressPhase === "failed" ? "failed" : operation.state;
  return (
    <section aria-label={t("visualPacks.operation.title")} className="rounded-[16px] border border-sky-400/30 bg-sky-400/[0.07] p-3 sm:p-4">
      <div>
        <h4 className="text-sm font-semibold text-sky-50">{t("visualPacks.operation.title")}</h4>
        <p aria-live="polite" className="mt-1 text-sm text-sky-100">
          {t(`visualPacks.operation.states.${displayedState}`)}
        </p>
      </div>
      <dl className="mt-3 grid grid-cols-[auto_minmax(0,1fr)] gap-x-2 gap-y-1 text-xs text-sky-100">
        <dt className="text-sky-200/70">{t("visualPacks.operation.id")}</dt><dd className="break-all font-mono">{operation.operationId}</dd>
        <dt className="text-sky-200/70">{t("visualPacks.operation.root")}</dt><dd className="break-all font-mono">{operation.catalogRoot}</dd>
      </dl>
      <label className="mt-4 block text-xs text-sky-50">
        <span className="flex items-center justify-between gap-3">
          <span>{t("visualPacks.operation.packs", { current: operation.packsPromoted, total: operation.packTotal })}</span>
          <span className="font-medium tabular-nums">{operation.packsPromoted}/{operation.packTotal}</span>
        </span>
        <progress className="mt-1.5 block h-2 w-full accent-sky-400" value={operation.packsPromoted} max={Math.max(operation.packTotal, 1)} />
      </label>
      <label className="mt-3 block text-xs text-sky-50">
        <span className="flex items-center justify-between gap-3">
          <span>{t("visualPacks.operation.objects", { current: operation.objectsPromoted, total: operation.objectTotal })}</span>
          <span className="font-medium tabular-nums">{operation.objectsPromoted}/{operation.objectTotal}</span>
        </span>
        <progress className="mt-1.5 block h-2 w-full accent-sky-400" value={operation.objectsPromoted} max={Math.max(operation.objectTotal, 1)} />
      </label>
      <p className="mt-3 text-xs leading-relaxed text-sky-100/80">{t("visualPacks.operation.scanNote")}</p>
      {operation.state === "finalizing" && <progress aria-label={t("visualPacks.operation.finalizing")} className="mt-4 block h-2 w-full accent-sky-400" />}
      <div className="mt-3 flex flex-wrap gap-2">
        {canCancel && (
          <button type="button" disabled={startPending || pendingActions.has("cancel")} onClick={onCancel} className="min-h-11 rounded-[12px] border border-rose-400/40 px-4 text-sm text-rose-100 disabled:opacity-40">
            {t("visualPacks.actions.cancel")}
          </button>
        )}
        {canResume && (
          <button type="button" disabled={startPending} onClick={onResume} className="min-h-11 rounded-[12px] border border-sky-400/50 px-4 text-sm text-sky-100 disabled:opacity-40">
            {t("visualPacks.actions.resume")}
          </button>
        )}
      </div>
    </section>
  );
}
