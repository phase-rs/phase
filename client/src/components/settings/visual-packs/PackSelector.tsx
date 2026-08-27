import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { useSetCatalog } from "../../../hooks/useSetSymbols.ts";
import {
  packId,
  type CatalogSummary,
  type InstallEstimate,
  type InstallSelector,
} from "../../../services/visualPacks/types.ts";

const IMAGE_LOCALES = ["de", "es", "fr", "it", "pt"] as const;
const MUTATION_ACTIONS = new Set(["install", "cancel", "resume", "repair", "remove"]);
type ImageLocale = (typeof IMAGE_LOCALES)[number];
type SelectorKind = "core" | "printing" | "locale" | "complete";

interface PackSelectorProps {
  summary: CatalogSummary;
  estimate: { selector: InstallSelector; value: InstallEstimate } | null;
  pendingActions: ReadonlySet<string>;
  durableMutationActive: boolean;
  onEstimate(selector: InstallSelector): void;
  onInstall(selector: InstallSelector): void;
}

function normalizedSet(value: string): string {
  return value.trim().normalize("NFC").toLowerCase();
}

function validSelector(kind: SelectorKind, setInput: string, language: ImageLocale, summary: CatalogSummary): InstallSelector | null {
  const set = normalizedSet(setInput);
  try {
    switch (kind) {
      case "core":
        return { kind: "core" };
      case "printing":
        packId(`printing:${set}`);
        return { kind: "printing", set };
      case "locale":
        packId(`locale:${language}:${set}`);
        return { kind: "locale", language, set };
      case "complete":
        return { kind: "complete", rootSha256: summary.catalogRoot };
    }
  } catch {
    return null;
  }
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
  }
}

export function PackSelector({ summary, estimate, pendingActions, durableMutationActive, onEstimate, onInstall }: PackSelectorProps) {
  const { t } = useTranslation("settings");
  const { catalog } = useSetCatalog();
  const [kind, setKind] = useState<SelectorKind>("core");
  const [setInput, setSetInput] = useState("");
  const [language, setLanguage] = useState<ImageLocale>("de");
  const selector = useMemo(
    () => validSelector(kind, setInput, language, summary),
    [kind, language, setInput, summary],
  );
  const matchingEstimate = selector && estimate && sameSelector(selector, estimate.selector)
    && estimate.value.catalogRoot === summary.catalogRoot
    && estimate.value.installedRevision === summary.installedRevision
    ? estimate.value
    : null;

  return (
    <fieldset className="flex flex-col gap-4 rounded-[16px] border border-white/10 bg-slate-950/20 p-3 sm:p-4">
      <legend className="px-1 text-sm font-semibold text-slate-100">{t("visualPacks.selector.title")}</legend>
      <div className="grid gap-2 sm:grid-cols-2" role="radiogroup" aria-label={t("visualPacks.selector.title")}>
        {(["core", "printing", "locale", "complete"] as const).map((option) => (
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
      {(kind === "printing" || kind === "locale") && (
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
      {kind === "locale" && (
        <label className="flex flex-col gap-1.5 text-sm text-slate-200">
          {t("visualPacks.selector.language")}
          <select
            value={language}
            onChange={(event) => setLanguage(event.target.value as ImageLocale)}
            className="min-h-11 rounded-[12px] border border-white/10 bg-slate-950 px-3 text-slate-100 focus:border-sky-400 focus:outline-none focus:ring-2 focus:ring-sky-400/20"
          >
            {IMAGE_LOCALES.map((locale) => <option key={locale} value={locale}>{locale}</option>)}
          </select>
        </label>
      )}
      {kind === "complete" && (
        <p className="break-all text-xs text-slate-400">{t("visualPacks.selector.currentRoot", { root: summary.catalogRoot })}</p>
      )}
      {!selector && (kind === "printing" || kind === "locale") && (
        <p role="alert" className="text-xs text-rose-300">{t("visualPacks.selector.invalidSet")}</p>
      )}
      {matchingEstimate && (
        <section aria-label={t("visualPacks.estimate.title")} className="rounded-[14px] border border-sky-400/20 bg-sky-400/[0.06] p-3 text-xs text-sky-100">
          <h4 className="mb-2 font-semibold text-slate-100">{t("visualPacks.estimate.title")}</h4>
          <ol className="mb-3 list-decimal space-y-1 pl-5">
            {matchingEstimate.packIds.map((id) => <li key={id} className="break-all">{id}</li>)}
          </ol>
          <dl className="grid grid-cols-[minmax(0,1fr)_auto] gap-x-3 gap-y-1 tabular-nums">
            {(["assetRecords", "uniqueObjects", "logicalImageBytes", "uniqueImageBytes", "shardCount", "shardBytes"] as const).map((metric) => (
              <div className="contents" key={metric}>
                <dt>{t(`visualPacks.metrics.${metric}`)}</dt>
                <dd className="break-all font-mono text-slate-100">{matchingEstimate[metric]}</dd>
              </div>
            ))}
          </dl>
        </section>
      )}
      <div className="flex flex-wrap gap-2 border-t border-white/8 pt-3">
        <button
          type="button"
          disabled={!selector || pendingActions.has("estimate")}
          onClick={() => selector && onEstimate(selector)}
          className="min-h-11 rounded-[12px] border border-sky-400/50 bg-sky-400/10 px-4 py-2 text-sm font-medium text-sky-50 transition-colors hover:bg-sky-400/18 disabled:opacity-40 focus-visible:ring-2 focus-visible:ring-sky-300"
        >
          {pendingActions.has("estimate") ? t("visualPacks.actions.estimating") : t("visualPacks.actions.estimate")}
        </button>
        <button
          type="button"
          disabled={!selector || !matchingEstimate || durableMutationActive || [...pendingActions].some((entry) => MUTATION_ACTIONS.has(entry))}
          onClick={() => selector && onInstall(selector)}
          className="min-h-11 rounded-[12px] border border-emerald-400/50 bg-emerald-400/10 px-4 py-2 text-sm font-medium text-emerald-50 transition-colors hover:bg-emerald-400/18 disabled:opacity-40 focus-visible:ring-2 focus-visible:ring-emerald-300"
        >
          {t("visualPacks.actions.install")}
        </button>
      </div>
    </fieldset>
  );
}
