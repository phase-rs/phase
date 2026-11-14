import { useState } from "react";
import { useTranslation } from "react-i18next";

import {
  packId,
  type CatalogSummary,
  type CuratedDrift,
  type DeckLibraryDrift,
  type InstalledPack,
  type PackId,
  type RemovalResponse,
  type VerificationResponse,
} from "../../../services/visualPacks/types.ts";
import { formatByteSize } from "../../../utils/byteSize.ts";
import { packLabel, shortDigest } from "./packLabels.ts";
import { hasPendingVisualPackMutation, localMembershipDriftState } from "./useVisualPackManager.ts";

const CURATED = packId("curated");
const DECK_LIBRARY = packId("deck_library");

/**
 * Whether an installed pack is behind what the panel could install now.
 *
 * For every bulk pack that is a catalog-identity question, and the comparison
 * below answers it: the pack was installed from a Scryfall snapshot, and a
 * newer snapshot means a newer pack.
 *
 * The curated pack is not in that namespace at all. Its `catalogRoot` IS its
 * membership digest, so `entry.catalogRoot !== summary.catalogRoot` compares a
 * membership fingerprint against a snapshot hash. Those are sha256s of two
 * different things, so short of a collision they differ for every curated
 * install, always: the badge it lit was a constant, and a constant badge tells
 * a user nothing except to distrust the badge.
 *
 * The question that means the same thing for curated is drift, and it is asked
 * through the ONE predicate every surface asks it through — a badge and a
 * selector that spelled it out separately had already disagreed about
 * `installedDigest: null`, which means nothing is installed rather than
 * everything has changed. `unknown` shows nothing: an unmeasured claim here
 * would be a claim that a multi-gigabyte download is outstanding.
 */
function upgradeAvailable(
  entry: InstalledPack,
  summary: CatalogSummary,
  curatedDrift: CuratedDrift | null,
  deckLibraryDrift: DeckLibraryDrift | null,
): boolean {
  if (entry.packId === CURATED) return localMembershipDriftState(summary, CURATED, curatedDrift) === "drifted";
  if (entry.packId === DECK_LIBRARY) return localMembershipDriftState(summary, DECK_LIBRARY, deckLibraryDrift) === "drifted";
  return entry.catalogRoot !== summary.catalogRoot;
}

interface PackStatusProps {
  summary: CatalogSummary;
  curatedDrift: CuratedDrift | null;
  deckLibraryDrift: DeckLibraryDrift | null;
  verification: VerificationResponse | null;
  removal: RemovalResponse | null;
  pendingActions: ReadonlySet<string>;
  durableMutationActive: boolean;
  networkActionsDisabled: boolean;
  onVerify(mode: "metadata" | "full"): void;
  onRepair(ids: PackId[]): void;
  onRemoveSelected(ids: PackId[], launcher: HTMLButtonElement): void;
  onRemoveComplete(launcher: HTMLButtonElement): void;
  onRemoveAll(launcher: HTMLButtonElement): void;
}

