import { useTranslation } from "react-i18next";

import type {
  RemovalSelector,
  VisualPackErrorKind,
} from "../../../services/visualPacks/types.ts";
import { ConfirmDialog } from "../../ui/ConfirmDialog.tsx";
import { OperationProgress } from "./OperationProgress.tsx";
import { PackSelector } from "./PackSelector.tsx";
import { PackStatus } from "./PackStatus.tsx";
import {
  useVisualPackManager,
  type FrozenConfirmation,
} from "./useVisualPackManager.ts";

function errorKey(kind: VisualPackErrorKind): string {
  switch (kind) {
    case "unsupported_shell": return "visualPacks.errors.unsupported_shell";
    case "unauthorized": return "visualPacks.errors.unauthorized";
    case "unavailable": return "visualPacks.errors.unavailable";
    case "invalid_input": return "visualPacks.errors.invalid_input";
    case "conflict": return "visualPacks.errors.conflict";
    case "cancelled": return "visualPacks.errors.cancelled";
    case "network": return "visualPacks.errors.network";
    case "storage": return "visualPacks.errors.storage";
    case "trust": return "visualPacks.errors.trust";
    case "emit": return "visualPacks.errors.emit";
    case "internal": return "visualPacks.errors.internal";
  }
}

function selectorLabel(selector: RemovalSelector): string {
  switch (selector.kind) {
    case "packs": return selector.packIds.join(", ");
    case "complete": return selector.rootSha256;
    case "all_installed": return "";
  }
}

function confirmationKeys(confirmation: FrozenConfirmation): { title: string; message: string; action: string } {
  switch (confirmation.kind) {
    case "cascade":
      return {
        title: "visualPacks.confirm.cascadeTitle",
        message: "visualPacks.confirm.cascadeMessage",
        action: "visualPacks.confirm.cascadeAction",
      };
    case "complete":
      return {
        title: "visualPacks.confirm.completeTitle",
        message: "visualPacks.confirm.completeMessage",
        action: "visualPacks.confirm.removeAction",
      };
    case "all":
      return {
        title: "visualPacks.confirm.allTitle",
        message: "visualPacks.confirm.allMessage",
        action: "visualPacks.confirm.removeAction",
      };
  }
}

export function VisualPackManager() {
  const { t } = useTranslation("settings");
  const manager = useVisualPackManager();
  const confirmationCopy = manager.confirmation ? confirmationKeys(manager.confirmation) : null;

  return (
    <section className="rounded-[20px] border border-white/10 bg-black/18 p-4 shadow-[0_18px_54px_rgba(0,0,0,0.18)] backdrop-blur-md sm:p-5">
      <div className="mb-4">
        <h3 className="text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-slate-500">
          {t("visualPacks.title")}
        </h3>
        <p className="mt-2 text-xs leading-relaxed text-slate-400">{t("visualPacks.description")}</p>
      </div>
      <div aria-live="polite" className="flex flex-col gap-4">
        {manager.availability.kind === "loading" && <p className="text-sm text-slate-300">{t("visualPacks.availability.loading")}</p>}
        {manager.availability.kind === "browser_unavailable" && (
          <div className="space-y-2 text-sm text-slate-300">
            <p>{t("visualPacks.availability.browser")}</p>
            <p className="text-xs text-slate-400">{t("visualPacks.availability.browserFuture")}</p>
          </div>
        )}
        {manager.availability.kind === "unsupported_shell" && (
          <p className="text-sm text-amber-200">{t("visualPacks.availability.unsupported")}</p>
        )}
        {manager.availability.kind === "transient_failure" && (
          <div className="space-y-2">
            <p role="alert" className="text-sm text-rose-200">{t(errorKey(manager.availability.error) as never)}</p>
            <button type="button" onClick={manager.retry} className="min-h-11 rounded-[12px] border border-sky-400/50 px-4 text-sm text-sky-100">
              {t("visualPacks.actions.retry")}
            </button>
          </div>
        )}
        {(manager.availability.kind === "empty" || manager.availability.kind === "invalid") && (
          <div className="space-y-2">
            <p className="text-sm text-amber-200">
              {t(manager.availability.kind === "empty" ? "visualPacks.availability.empty" : "visualPacks.availability.invalid")}
            </p>
            <button type="button" disabled={manager.pendingActions.has("refresh")} onClick={manager.refresh} className="min-h-11 rounded-[12px] border border-sky-400/50 px-4 text-sm text-sky-100 disabled:opacity-40">
              {manager.pendingActions.has("refresh") ? t("visualPacks.actions.refreshing") : t("visualPacks.actions.refresh")}
            </button>
          </div>
        )}
        {manager.availability.kind === "ready" && manager.summary && (
          <>
            <button type="button" disabled={manager.pendingActions.has("refresh")} onClick={manager.refresh} className="min-h-11 self-start rounded-[12px] border border-sky-400/50 px-4 text-sm text-sky-100 disabled:opacity-40">
              {manager.pendingActions.has("refresh") ? t("visualPacks.actions.refreshing") : t("visualPacks.actions.refresh")}
            </button>
            {manager.operation && (
              <OperationProgress
                operation={manager.operation}
                progressPhase={manager.progress?.phase ?? null}
                pendingActions={manager.pendingActions}
                onCancel={manager.cancel}
                onResume={manager.resume}
              />
            )}
            {manager.actionError && (
              <p role="alert" className="rounded-[12px] border border-rose-400/25 bg-rose-400/[0.08] px-3 py-2 text-sm text-rose-100">
                {t(errorKey(manager.actionError) as never)}
              </p>
            )}
            <PackSelector
              summary={manager.summary}
              estimate={manager.estimate}
              pendingActions={manager.pendingActions}
              durableMutationActive={manager.durableMutationActive}
              onEstimate={manager.estimateInstall}
              onInstall={manager.install}
            />
            <PackStatus
              summary={manager.summary}
              verification={manager.verification?.value ?? null}
              removal={manager.removal}
              pendingActions={manager.pendingActions}
              durableMutationActive={manager.durableMutationActive}
              onVerify={manager.verify}
              onRepair={manager.repair}
              onRemoveSelected={manager.removeSelected}
              onRemoveComplete={manager.removeComplete}
              onRemoveAll={manager.removeAll}
            />
          </>
        )}
        {manager.actionError && manager.availability.kind !== "ready" && (
          <p role="alert" className="text-sm text-rose-200">{t(errorKey(manager.actionError) as never)}</p>
        )}
      </div>
      <ConfirmDialog
        open={manager.confirmation != null}
        title={confirmationCopy ? t(confirmationCopy.title as never) : ""}
        message={manager.confirmation && confirmationCopy
          ? t(confirmationCopy.message as never, { selection: selectorLabel(manager.confirmation.selector) })
          : ""}
        confirmLabel={confirmationCopy ? t(confirmationCopy.action as never) : ""}
        onConfirm={manager.confirmRemoval}
        onCancel={manager.dismissConfirmation}
        tone="danger"
      />
    </section>
  );
}
