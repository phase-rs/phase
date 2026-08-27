import { useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  CatalogSummary,
  PackId,
  RemovalResponse,
  VerificationResponse,
} from "../../../services/visualPacks/types.ts";
import { hasPendingVisualPackMutation } from "./useVisualPackManager.ts";

interface PackStatusProps {
  summary: CatalogSummary;
  verification: VerificationResponse | null;
  removal: RemovalResponse | null;
  pendingActions: ReadonlySet<string>;
  durableMutationActive: boolean;
  onVerify(mode: "metadata" | "full"): void;
  onRepair(ids: PackId[]): void;
  onRemoveSelected(ids: PackId[]): void;
  onRemoveComplete(): void;
  onRemoveAll(): void;
}

export function PackStatus({
  summary,
  verification,
  removal,
  pendingActions,
  durableMutationActive,
  onVerify,
  onRepair,
  onRemoveSelected,
  onRemoveComplete,
  onRemoveAll,
}: PackStatusProps) {
  const { t } = useTranslation("settings");
  const [selected, setSelected] = useState<Set<PackId>>(new Set());
  const selectedIds = summary.installedPacks.map((entry) => entry.packId).filter((id) => selected.has(id));
  const mutationPending = hasPendingVisualPackMutation(pendingActions);
  return (
    <section className="flex flex-col gap-3 rounded-[16px] border border-white/10 p-3">
      <h4 className="text-sm font-semibold text-slate-100">{t("visualPacks.status.title")}</h4>
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 text-xs text-slate-300">
        <dt>{t("visualPacks.status.root")}</dt><dd className="break-all font-mono">{summary.catalogRoot}</dd>
        <dt>{t("visualPacks.status.revision")}</dt><dd className="break-all font-mono">{summary.installedRevision}</dd>
      </dl>
      <fieldset className="flex flex-col gap-2">
        <legend className="text-xs font-semibold text-slate-300">{t("visualPacks.status.installed")}</legend>
        {summary.installedPacks.length === 0 && <p className="text-xs text-slate-500">{t("visualPacks.status.noneInstalled")}</p>}
        {summary.installedPacks.map((entry) => (
          <label key={`${entry.catalogRoot}:${entry.packId}`} className="flex min-h-11 items-start gap-2 rounded-[12px] bg-black/20 px-3 py-2 text-xs text-slate-200">
            <input
              type="checkbox"
              checked={selected.has(entry.packId)}
              onChange={(event) => setSelected((current) => {
                const next = new Set(current);
                if (event.target.checked) next.add(entry.packId); else next.delete(entry.packId);
                return next;
              })}
            />
            <span className="min-w-0 break-all">
              {entry.packId}
              {entry.catalogRoot !== summary.catalogRoot && <span className="ml-2 text-amber-300">{t("visualPacks.status.upgradeAvailable")}</span>}
              <span className="mt-1 block text-slate-500">
                {t("visualPacks.status.receiptRoot", { root: entry.catalogRoot })}
              </span>
            </span>
          </label>
        ))}
      </fieldset>
      <div className="flex flex-wrap gap-2">
        <button type="button" disabled={pendingActions.has("verify:metadata")} onClick={() => onVerify("metadata")} className="min-h-11 rounded-[12px] border border-white/15 px-3 text-sm text-slate-100 disabled:opacity-40">{t("visualPacks.actions.verifyMetadata")}</button>
        <button type="button" disabled={pendingActions.has("verify:full")} onClick={() => onVerify("full")} className="min-h-11 rounded-[12px] border border-white/15 px-3 text-sm text-slate-100 disabled:opacity-40">{t("visualPacks.actions.verifyFull")}</button>
        <button type="button" disabled={durableMutationActive || mutationPending || selectedIds.length === 0} onClick={() => onRepair(selectedIds)} className="min-h-11 rounded-[12px] border border-sky-400/40 px-3 text-sm text-sky-100 disabled:opacity-40">{t("visualPacks.actions.repair")}</button>
        <button type="button" disabled={durableMutationActive || mutationPending || selectedIds.length === 0} onClick={() => onRemoveSelected(selectedIds)} className="min-h-11 rounded-[12px] border border-rose-400/40 px-3 text-sm text-rose-100 disabled:opacity-40">{t("visualPacks.actions.removeSelected")}</button>
        <button type="button" disabled={durableMutationActive || mutationPending} onClick={onRemoveComplete} className="min-h-11 rounded-[12px] border border-rose-400/40 px-3 text-sm text-rose-100 disabled:opacity-40">{t("visualPacks.actions.removeComplete")}</button>
        <button type="button" disabled={durableMutationActive || mutationPending} onClick={onRemoveAll} className="min-h-11 rounded-[12px] border border-rose-400/40 px-3 text-sm text-rose-100 disabled:opacity-40">{t("visualPacks.actions.removeAll")}</button>
      </div>
      {verification && (
        <div aria-live="polite" className="text-xs text-slate-300">
          <p>{t("visualPacks.verification.revision", { revision: verification.revision })}</p>
          {verification.issues.length === 0 ? <p>{t("visualPacks.verification.healthy")}</p> : (
            <ul className="list-disc pl-5">
              {verification.issues.map((issue, index) => <li key={`${issue.kind}:${index}`}>{t(`visualPacks.verification.issues.${issue.kind}`)}</li>)}
            </ul>
          )}
        </div>
      )}
      {removal && (
        <div aria-live="polite" className="text-xs text-slate-300">
          <p>{t("visualPacks.removal.committed", { revision: removal.revision })}</p>
          <ul className="list-disc pl-5">
            {removal.cleanupIssues.map((issue, index) => <li key={`${issue.kind}:${index}`}>{t(`visualPacks.cleanup.${issue.kind}`)}</li>)}
          </ul>
        </div>
      )}
    </section>
  );
}
