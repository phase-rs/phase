import { useTranslation } from "react-i18next";

import type { OperationStatus, ProgressEvent } from "../../../services/visualPacks/types.ts";
import { shortDigest } from "./packLabels.ts";

interface OperationProgressProps {
  operation: OperationStatus;
  progressPhase: ProgressEvent["phase"] | null;
  pendingActions: ReadonlySet<string>;
  networkActionsDisabled: boolean;
  onCancel(): void;
  onResume(): void;
}

export function OperationProgress({ operation, progressPhase, pendingActions, networkActionsDisabled, onCancel, onResume }: OperationProgressProps) {
  const { t } = useTranslation("settings");
  const imageTotal = operation.objectEstimate
    ?? (operation.state === "finalizing" || operation.state === "completed" ? operation.objectTotal : null);
  const startPending = pendingActions.has("install") || pendingActions.has("repair") || pendingActions.has("resume");
  const canCancel = operation.state === "downloading" && progressPhase !== "failed";
  /**
   * `finalizing` as well as `downloading`, because a failure there really is
   * resumable and the backend already treats it that way: `finish()` leaves the
   * record `finalizing` when its transaction rejects, that classifies as
   * `storage`, `storage` is retryable — and `create()`'s pending loop re-runs
   * every `downloading` OR `finalizing` record on the next launch. Offering
   * Resume only makes available now what would otherwise happen silently at
   * relaunch; `start({ kind: "resume" })` accepts a `finalizing` record on the
   * same path, and `run()` refuses a second worker for an operation already
   * running.
   */
  const canResume = (operation.state === "downloading" || operation.state === "finalizing")
    && progressPhase === "failed";
  /**
   * Three endings, not two, because the record and the event each know half of
   * one of them.
   *
   * `operation.state` alone cannot report a failure: the backend terminates a
   * non-retryable operation by writing `state: "cancelled"`, the same value a
   * user's Cancel writes, so rendering the record would show a conflict as a
   * cancellation the user never asked for. The event's `failed` phase is what
   * says otherwise, and it wins.
   *
   * Which of the two failure labels shows is keyed to `canResume` — the same
   * test that decides whether the button is there — and deliberately NOT to the
   * record's terminality. "Failed — ready to resume" is a sentence about a
   * control, so it must be true exactly when the control is: keying it to
   * terminality instead promises a Resume beside no Resume for a
   * `cancel_requested` record, which is not terminal and is not resumable
   * either. The bug this had was that `canResume`'s SET was wrong — it omitted
   * `finalizing` — and that is fixed above, at the definition, rather than
   * worked around here.
   */
  const displayedState = progressPhase === "failed" ? (canResume ? "failed" : "terminated") : operation.state;
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
        <dt className="text-sky-200/70">{t("visualPacks.operation.root")}</dt><dd className="break-all font-mono">{shortDigest(operation.catalogRoot)}</dd>
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
          <span>{imageTotal === null
            ? t("visualPacks.operation.objectsUnknown", { current: operation.objectsPromoted })
            : t("visualPacks.operation.objects", { current: operation.objectsPromoted, total: imageTotal })}</span>
          <span className="font-medium tabular-nums">{imageTotal === null ? operation.objectsPromoted : `${operation.objectsPromoted}/${imageTotal}`}</span>
        </span>
        {imageTotal === null
          ? <progress className="mt-1.5 block h-2 w-full accent-sky-400" />
          : <progress className="mt-1.5 block h-2 w-full accent-sky-400" value={operation.objectsPromoted} max={Math.max(imageTotal, 1)} />}
      </label>
      <p className="mt-3 text-xs leading-relaxed text-sky-100/80">
        {t(operation.objectEstimate === null ? "visualPacks.operation.scanNote" : "visualPacks.operation.estimateNote")}
      </p>
      {operation.state === "finalizing" && <progress aria-label={t("visualPacks.operation.finalizing")} className="mt-4 block h-2 w-full accent-sky-400" />}
      <div className="mt-3 flex flex-wrap gap-2">
        {canCancel && (
          <button type="button" disabled={startPending || pendingActions.has("cancel")} onClick={onCancel} className="min-h-11 rounded-[12px] border border-rose-400/40 px-4 text-sm text-rose-100 disabled:opacity-40">
            {t("visualPacks.actions.cancel")}
          </button>
        )}
        {canResume && (
          <button type="button" disabled={networkActionsDisabled || startPending} onClick={onResume} className="min-h-11 rounded-[12px] border border-sky-400/50 px-4 text-sm text-sky-100 disabled:opacity-40">
            {t("visualPacks.actions.resume")}
          </button>
        )}
      </div>
    </section>
  );
}