export function PackStatus({
  summary,
  curatedDrift,
  deckLibraryDrift,
  verification,
  removal,
  pendingActions,
  durableMutationActive,
  networkActionsDisabled,
  onVerify,
  onRepair,
  onRemoveSelected,
  onRemoveComplete,
  onRemoveAll,
}: PackStatusProps) {
  const { t, i18n } = useTranslation("settings");
  const [selected, setSelected] = useState<Set<PackId>>(new Set());
  const selectedIds = summary.installedPacks.map((entry) => entry.packId).filter((id) => selected.has(id));
  const mutationPending = hasPendingVisualPackMutation(pendingActions);
  return (
    <section className="flex flex-col gap-3 rounded-[16px] border border-white/10 p-3">
      <h4 className="text-sm font-semibold text-slate-100">{t("visualPacks.status.title")}</h4>
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 text-xs text-slate-300">
        <dt>{t("visualPacks.status.root")}</dt><dd className="break-all font-mono">{shortDigest(summary.catalogRoot)}</dd>
        <dt>{t("visualPacks.status.revision")}</dt><dd className="break-all font-mono">{summary.installedRevision}</dd>
        {/* Omitted rather than shown as zero when the browser will not say —
            `null` means unknown, and a "0 B" here would read as "nothing
            stored". Same rule PackSelector applies to `availableBytes`.

            The figure is the ORIGIN's usage, not this feature's: it counts
            every byte this site keeps, offline images among them. The label
            says site, and must keep saying site — attributing all of it to the
            visual packs would be the display layer inventing a breakdown the
            engine never measured. */}
        {summary.storage.usageBytes !== null && (
          <>
            <dt>{t("visualPacks.status.storageUsage")}</dt>
            <dd className="tabular-nums">{formatByteSize(summary.storage.usageBytes, i18n.language)}</dd>
          </>
        )}
        {/* Rendered for all three arms, `unsupported` included: "this browser
            will not say" is an answer a user managing offline downloads needs,
            and dropping the row would let silence read as a grant. */}
        <dt>{t("visualPacks.status.persistence")}</dt>
        <dd className={summary.storage.persistence === "best_effort" ? "text-amber-300" : undefined}>
          {t(`visualPacks.status.persistenceState.${summary.storage.persistence}`)}
        </dd>
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
              {packLabel(entry.packId, t)}
              {upgradeAvailable(entry, summary, curatedDrift, deckLibraryDrift) && <span className="ml-2 text-amber-300">{t("visualPacks.status.upgradeAvailable")}</span>}
              <span className="mt-1 block text-slate-500">
                {/* Curated is stored under its own membership digest, so
                    "installed from snapshot" would name a Scryfall snapshot it
                    was never built from. */}
                {entry.packId === CURATED || entry.packId === DECK_LIBRARY
                  ? t("visualPacks.status.membershipDigest", { digest: shortDigest(entry.catalogRoot) })
                  : t("visualPacks.status.receiptRoot", { root: shortDigest(entry.catalogRoot) })}
              </span>
            </span>
          </label>
        ))}
      </fieldset>
      <div className="flex flex-wrap gap-2">
        <button type="button" disabled={pendingActions.has("verify:metadata")} onClick={() => onVerify("metadata")} className="min-h-11 rounded-[12px] border border-white/15 px-3 text-sm text-slate-100 disabled:opacity-40">{t("visualPacks.actions.verifyMetadata")}</button>
        <button type="button" disabled={pendingActions.has("verify:full")} onClick={() => onVerify("full")} className="min-h-11 rounded-[12px] border border-white/15 px-3 text-sm text-slate-100 disabled:opacity-40">{t("visualPacks.actions.verifyFull")}</button>
        <button type="button" disabled={networkActionsDisabled || durableMutationActive || mutationPending || selectedIds.length === 0} onClick={() => onRepair(selectedIds)} className="min-h-11 rounded-[12px] border border-sky-400/40 px-3 text-sm text-sky-100 disabled:opacity-40">{t("visualPacks.actions.repair")}</button>
        <button type="button" disabled={durableMutationActive || mutationPending || selectedIds.length === 0} onClick={(event) => onRemoveSelected(selectedIds, event.currentTarget)} className="min-h-11 rounded-[12px] border border-rose-400/40 px-3 text-sm text-rose-100 disabled:opacity-40">{t("visualPacks.actions.removeSelected")}</button>
        <button type="button" disabled={durableMutationActive || mutationPending} onClick={(event) => onRemoveComplete(event.currentTarget)} className="min-h-11 rounded-[12px] border border-rose-400/40 px-3 text-sm text-rose-100 disabled:opacity-40">{t("visualPacks.actions.removeComplete")}</button>
        <button type="button" disabled={durableMutationActive || mutationPending} onClick={(event) => onRemoveAll(event.currentTarget)} className="min-h-11 rounded-[12px] border border-rose-400/40 px-3 text-sm text-rose-100 disabled:opacity-40">{t("visualPacks.actions.removeAll")}</button>
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
      {pendingActions.has("remove") && (
        <p aria-live="polite" className="text-xs text-slate-300">{t("visualPacks.removal.inProgress")}</p>
      )}
    </section>
  );
}
