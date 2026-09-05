import type { TFunction } from "i18next";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { useSetCatalog } from "../../../hooks/useSetSymbols.ts";
import {
  packId,
  type CatalogSummary,
  type CatalogScanProgress,
  type CuratedDrift,
  type CuratedInstallSelector,
  type DeckLibraryDrift,
  type DeckLibraryInstallSelector,
  type InstallEstimate,
  type InstallSelector,
} from "../../../services/visualPacks/types.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { formatByteSize } from "../../../utils/byteSize.ts";
import { packLabel } from "./packLabels.ts";
import { localMembershipDriftState } from "./useVisualPackManager.ts";

// Autonyms, matching the language selector in PreferencesModal.
const IMAGE_LOCALES = {
  en: "English",
  de: "Deutsch",
  es: "Español",
  fr: "Français",
  it: "Italiano",
  pt: "Português",
} as const;
const MUTATION_ACTIONS = new Set(["install", "cancel", "resume", "repair", "remove"]);
type ImageLocale = keyof typeof IMAGE_LOCALES;
type SelectorKind = "core" | "printing" | "complete" | "curated" | "deck_library";

const SELECTOR_KINDS = ["curated", "deck_library", "core", "printing", "complete"] as const;

const CURATED = packId("curated");
const DECK_LIBRARY = packId("deck_library");

/**
 * How long a curated selection settles before its estimate is requested.
 *
 * Curated is the one selector estimated without a click, so a user flicking
 * through the radios would otherwise start a membership plan for every one
 * they pass through. Short enough that a deliberate choice reads as immediate.
 */
const CURATED_ESTIMATE_DEBOUNCE_MS = 200;

/** The catalog-scan figures, which only a bulk selector has. */
const BULK_METRICS = ["assetRecords", "uniqueObjects", "logicalImageBytes", "uniqueImageBytes", "shardCount", "shardBytes"] as const;

interface PackSelectorProps {
  summary: CatalogSummary;
  curatedSelector: CuratedInstallSelector | null;
  deckLibrarySelector: DeckLibraryInstallSelector | null;
  curatedDrift: CuratedDrift | null;
  deckLibraryDrift: DeckLibraryDrift | null;
  estimate: { selector: InstallSelector; value: InstallEstimate } | null;
  estimateProgress: CatalogScanProgress | null;
  pendingActions: ReadonlySet<string>;
  durableMutationActive: boolean;
  networkActionsDisabled: boolean;
  onSelectCurated(): void;
  onSelectDeckLibrary(): void;
  onEstimate(selector: InstallSelector): void;
  onInstall(selector: InstallSelector): void;
}

function normalizedSet(value: string): string {
  return value.trim().normalize("NFC").toLowerCase();
}

function validSelector(
  kind: SelectorKind,
  setInput: string,
  language: ImageLocale,
  summary: CatalogSummary,
  curated: CuratedInstallSelector | null,
  deckLibrary: DeckLibraryInstallSelector | null,
): InstallSelector | null {
  const set = normalizedSet(setInput);
  try {
    switch (kind) {
      case "core":
        return { kind: "core" };
      case "printing":
        if (language === "en") {
          packId(`printing:${set}`);
          return { kind: "printing", set };
        }
        packId(`locale:${language}:${set}`);
        return { kind: "locale", language, set };
      case "complete":
        return { kind: "complete", rootSha256: summary.catalogRoot };
      case "curated":
        // Not composed here. The digest names a membership only the planner
        // can compute, so the backend resolves it and this option merely
        // reports what came back — null until it has.
        return curated;
      case "deck_library":
        return deckLibrary;
    }
  } catch {
    return null;
  }
}

/** A `shardBytes` figure, which is a byte count when it is a number at all and
 *  the literal `"unknown"` when the selector reads no shard. */
function formatCatalogSize(value: string, locale: string): string {
  const bytes = Number(value);
  return Number.isFinite(bytes) ? formatByteSize(bytes, locale) : value;
}

/**
 * The figures worth showing for the selector this estimate was taken for.
 *
 * The list is selector-aware because the bulk figures are not merely
 * uninteresting for a curated pack, they are FALSE about it: it reads no shard
 * of the Scryfall archive, so "Metadata files: 0" and "Compressed Scryfall
 * catalog: unknown" describe an archive it never opens. Its own two record
 * counts are the same number by construction, so one of them is shown.
 *
 * The download size and the free space the browser reports are common to every
 * selector — they are the two numbers the Install decision is actually made on.
 */
