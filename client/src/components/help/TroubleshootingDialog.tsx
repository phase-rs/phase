import { useEffect, useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";

import { runConnectivityDiagnostics } from "../../network/connectivityDiagnostics";
import { downloadBlob } from "../../services/fileDownload";
import { directoryUrl } from "../../services/serverDirectory";
import { runDiagnostics, type DiagnosticReport } from "../../services/troubleshooting";
import { useGameStore } from "../../stores/gameStore";
import { ModalPanelShell } from "../ui/ModalPanelShell";

const actionClass = "min-h-11 min-w-11 rounded-xl border border-white/15 bg-white/5 px-4 py-2 text-sm font-semibold text-slate-100 transition hover:bg-white/10 active:bg-white/20 focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-300 disabled:cursor-not-allowed disabled:opacity-50";
const statusClass = { pass: "text-emerald-300", warning: "text-amber-200", unavailable: "text-slate-300", error: "text-rose-300" };

export function TroubleshootingDialog({ onClose, returnFocusRef }: {
  onClose: () => void;
  returnFocusRef?: RefObject<HTMLElement | SVGElement | null>;
}) {
  const { t } = useTranslation();
  const mode = useGameStore((s) => s.gameMode);
  const [report, setReport] = useState<DiagnosticReport | null>(null);
  const [running, setRunning] = useState(false);
  const [saving, setSaving] = useState(false);
  const [notice, setNotice] = useState<"saved" | "requested" | "failed" | "cancelled" | "runnerFailed" | null>(null);
  const runRef = useRef<AbortController | null>(null);
  const mounted = useRef(true);
  const savingRef = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; runRef.current?.abort(); };
  }, []);

  const close = () => { mounted.current = false; runRef.current?.abort(); onClose(); };
  const run = async () => {
    if (runRef.current || savingRef.current) return;
    const controller = new AbortController();
    runRef.current = controller;
    setRunning(true);
    setReport(null);
    setNotice(null);
    try {
      const result = await runDiagnostics({
        version: __APP_VERSION__, build: __BUILD_HASH__, mode,
        online: navigator.onLine, visibility: document.visibilityState,
        webAssembly: typeof WebAssembly !== "undefined", webRtc: typeof RTCPeerConnection !== "undefined",
      }, directoryUrl(), controller.signal, runConnectivityDiagnostics);
      if (mounted.current && !controller.signal.aborted) setReport(result);
    } catch {
      if (mounted.current && !controller.signal.aborted) setNotice("runnerFailed");
    } finally {
      if (runRef.current === controller) runRef.current = null;
      if (mounted.current) setRunning(false);
    }
  };
  const save = async () => {
    if (!report || savingRef.current || runRef.current) return;
    savingRef.current = true;
    setSaving(true);
    setNotice(null);
    try {
      const result = await downloadBlob("phase-diagnostics.json", new Blob([JSON.stringify(report, null, 2)], { type: "application/json" }));
      if (mounted.current) setNotice(result.kind);
    } catch (error) {
      if (mounted.current) setNotice(error instanceof DOMException && error.name === "AbortError" ? "cancelled" : "failed");
    } finally {
      savingRef.current = false;
      if (mounted.current) setSaving(false);
    }
  };
  const recentHistory = report?.history.slice().reverse() ?? [];
  const lastEngineFailure = recentHistory.find((event) => event.kind === "engine-not-initialized");
  const lastDisconnect = recentHistory.find((event) => event.kind === "disconnect" && event.cause !== "local-close" && event.cause !== "remote-disconnect");
  const lastSetupFailure = recentHistory.find((event) =>
    (event.kind === "signaling" && (event.event === "error" || event.event === "constructor-error" || event.event === "timeout"))
    || (event.kind === "credentials" && event.outcome === "stun-fallback")
    || (event.kind === "connection-attempt" && (event.event === "error" || event.event === "timeout")));
  const lastRoute = recentHistory.find((event) => event.kind === "candidate-route" || (event.kind === "disconnect" && event.candidates));
  const retainedCandidates = lastRoute && (lastRoute.kind === "candidate-route" || lastRoute.kind === "disconnect") ? lastRoute.candidates : null;
  return (
    <ModalPanelShell title={t("troubleshooting.title")} subtitle={t("troubleshooting.intro")}
      onClose={close} returnFocusRef={returnFocusRef} maxWidthClassName="max-w-2xl"
      overlayClassName="z-[130] [&_button]:min-h-11 [&_button]:min-w-11"
      bodyClassName="overflow-y-auto p-4 lg:p-6">
      <p className="mb-4 text-sm leading-6 text-slate-300">{t("troubleshooting.limitations")}</p>
      <div className="flex flex-wrap gap-3">
        <button type="button" className={actionClass} disabled={running || saving} onClick={() => void run()}>
          {t(running ? "troubleshooting.running" : report ? "troubleshooting.rerun" : "troubleshooting.run")}
        </button>
        <button type="button" className={actionClass} disabled={!report || running || saving} onClick={() => void save()}>
          {t("troubleshooting.export")}
        </button>
      </div>
      <div role="status" aria-live="polite" className="mt-3 text-sm text-slate-200">
        {running && t("troubleshooting.running")}
        {notice && t(`troubleshooting.${notice}`)}
      </div>
      {report && <>
        <ul className="mt-4 divide-y divide-white/10 rounded-xl border border-white/10 bg-black/15" aria-label={t("troubleshooting.title")}>
          {report.results.map((result, index) => <li key={`${result.check}-${index}`} className="p-4">
            <div className="flex flex-wrap items-baseline justify-between gap-2">
              <h3 className="text-sm font-semibold text-white">{t(`troubleshooting.checks.${result.check}`)}</h3>
              <span className={`text-sm ${statusClass[result.status]}`}>{t(`troubleshooting.statuses.${result.status}`)}</span>
            </div>
            <p className="mt-1 text-sm leading-6 text-slate-300">{t(`troubleshooting.reasons.${result.reason}`)}</p>
            {result.evidence?.credentialFailure && <p className="mt-1 text-sm text-slate-300">{t("troubleshooting.credentialDetail", { reason: t(`troubleshooting.credentialFailures.${result.evidence.credentialFailure}`) })}</p>}
            {result.evidence?.httpStatus !== undefined && <p className="mt-1 text-sm text-slate-300">{t("troubleshooting.httpStatus", { status: result.evidence.httpStatus })}</p>}
            {result.diagnosticId && <p className="mt-1 break-all text-xs text-slate-400">{t("troubleshooting.connectionId", { id: result.diagnosticId })}</p>}
            {result.evidence?.connectionError && <p className="mt-1 text-sm text-slate-300">{t("troubleshooting.peerError", { code: result.evidence.connectionError })}</p>}
            {result.evidence?.candidates?.relayProtocol && <p className="mt-1 text-sm text-slate-300">{t("troubleshooting.relayProtocol", { protocol: result.evidence.candidates.relayProtocol })}</p>}
            {result.evidence?.peerError && <p className="mt-1 text-sm text-slate-300">{t("troubleshooting.peerError", { code: result.evidence.peerError })}</p>}
            {result.evidence?.durationMs !== undefined && <p className="mt-1 text-xs text-slate-400">{t("troubleshooting.duration", { duration: result.evidence.durationMs })}</p>}
            {result.evidence?.candidates && <p className="mt-1 text-sm text-slate-300">{t("troubleshooting.routeEvidence", {
              local: result.evidence.candidates.localType ?? t("troubleshooting.unknownRoute"),
              remote: result.evidence.candidates.remoteType ?? t("troubleshooting.unknownRoute"),
              protocol: result.evidence.candidates.localProtocol ?? t("troubleshooting.unknownRoute"),
            })}</p>}
            {!!result.evidence?.iceErrorCodes?.length && <p className="mt-1 text-sm text-slate-300">{t("troubleshooting.iceErrors", { codes: result.evidence.iceErrorCodes.join(", ") })}</p>}
            <p className="mt-1 text-xs text-slate-400">{t("troubleshooting.observed", { time: new Date(result.observedAt).toLocaleString() })}</p>
          </li>)}
        </ul>
        <section className="mt-4 space-y-2 rounded-xl border border-white/10 p-4 text-sm text-slate-300">
          <p>{t("troubleshooting.history", { count: report.history.length })}</p>
          {lastSetupFailure && <p>{t("troubleshooting.pastSetup")} <span className="text-slate-400">{t("troubleshooting.observed", { time: new Date(lastSetupFailure.observedAt).toLocaleString() })}</span></p>}
          {retainedCandidates && lastRoute && <p>{t("troubleshooting.pastRoute", { local: retainedCandidates.localType ?? t("troubleshooting.unknownRoute"), remote: retainedCandidates.remoteType ?? t("troubleshooting.unknownRoute") })} <span className="text-slate-400">{t("troubleshooting.observed", { time: new Date(lastRoute.observedAt).toLocaleString() })}</span></p>}
          {lastEngineFailure && <p>{t("troubleshooting.pastEngine")} <span className="text-slate-400">{t("troubleshooting.observed", { time: new Date(lastEngineFailure.observedAt).toLocaleString() })}</span></p>}
          {lastDisconnect && <p>{t("troubleshooting.pastDisconnect")} <span className="text-slate-400">{t("troubleshooting.observed", { time: new Date(lastDisconnect.observedAt).toLocaleString() })}</span></p>}
        </section>
      </>}
      <p className="mt-4 text-sm leading-6 text-slate-400">{t("troubleshooting.privacy")}</p>
    </ModalPanelShell>
  );
}
