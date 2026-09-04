import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  prepareForOffline,
  type OfflinePreparationCapabilityName,
  type OfflinePreparationResult,
} from "../../services/offlinePreparation.ts";
import { useConnectivityStore, useEffectiveOffline } from "../../stores/connectivityStore.ts";
import { ConfirmDialog } from "../ui/ConfirmDialog.tsx";
import { packLabel } from "./visual-packs/packLabels.ts";

const CAPABILITIES: readonly OfflinePreparationCapabilityName[] = [
  "appShell",
  "browserEngine",
  "scryfallSearch",
  "preconCatalog",
  "bundledAiCatalog",
  "deckLibrary",
  "coreVisuals",
  "nativeEngine",
];

type PreparationIntent = "prepare" | "enable-offline";

function reconnectRequiredResult(): OfflinePreparationResult {
  return {
    status: "reconnect-required",
    capabilities: {
      appShell: { status: "not-ready" },
      browserEngine: { status: "not-ready" },
      scryfallSearch: { status: "not-ready" },
      preconCatalog: { status: "not-ready" },
      bundledAiCatalog: { status: "not-ready" },
      deckLibrary: { status: "not-ready" },
      coreVisuals: { status: "not-ready" },
      nativeEngine: { status: "not-applicable" },
    },
    visualPacks: { status: "not-installed", installedPacks: [] },
    requiredGaps: [],
  };
}

export function OfflinePreparationSection({ nativeEngineEnabled }: { nativeEngineEnabled: boolean }) {
  const { t } = useTranslation("settings");
  const forcedOffline = useConnectivityStore((state) => state.forcedOffline);
  const setForcedOffline = useConnectivityStore((state) => state.setForcedOffline);
  const effectiveOffline = useEffectiveOffline();
  const [result, setResult] = useState<OfflinePreparationResult | null>(null);
  const [preparing, setPreparing] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const mounted = useRef(true);
  const requestGeneration = useRef(0);
  const activeIntent = useRef<PreparationIntent | null>(null);
  const prepareButtonRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      requestGeneration.current += 1;
      activeIntent.current = null;
    };
  }, []);

  const prepare = useCallback(async (intent: PreparationIntent) => {
    if (activeIntent.current === intent) return;
    activeIntent.current = intent;
    const generation = ++requestGeneration.current;
    setPreparing(true);
    setConfirming(false);
    const next = await prepareForOffline({ nativeEngineEnabled });
    if (!mounted.current || generation !== requestGeneration.current) return;
    activeIntent.current = null;
    setPreparing(false);
    setResult(next);
    if (intent !== "enable-offline") return;
    if (next.status === "ready") {
      setForcedOffline(true);
      return;
    }
    if (next.status !== "reconnect-required") setConfirming(true);
  }, [nativeEngineEnabled, setForcedOffline]);

  const toggleOffline = (checked: boolean) => {
    if (!checked) {
      requestGeneration.current += 1;
      activeIntent.current = null;
      setPreparing(false);
      setConfirming(false);
      setForcedOffline(false);
      return;
    }
    if (effectiveOffline) {
      requestGeneration.current += 1;
      activeIntent.current = null;
      setPreparing(false);
      setConfirming(false);
      setResult(reconnectRequiredResult());
      return;
    }
    void prepare("enable-offline");
  };

  const statusKey = preparing ? "preparing" : result?.status ?? "needs-preparation";
  const requiredGaps = result?.requiredGaps ?? [];
  const installedArt = result?.visualPacks.installedPacks ?? [];

  return (
    <section className="rounded-[20px] border border-white/10 bg-black/18 p-4 shadow-[0_18px_54px_rgba(0,0,0,0.18)] backdrop-blur-md sm:p-5">
      <h3 className="mb-2 text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-slate-500">
        {t("offlinePreparation.title")}
      </h3>
      <p className="text-xs leading-relaxed text-slate-400">{t("offlinePreparation.description")}</p>

      <div className="mt-4 flex flex-wrap items-center gap-3">
        <button
          ref={prepareButtonRef}
          type="button"
          onClick={() => { void prepare("prepare"); }}
          disabled={preparing}
          className="min-h-11 rounded-[14px] border border-sky-400/40 bg-sky-500/14 px-4 py-2 text-sm font-medium text-sky-100 transition hover:bg-sky-500/25 disabled:cursor-not-allowed disabled:opacity-60"
        >
          {preparing ? t("offlinePreparation.preparing") : t("offlinePreparation.prepare")}
        </button>
        <label className="flex min-h-11 items-center gap-2 text-sm text-slate-200">
          <input
            type="checkbox"
            checked={forcedOffline}
            onChange={(event) => toggleOffline(event.target.checked)}
            className="accent-cyan-500"
          />
          <span>{t("offlinePreparation.workOffline")}</span>
        </label>
      </div>

      <p role="status" aria-live="polite" className="mt-3 text-sm text-slate-300">
        {t(`offlinePreparation.states.${statusKey}`)}
      </p>

      {result && (
        <>
          <ul className="mt-3 space-y-1 text-xs text-slate-400" aria-label={t("offlinePreparation.checklistLabel")}>
            {CAPABILITIES.map((name) => (
              <li key={name} className="flex justify-between gap-4">
                <span>{t(`offlinePreparation.capabilities.${name}`)}</span>
                <span>{t(`offlinePreparation.capabilityStates.${result.capabilities[name].status}`)}</span>
              </li>
            ))}
          </ul>
          {/* Card art is reported, never required — a deck plays correctly with
              none installed. Saying so plainly is the point: this panel used to
              render nothing at all in the "not-installed" case, so a device with
              zero images cached still read as ready. */}
          {result.visualPacks.status === "not-installed" && (
            <p className="mt-3 text-xs text-amber-200">{t("offlinePreparation.cardArtMissing")}</p>
          )}
          {result.visualPacks.status === "warning" && (
            <p className="mt-3 text-xs text-amber-200">{t("offlinePreparation.visualWarning")}</p>
          )}
          {result.visualPacks.status === "ready" && installedArt.length > 0 && (
            <p className="mt-3 text-xs text-slate-400">
              {t("offlinePreparation.cardArtInstalled", {
                packs: installedArt.map((pack) => packLabel(pack, t)).join(", "),
              })}
            </p>
          )}
        </>
      )}

      <ConfirmDialog
        open={confirming}
        title={t("offlinePreparation.incompleteTitle")}
        message={t("offlinePreparation.incompleteMessage", {
          capabilities: requiredGaps.map((name) => t(`offlinePreparation.capabilities.${name}`)).join(", "),
        })}
        confirmLabel={t("offlinePreparation.confirmOffline")}
        onConfirm={() => {
          setConfirming(false);
          setForcedOffline(true);
        }}
        onCancel={() => setConfirming(false)}
        tone="danger"
        returnFocusRef={prepareButtonRef}
      />
    </section>
  );
}
