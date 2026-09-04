import type { TFunction } from "i18next";
import { useRef } from "react";
import { useTranslation } from "react-i18next";

import type {
  RemovalSelector,
  VisualPackErrorKind,
} from "../../../services/visualPacks/types.ts";
import { formatByteSize } from "../../../utils/byteSize.ts";
import { ConfirmDialog } from "../../ui/ConfirmDialog.tsx";
import { OperationProgress } from "./OperationProgress.tsx";
import { packLabel, shortDigest } from "./packLabels.ts";
import { PackSelector } from "./PackSelector.tsx";
import { PackStatus } from "./PackStatus.tsx";
import {
  useVisualPackManager,
  type FrozenConfirmation,
} from "./useVisualPackManager.ts";
import { useEffectiveOffline } from "../../../stores/connectivityStore.ts";

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
    case "insufficient_storage": return "visualPacks.errors.insufficient_storage";
    case "trust": return "visualPacks.errors.trust";
    case "emit": return "visualPacks.errors.emit";
    case "internal": return "visualPacks.errors.internal";
  }
}

/** What the confirmation sentence names, in the words the rest of the panel
 *  uses for the same packs — a removal prompt is the last place to quote wire
 *  identities at somebody. */
function selectorLabel(selector: RemovalSelector, t: TFunction<"settings">): string {
  switch (selector.kind) {
    case "packs": return selector.packIds.map((id) => packLabel(id, t)).join(", ");
    case "complete": return shortDigest(selector.rootSha256);
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
  const { t, i18n } = useTranslation("settings");
  const networkActionsDisabled = useEffectiveOffline();
  const manager = useVisualPackManager();
  const confirmationLauncherRef = useRef<HTMLButtonElement>(null);
  const durableFocusRef = useRef<HTMLHeadingElement>(null);
  const confirmationCopy = manager.confirmation ? confirmationKeys(manager.confirmation) : null;
  const refusal = manager.actionErrorRefusal;
  // One message for both places the panel reports a failed action. The figures
  // are the backend's own — the ones its pre-flight gate compared — and only
  // their unit and separators are decided here.
  const actionErrorMessage = manager.actionError
    ? t(errorKey(manager.actionError) as never, refusal
      ? {
          required: formatByteSize(refusal.requiredBytes, i18n.language),
          available: formatByteSize(refusal.availableBytes, i18n.language),
        }
      : {})
    : null;

  return (
    <section className="rounded-[20px] border border-white/10 bg-black/18 p-4 shadow-[0_18px_54px_rgba(0,0,0,0.18)] backdrop-blur-md sm:p-5">
      <div className="mb-4">
        <h3
          ref={durableFocusRef}
          tabIndex={-1}
          className="text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-slate-500"
        >
          {t("visualPacks.title")}
        </h3>
        <p className="mt-2 text-xs leading-relaxed text-slate-400">{t("visualPacks.description")}</p>
        {networkActionsDisabled && <p className="mt-2 text-xs leading-relaxed text-amber-200">{t("visualPacks.offlineUnavailable")}</p>}
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
            <button type="button" disabled={networkActionsDisabled || manager.pendingActions.has("refresh")} onClick={manager.refresh} className="min-h-11 rounded-[12px] border border-sky-400/50 px-4 text-sm text-sky-100 disabled:opacity-40">
              {manager.pendingActions.has("refresh") ? t("visualPacks.actions.refreshing") : t("visualPacks.actions.refresh")}
            </button>
          </div>
        )}
        {manager.availability.kind === "ready" && manager.summary && (
          <>
            <button type="button" disabled={networkActionsDisabled || manager.pendingActions.has("refresh")} onClick={manager.refresh} className="min-h-11 self-start rounded-[12px] border border-sky-400/50 px-4 text-sm text-sky-100 disabled:opacity-40">
              {manager.pendingActions.has("refresh") ? t("visualPacks.actions.refreshing") : t("visualPacks.actions.refresh")}
            </button>
            {manager.operation && (
              <OperationProgress
                operation={manager.operation}
                progressPhase={manager.progress?.phase ?? null}
                pendingActions={manager.pendingActions}
                networkActionsDisabled={networkActionsDisabled}
                onCancel={manager.cancel}
                onResume={manager.resume}
              />
            )}
            {manager.actionError && (
              <div role="alert" className="rounded-[12px] border border-rose-400/25 bg-rose-400/[0.08] px-3 py-2 text-sm text-rose-100">
                <p>{actionErrorMessage}</p>
                {manager.actionErrorDetail && (
                  <code className="mt-1 block select-text break-words font-mono text-xs text-rose-100/90">
                    {manager.actionErrorDetail}
                  </code>
                )}
              </div>
            )}
            <PackSelector
              summary={manager.summary}
              curatedSelector={manager.curatedSelector}
              deckLibrarySelector={manager.deckLibrarySelector}
              curatedDrift={manager.curatedDrift}
              deckLibraryDrift={manager.deckLibraryDrift}
              estimate={manager.estimate}
              estimateProgress={manager.estimateProgress}
              pendingActions={manager.pendingActions}
              durableMutationActive={manager.durableMutationActive}
              networkActionsDisabled={networkActionsDisabled}
              onSelectCurated={manager.resolveCuratedSelector}
              onSelectDeckLibrary={manager.resolveDeckLibrarySelector}
              onEstimate={manager.estimateInstall}
              onInstall={manager.install}
            />
            <PackStatus
              summary={manager.summary}
              curatedDrift={manager.curatedDrift}
              deckLibraryDrift={manager.deckLibraryDrift}
              verification={manager.verification?.value ?? null}
              removal={manager.removal}
              pendingActions={manager.pendingActions}
              durableMutationActive={manager.durableMutationActive}
              networkActionsDisabled={networkActionsDisabled}
              onVerify={manager.verify}
              onRepair={manager.repair}
              onRemoveSelected={(ids, launcher) => {
                confirmationLauncherRef.current = launcher;
                // Non-cascading selections remove immediately. Move focus off
                // the launcher before that asynchronous mutation disables it;
                // a cascade confirmation still restores to the explicit
                // launcher when cancelled.
                durableFocusRef.current?.focus();
                manager.removeSelected(ids);
              }}
              onRemoveComplete={(launcher) => {
                confirmationLauncherRef.current = launcher;
                manager.removeComplete();
              }}
              onRemoveAll={(launcher) => {
                confirmationLauncherRef.current = launcher;
                manager.removeAll();
              }}
            />
          </>
        )}
        {manager.actionError && manager.availability.kind !== "ready" && (
          <div role="alert" className="text-sm text-rose-200">
            <p>{actionErrorMessage}</p>
            {manager.actionErrorDetail && (
              <code className="mt-1 block select-text break-words font-mono text-xs text-rose-100/90">
                {manager.actionErrorDetail}
              </code>
            )}
          </div>
        )}
      </div>
      <ConfirmDialog
        open={manager.confirmation != null}
        title={confirmationCopy ? t(confirmationCopy.title as never) : ""}
        message={manager.confirmation && confirmationCopy
          ? t(confirmationCopy.message as never, { selection: selectorLabel(manager.confirmation.selector, t) })
          : ""}
        confirmLabel={confirmationCopy ? t(confirmationCopy.action as never) : ""}
        onConfirm={() => {
          // A successful removal may disable its launcher. Move focus to a
          // durable section landmark before the nested scope unmounts.
          durableFocusRef.current?.focus();
          manager.confirmRemoval();
        }}
        onCancel={manager.dismissConfirmation}
        tone="danger"
        returnFocusRef={confirmationLauncherRef}
      />
    </section>
  );
}