function estimateRows(
  estimate: InstallEstimate,
  localMembership: boolean,
  locale: string,
  t: TFunction<"settings">,
): Array<{ key: string; label: string; value: string }> {
  const count = new Intl.NumberFormat(locale);
  const rows = localMembership
    ? [{ key: "uniqueObjects", label: t("visualPacks.metrics.uniqueObjects"), value: count.format(Number(estimate.uniqueObjects)) }]
    : BULK_METRICS.map((metric) => ({
      key: metric,
      label: t(`visualPacks.metrics.${metric}`),
      value: metric === "shardBytes" ? formatCatalogSize(estimate[metric], locale) : estimate[metric],
    }));
  rows.push({
    key: "estimatedImageBytes",
    label: t("visualPacks.metrics.estimatedImageBytes"),
    value: formatByteSize(estimate.estimatedImageBytes, locale),
  });
  // Omitted rather than shown as zero when the browser will not say: `null`
  // means unknown, and rendering it as a figure would read as "no space left".
  if (estimate.storage.availableBytes !== null) {
    rows.push({
      key: "availableBytes",
      label: t("visualPacks.metrics.availableBytes"),
      value: formatByteSize(estimate.storage.availableBytes, locale),
    });
  }
  return rows;
}

function sameSelector(left: InstallSelector, right: InstallSelector): boolean {
  if (left.kind !== right.kind) return false;
  switch (left.kind) {
    case "core":
      return true;
    case "printing":
      return right.kind === "printing" && left.set === right.set;
    case "locale":
      return right.kind === "locale" && left.language === right.language && left.set === right.set;
    case "complete":
      return right.kind === "complete" && left.rootSha256 === right.rootSha256;
    case "curated":
      return right.kind === "curated" && left.membershipDigest === right.membershipDigest;
    case "deck_library":
      return right.kind === "deck_library" && left.membershipDigest === right.membershipDigest;
  }
}

