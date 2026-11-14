import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  discoverLanServers, getLanServerStatus, initializeLanCapabilities, normalizeLanEndpoint,
  startLanServer, stopLanServer, type DiscoveredLanServer, type LanServerStatus,
} from "../../services/lan";
import { nativeEngineKeyForCurrentOrigin } from "../../services/nativeEngine";
import { MAX_USER_LOBBY_SOURCES, useMultiplayerStore, type LobbySource } from "../../stores/multiplayerStore";
import { menuButtonClass } from "../menu/buttonStyles";

// Object identity ensures stop cannot remove a source the user removed and
// subsequently re-added themselves. Ownership survives panel remounts.
const ownedSources = new Set<LobbySource>();
// Panels share the same native lifecycle and source ownership. A response
// started before an action must not reconcile state after that action.
let operationRevision = 0;
let activeOperations = 0;

function reconcileOwnedSources(status: LanServerStatus): void {
  const store = useMultiplayerStore.getState();
  for (const source of ownedSources) {
    if (status.running && status.addresses.includes(normalizeLanEndpoint(source.url) ?? "")) continue;
    if (store.userLobbySources.includes(source)) store.removeUserLobbySource(source.url);
    ownedSources.delete(source);
  }
}

export function LanServers() {
  const { t } = useTranslation("multiplayer");
  const [supported, setSupported] = useState<boolean | null>(null);
  const [status, setStatus] = useState<LanServerStatus | null>(null);
  const [servers, setServers] = useState<DiscoveredLanServer[] | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const busy = useRef(false);
  const button = `${menuButtonClass({ tone: "neutral", size: "sm" })} min-h-11 disabled:opacity-50`;

  const reportError = useCallback((failure: unknown) => {
    const detail = failure instanceof Error ? failure.message
      : typeof failure === "object" && failure !== null && "detail" in failure
        ? String(failure.detail) : String(failure);
    setError(t("lan.error", { detail }));
  }, [t]);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const refresh = async () => {
      const revision = operationRevision;
      const isCurrent = () => !cancelled && activeOperations === 0 && revision === operationRevision;
      try {
        if (activeOperations === 0) {
          const next = await getLanServerStatus();
          if (isCurrent()) { reconcileOwnedSources(next); setStatus(next); }
        }
      } catch (failure) {
        if (isCurrent()) { setStatus(null); reportError(failure); }
      } finally {
        if (!cancelled) timer = setTimeout(() => void refresh(), 5000);
      }
    };
    void initializeLanCapabilities().then((available) => {
      if (cancelled) return;
      setSupported(available);
      if (available) void refresh();
    });
    return () => { cancelled = true; clearTimeout(timer); };
  }, [reportError]);

  const run = async (operation: () => Promise<void>) => {
    if (busy.current) return;
    busy.current = true;
    operationRevision++;
    activeOperations++;
    setPending(true); setError(null); setNotice(null);
    try { await operation(); } catch (failure) { reportError(failure); }
    finally { activeOperations--; busy.current = false; setPending(false); }
  };

  const add = async (url: string, owned: boolean) => {
    const store = useMultiplayerStore.getState();
    const result = store.addUserLobbySource(url);
    if (!result.ok) {
      setError(result.reason === "duplicate" ? t("serverPicker.sourceDuplicate")
        : result.reason === "cap_reached" ? t("serverPicker.sourceCapReached", { max: MAX_USER_LOBBY_SOURCES })
          : t("serverPicker.urlError"));
      return;
    }
    if (owned) ownedSources.add(result.source);
    const socket = await store.ensureSubscriptionSocket(result.source.url);
    if (socket) setNotice(t("lan.added"));
    else setError(t("serverPicker.sourceStatusDegraded"));
  };

  return (
    <section aria-label={t("lan.title")} className="flex flex-col gap-3 rounded-panel border border-hairline p-4">
      <h3 className="text-sm font-semibold text-fg-card-body">{t("lan.title")}</h3>
      {supported === null ? <p role="status">{t("lan.checking")}</p> : !supported ? (
        <><p className="text-sm text-fg-meta">{t("lan.unavailable")}</p><p className="text-sm text-fg-meta">{t("lan.guidance")}</p></>
      ) : (
        <>
          <div className="flex flex-wrap gap-2">
            {status?.running ? (
              <button type="button" className={button} disabled={pending} onClick={() => void run(async () => {
                await stopLanServer();
                const next = await getLanServerStatus();
                reconcileOwnedSources(next); setStatus(next);
              })}>{t("lan.stop")}</button>
            ) : (
              <button type="button" className={button} disabled={pending || !status || !nativeEngineKeyForCurrentOrigin()} onClick={() => void run(async () => {
                const next = await startLanServer(); setStatus(next);
              })}>{t("lan.start")}</button>
            )}
            <button type="button" className={button} disabled={pending} onClick={() => void run(async () => {
              setServers(null); setServers(await discoverLanServers());
            })}>{t("lan.scan")}</button>
          </div>
          {!nativeEngineKeyForCurrentOrigin() && <p className="text-sm text-fg-meta">{t("lan.originUnavailable")}</p>}
          {status?.running && <p role="status" className="text-sm text-fg-meta">{t("lan.running")}</p>}
          {status?.addresses.map((url) => (
            <div key={url} className="flex flex-wrap items-center gap-2">
              <code className="min-w-0 break-all text-xs">{url}</code>
              <button type="button" className={button} disabled={pending} aria-label={t("lan.copy")} onClick={() => void run(async () => {
                await navigator.clipboard.writeText(url); setNotice(t("lan.copied"));
              })}>{t("lan.copy")}</button>
              <button type="button" className={button} disabled={pending} onClick={() => void run(() => add(url, true))}>{t("lan.add")}</button>
            </div>
          ))}
          {servers?.length === 0 && <p role="status" className="text-sm text-fg-meta">{t("lan.empty")}</p>}
          {servers?.map((server) => (
            <div key={server.url} className="flex flex-wrap items-center gap-2">
              <span className="text-sm">{server.name}</span><code className="break-all text-xs">{server.url}</code>
              <button type="button" className={button} disabled={pending} onClick={() => void run(() => add(server.url, status?.addresses.includes(normalizeLanEndpoint(server.url) ?? "") ?? false))}>{t("lan.add")}</button>
            </div>
          ))}
        </>
      )}
      {pending && <p role="status" className="text-sm text-fg-meta">{t("lan.pending")}</p>}
      {notice && <p role="status" className="text-sm text-fg-meta">{notice}</p>}
      {error && <p role="alert" className="text-sm text-rose-300">{error}</p>}
    </section>
  );
}