export function PackSelector({ summary, curatedSelector, deckLibrarySelector, curatedDrift, deckLibraryDrift, estimate, estimateProgress, pendingActions, durableMutationActive, networkActionsDisabled, onSelectCurated, onSelectDeckLibrary, onEstimate, onInstall }: PackSelectorProps) {
  const { t, i18n } = useTranslation("settings");
  const { catalog } = useSetCatalog();
  const artChain = usePreferencesStore((state) => state.artChain);
  const [kind, setKind] = useState<SelectorKind>("core");
  const [setInput, setSetInput] = useState("");
  const [language, setLanguage] = useState<ImageLocale>("en");
  const curated = kind === "curated";
  const deckLibrary = kind === "deck_library";
  const localMembership = curated || deckLibrary;
  const selector = useMemo(
    () => validSelector(kind, setInput, language, summary, curatedSelector, deckLibrarySelector),
    [curatedSelector, deckLibrarySelector, kind, language, setInput, summary],
  );
  const localSelector = selector?.kind === "curated" || selector?.kind === "deck_library" ? selector : null;
  const matchingEstimate = selector && estimate && sameSelector(selector, estimate.selector)
    && estimate.value.catalogRoot === summary.catalogRoot
    && estimate.value.installedRevision === summary.installedRevision
    ? estimate.value
    : null;
  // Built on the panel's own language rather than the runtime default, like
  // every other figure this component renders.
  const number = new Intl.NumberFormat(i18n.language);
  const estimatePending = pendingActions.has("estimate");
  /** A selected local membership whose backend-owned selector is still absent. */
  const localUnresolved = localMembership && !localSelector;
  /**
   * What may be said about the installed curated pack, from the one predicate
   * that decides it for the badge as well.
   *
   * `unknown` is "say nothing" and covers every case with nothing measured
   * behind it: nothing installed, the read not finished, the read failed, or
   * the card data not resident so the backend declined to load 76 MB to answer.
   */
  const localPack = curated ? CURATED : DECK_LIBRARY;
  const localDrift = curated ? curatedDrift : deckLibraryDrift;
  const driftState = localMembership ? localMembershipDriftState(summary, localPack, localDrift) : "unknown";
  /**
   * Whether there is a curated pack on disk — which is what makes the primary
   * action a sync rather than an install.
   *
   * Read from the SUMMARY, never from whether drift has been measured. Those
   * are different questions with a reachable, permanent gap between them: with
   * a curated pack installed and the card data not resident, a
   * `curatedSelector()` that rejects (the card-data fetch failing makes
   * `planMembership` throw `network`) leaves both the selector and the drift
   * null for the life of the tab — and the button would have offered to
   * "install" a pack already installed, for ever. The summary answers it for
   * free and cannot fail.
   */
  const curatedInstalled = summary.installedPacks.some((entry) => entry.packId === CURATED);
  const deckLibraryInstalled = summary.installedPacks.some((entry) => entry.packId === DECK_LIBRARY);
  const localInstalled = curated ? curatedInstalled : deckLibraryInstalled;
  const autoEstimatedRef = useRef<string | null>(null);

  // Ask what "curated" means the moment it is chosen, and not before: the
  // answer costs a membership plan, and every other option is composed from
  // what is already on screen. Keep each membership in its own effect: a
  // Curated request completing is not an instruction to retry a failed Deck
  // library request, even though both selectors live in this component.
  useEffect(() => {
    if (!networkActionsDisabled && curated && !curatedSelector) onSelectCurated();
  }, [curated, curatedSelector, networkActionsDisabled, onSelectCurated]);
  useEffect(() => {
    if (!networkActionsDisabled && deckLibrary && !deckLibrarySelector) onSelectDeckLibrary();
  }, [deckLibrary, deckLibrarySelector, networkActionsDisabled, onSelectDeckLibrary]);

  // A rejected estimate is deliberately not retried while this exact option
  // remains selected. Moving through another local option is an intentional
  // retry, though, even when that option reuses the estimate already on
  // screen; otherwise a failed deck-library key survives the round trip and
  // suppresses the user's next deck-library selection.
  const localSelectionKey = localSelector
    ? `${localSelector.kind}:${localSelector.membershipDigest}`
    : null;
  useEffect(() => {
    autoEstimatedRef.current = null;
  }, [localSelectionKey]);

  // Estimate on selection, for curated only. Curated opens no bulk stream, so
  // there is nothing for a user to consent to before it runs and no reason to
  // make them press a button whose label promises a catalog scan; every other
  // selector reads the multi-gigabyte archive and must stay a deliberate act.
  //
  // Keyed on the digest AND the catalog the estimate would be bound to,
  // because the hook discards an estimate taken against a superseded one — so
  // a summary change has to be able to ask again, while a re-render must not.
  useEffect(() => {
    if (networkActionsDisabled || !localMembership || !localSelector) return;
    // Already on screen: nothing to ask for. The key is deliberately NOT
    // cleared here — leaving the option does that, and doing it in both places
    // was measured to be redundant (no probe could kill this line).
    if (matchingEstimate) return;
    // The estimate slot is single-occupancy: `estimateInstall` refuses a
    // second request outright. Asking now would be dropped silently and leave
    // Install disabled with no estimate and no error, so wait for the slot
    // instead — `estimatePending` is a dependency, so freeing it re-runs this.
    if (estimatePending) return;
    const key = `${localSelector.kind}:${localSelector.membershipDigest}:${summary.catalogRoot}:${summary.installedRevision}`;
    // Asked for exactly this and got nothing back, so the request failed
    // rather than raced. Retrying on a timer would hammer a failing backend at
    // five requests a second; the user retries with the button, or by
    // reselecting the option, which clears this below.
    if (autoEstimatedRef.current === key) return;
    const timer = setTimeout(() => {
      autoEstimatedRef.current = key;
      onEstimate(localSelector);
    }, CURATED_ESTIMATE_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [estimatePending, localMembership, localSelector, matchingEstimate, networkActionsDisabled, onEstimate, summary]);

  return (
    <fieldset className="flex flex-col gap-4 rounded-[16px] border border-white/10 bg-slate-950/20 p-3 sm:p-4">
      <legend className="px-1 text-sm font-semibold text-slate-100">{t("visualPacks.selector.title")}</legend>
      <div className="grid gap-2 sm:grid-cols-2" role="radiogroup" aria-label={t("visualPacks.selector.title")}>
        {SELECTOR_KINDS.map((option) => (
          <label
            key={option}
            className={`flex min-h-14 cursor-pointer items-center gap-3 rounded-[14px] border px-3 py-3 text-sm transition-colors focus-within:ring-2 focus-within:ring-sky-300 ${
              kind === option
                ? "border-sky-400/60 bg-sky-400/12 text-sky-50"
                : "border-white/10 bg-black/16 text-slate-200 hover:border-white/25 hover:bg-white/[0.04]"
            }`}
          >
            <input
              type="radio"
              name="visual-pack-selector"
              checked={kind === option}
              onChange={() => setKind(option)}
              className="h-4 w-4 accent-sky-400"
            />
            <span className="font-medium">{t(`visualPacks.selector.${option}`)}</span>
          </label>
        ))}
      </div>
      {kind === "printing" && (
        <label className="flex flex-col gap-1.5 text-sm text-slate-200">
          {t("visualPacks.selector.setCode")}
          <input
            value={setInput}
            onChange={(event) => setSetInput(event.target.value)}
            list="visual-pack-set-suggestions"
            placeholder={t("visualPacks.selector.setPlaceholder")}
            className="min-h-11 rounded-[12px] border border-white/10 bg-black/20 px-3 text-slate-100 placeholder:text-slate-500 focus:border-sky-400 focus:outline-none focus:ring-2 focus:ring-sky-400/20"
          />
          <datalist id="visual-pack-set-suggestions">
            {Object.entries(catalog ?? {}).map(([code, set]) => (
              <option key={code} value={code}>{set.name}</option>
            ))}
          </datalist>
        </label>
      )}
      {kind === "printing" && (
        <label className="flex flex-col gap-1.5 text-sm text-slate-200">
          {t("visualPacks.selector.language")}
          <select
            value={language}
            onChange={(event) => setLanguage(event.target.value as ImageLocale)}
            className="min-h-11 rounded-[12px] border border-white/10 bg-slate-950 px-3 text-slate-100 focus:border-sky-400 focus:outline-none focus:ring-2 focus:ring-sky-400/20"
          >
            {Object.entries(IMAGE_LOCALES).map(([locale, name]) => <option key={locale} value={locale}>{name}</option>)}
          </select>
        </label>
      )}
      {kind === "complete" && (
        <p className="break-all text-xs text-slate-400">{t("visualPacks.selector.currentRoot", { root: summary.catalogRoot })}</p>
      )}
      {curated && (
        // Two sentences, because the honest one depends on whether the user
        // has actually made the setting this pack follows. An empty art chain
        // is the DEFAULT, and it selects each card's canonical art — copy that
        // said "your configured set priority" would credit the user with a
        // choice they have not made.
        <p className="text-xs text-slate-400">
          {t(artChain.length === 0 ? "visualPacks.selector.curatedDefaultNote" : "visualPacks.selector.curatedNote")}
        </p>
      )}
      {deckLibrary && <p className="text-xs text-slate-400">{t("visualPacks.selector.deckLibraryNote")}</p>}
      {/* What a Sync would change, in the three categories the backend counts.
          THREE, not two: a Scryfall re-scan moves a `sourceUrl` under an
          unchanged asset key, so the membership differs while both key sets are
          identical — an add/remove-only report would say "0 to add, 0 to
          remove" beside a live Sync button and read as a bug. Nothing here
          starts a download; only pressing Sync does. */}
      {/* `localDrift &&` is the type narrowing, not a second condition:
          `driftState` is `unknown` whenever it is null. */}
      {localMembership && driftState !== "unknown" && localDrift && (
        <p className={`text-xs ${driftState === "current" ? "text-slate-400" : "text-amber-200"}`}>
          {driftState === "current"
            ? t("visualPacks.selector.driftNone")
            : t("visualPacks.selector.driftSummary", {
              add: number.format(localDrift.add),
              remove: number.format(localDrift.remove),
              refresh: number.format(localDrift.refresh),
            })}
        </p>
      )}
      {!selector && kind === "printing" && (
        <p role="alert" className="text-xs text-rose-300">{t("visualPacks.selector.invalidSet")}</p>
      )}
      {matchingEstimate && (
        <section
          aria-label={t(localMembership ? "visualPacks.estimate.curatedTitle" : "visualPacks.estimate.title")}
          className="rounded-[14px] border border-sky-400/20 bg-sky-400/[0.06] p-3 text-xs text-sky-100"
        >
          <h4 className="mb-2 font-semibold text-slate-100">
            {t(localMembership ? "visualPacks.estimate.curatedTitle" : "visualPacks.estimate.title")}
          </h4>
          <ol className="mb-3 list-decimal space-y-1 pl-5">
            {matchingEstimate.packIds.map((id) => <li key={id} className="break-all">{packLabel(id, t)}</li>)}
          </ol>
          <dl className="grid grid-cols-[minmax(0,1fr)_auto] gap-x-3 gap-y-1 tabular-nums">
            {estimateRows(matchingEstimate, localMembership, i18n.language, t).map((row) => (
              <div className="contents" key={row.key}>
                <dt>{row.label}</dt>
                <dd className="break-all font-mono text-slate-100">{row.value}</dd>
              </div>
            ))}
          </dl>
          {/* A warning, never a veto: the projection behind it is an
              order-of-magnitude figure from six samples per rung, and running
              out of quota mid-download is the milder failure — it classifies
              as `storage`, which is retryable, and a resume skips everything
              already cached. See `InstallEstimate.headroom`. */}
          {matchingEstimate.headroom === "insufficient" && (
            <p className="mt-3 text-amber-200">{t("visualPacks.estimate.headroomWarning")}</p>
          )}
          {matchingEstimate.storage.persistence === "best_effort" && (
            <p className="mt-2 text-amber-200">{t("visualPacks.estimate.evictionWarning")}</p>
          )}
        </section>
      )}
      {estimateProgress && (
        <section aria-label={t("visualPacks.estimate.progressTitle")} className="rounded-[14px] border border-sky-400/20 bg-sky-400/[0.06] p-3 text-xs text-sky-100">
          <h4 className="mb-2 font-semibold text-slate-100">{t("visualPacks.estimate.progressTitle")}</h4>
          <progress
            value={estimateProgress.compressedBytesRead}
            max={estimateProgress.compressedBytesTotal}
            className="h-2 w-full overflow-hidden rounded-full accent-sky-400"
          />
          <p className="mt-2 tabular-nums text-sky-100/80">
            {t("visualPacks.estimate.progress", {
              downloaded: number.format(Math.floor(estimateProgress.compressedBytesRead / (1024 * 1024))),
              total: number.format(Math.ceil(estimateProgress.compressedBytesTotal / (1024 * 1024))),
              records: number.format(estimateProgress.recordsScanned),
              images: number.format(estimateProgress.assetRecords),
            })}
          </p>
        </section>
      )}
      <div className="flex flex-wrap gap-2 border-t border-white/8 pt-3">
        <button
          type="button"
          // Enabled with no selector when curated's resolution FAILED, because
          // that is the only state in which the panel has an error on screen
          // and every control beneath it dead: the selector is null, so both
          // buttons would be disabled and the only undiscoverable way out is
          // to pick another option and come back. Here it retries the thing
          // that failed.
          disabled={networkActionsDisabled || (!selector && !localUnresolved) || estimatePending || (localMembership && pendingActions.has(curated ? "curated" : "deck_library"))}
          onClick={() => {
            if (localUnresolved) {
              if (curated) onSelectCurated(); else onSelectDeckLibrary();
            }
            else if (selector) onEstimate(selector);
          }}
          className="min-h-11 rounded-[12px] border border-sky-400/50 bg-sky-400/10 px-4 py-2 text-sm font-medium text-sky-50 transition-colors hover:bg-sky-400/18 disabled:opacity-40 focus-visible:ring-2 focus-visible:ring-sky-300"
        >
          {/* Curated scans no catalog, so it says neither "Scan catalog" nor
              "Scanning": the estimate has already run on selection and this is
              only the way back to it. */}
          {localMembership
            ? t(estimatePending ? "visualPacks.actions.estimatingCurated" : "visualPacks.actions.estimateCurated")
            : t(estimatePending ? "visualPacks.actions.estimating" : "visualPacks.actions.estimate")}
        </button>
        <button
          type="button"
          disabled={networkActionsDisabled || !selector || !matchingEstimate || durableMutationActive || [...pendingActions].some((entry) => MUTATION_ACTIONS.has(entry))}
          onClick={() => selector && onInstall(selector)}
          className="min-h-11 rounded-[12px] border border-emerald-400/50 bg-emerald-400/10 px-4 py-2 text-sm font-medium text-emerald-50 transition-colors hover:bg-emerald-400/18 disabled:opacity-40 focus-visible:ring-2 focus-visible:ring-emerald-300"
        >
          {/* "Sync" once a curated pack is on disk: the same request, but the
              act is bringing an existing pack up to date rather than adding one.
              `driftState` says by HOW MUCH and may be `unknown`; whether there
              is anything to sync at all is a question the summary answers. */}
          {t(localMembership && localInstalled ? "visualPacks.actions.sync" : "visualPacks.actions.install")}
        </button>
      </div>
    </fieldset>
  );
}
