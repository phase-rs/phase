import { useEffect, useMemo, useRef, useState } from "react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import {
  SERVER_PRESETS,
  isValidWebSocketUrl,
  mixedContentBlockReason,
} from "../../services/serverDetection";
import {
  MAX_USER_LOBBY_SOURCES,
  directoryLobbySources,
  lobbySources,
  useMultiplayerStore,
  type LobbySource,
} from "../../stores/multiplayerStore";
import type { ReconnectState } from "../../services/openPhaseSocket";
import { menuButtonClass } from "../menu/buttonStyles";
import { ServerFlag } from "./ServerFlag";

interface ServerPickerProps {
  onClose: () => void;
}

type ConnTestState = "idle" | "testing" | "ok" | "fail";

export function ServerPicker({ onClose }: ServerPickerProps) {
  const { t } = useTranslation("multiplayer");
  const hostingServer = useMultiplayerStore((s) => s.hostingServer);
  const userLobbySources = useMultiplayerStore((s) => s.userLobbySources);
  const sourceStatus = useMultiplayerStore((s) => s.sourceStatus);
  const setHostingServer = useMultiplayerStore((s) => s.setHostingServer);
  const addUserLobbySource = useMultiplayerStore((s) => s.addUserLobbySource);
  const removeUserLobbySource = useMultiplayerStore((s) => s.removeUserLobbySource);
  const directorySources = useMultiplayerStore((s) => s.directorySources);
  const disabledDirectorySources = useMultiplayerStore(
    (s) => s.disabledDirectorySources,
  );
  const setDirectorySourceEnabled = useMultiplayerStore(
    (s) => s.setDirectorySourceEnabled,
  );
  const sources = useMemo(
    () =>
      lobbySources({
        userLobbySources,
        sourceStatus,
        directorySources,
        disabledDirectorySources,
      }),
    [userLobbySources, sourceStatus, directorySources, disabledDirectorySources],
  );
  /** The same unshadowed directory set, but including the DISABLED entries
   * `lobbySources` omits — this list is the only place one can be switched back
   * on. Single authority for every directory row rendered below. */
  const directoryRows = useMemo(
    () =>
      directoryLobbySources({
        userLobbySources,
        sourceStatus,
        directorySources,
        disabledDirectorySources,
      }),
    [userLobbySources, sourceStatus, directorySources, disabledDirectorySources],
  );
  const [customUrl, setCustomUrl] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [connTest, setConnTest] = useState<ConnTestState>("idle");
  const panelRef = useRef<HTMLDivElement>(null);

  /** Live status line for one source row. `undefined` = never dialed. */
  const statusLabel = (state: ReconnectState | undefined): string | null => {
    switch (state) {
      case "open":
        return t("serverPicker.connected");
      case "connecting":
      case "reconnecting":
        return t("serverPicker.sourceStatusConnecting");
      case "offline":
        return t("serverPicker.sourceStatusDegraded");
      case undefined:
        return null;
    }
  };

  // 3s WebSocket probe — opens the URL, succeeds on `onopen`, fails on
  // `onerror` or timeout. Cheap diagnostic that catches the common cases
  // (server down, wrong port, blocked by firewall) before the user
  // commits the address.
  const testUrl = (url: string) => {
    const trimmed = url.trim();
    if (!isValidWebSocketUrl(trimmed)) {
      setConnTest("fail");
      return;
    }
    setConnTest("testing");
    const ws = new WebSocket(trimmed);
    const timeout = window.setTimeout(() => {
      ws.close();
      setConnTest("fail");
    }, 3000);
    ws.onopen = () => {
      window.clearTimeout(timeout);
      ws.close();
      setConnTest("ok");
    };
    ws.onerror = () => {
      window.clearTimeout(timeout);
      setConnTest("fail");
    };
  };

  // Dismiss on outside-click or Escape — this picker is a preferences dialog,
  // not a forced choice, so unlike ServerOfflinePrompt it is dismissible.
  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    const handleClick = (e: MouseEvent) => {
      if (panelRef.current && !panelRef.current.contains(e.target as Node)) {
        onClose();
      }
    };
    document.addEventListener("keydown", handleKey);
    document.addEventListener("mousedown", handleClick);
    return () => {
      document.removeEventListener("keydown", handleKey);
      document.removeEventListener("mousedown", handleClick);
    };
  }, [onClose]);

  const addSource = () => {
    const trimmed = customUrl.trim();
    // Mixed content is refused here, at the page-origin boundary: an https
    // page cannot open a remote `ws://` socket at all, and the browser blocks
    // it before the handshake, which is otherwise indistinguishable from an
    // unreachable server.
    const blocked = mixedContentBlockReason(trimmed);
    if (blocked) {
      setError(blocked);
      return;
    }
    const result = addUserLobbySource(trimmed);
    if (!result.ok) {
      setError(
        result.reason === "invalid_url"
          ? t("serverPicker.urlError")
          : result.reason === "duplicate"
            ? t("serverPicker.sourceDuplicate")
            : t("serverPicker.sourceCapReached", { max: MAX_USER_LOBBY_SOURCES }),
      );
      return;
    }
    setCustomUrl("");
    setError(null);
    setConnTest("idle");
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center px-4">
      <div className="absolute inset-0 bg-black/60" />
      <motion.div
        ref={panelRef}
        initial={{ opacity: 0, scale: 0.97 }}
        animate={{ opacity: 1, scale: 1 }}
        transition={{ duration: 0.15 }}
        className="relative z-10 w-full max-w-md rounded-[22px] border border-white/10 bg-[#0b1020]/96 p-5 shadow-2xl backdrop-blur-md sm:p-6"
      >
        <h2 className="text-base font-semibold text-white">{t("serverPicker.title")}</h2>
        <p className="mt-1 text-xs text-slate-400">
          {t("serverPicker.subtitle")}
        </p>

        {/* Hosting server — where this client's own games register. Separate
            from the browsed sources: you can watch many lobbies but host on
            exactly one. */}
        <div className="mt-4">
          <label className="block text-[0.6rem] uppercase tracking-[0.22em] text-slate-500">
            {t("serverPicker.hostingServer")}
          </label>
          <p className="mt-1 text-xs text-slate-500">{t("serverPicker.hostingServerHelp")}</p>
          <div className="mt-2 flex flex-col gap-2">
            {SERVER_PRESETS.map((preset) => {
              const isActive = preset.url === hostingServer;
              return (
                <button
                  key={preset.url}
                  type="button"
                  onClick={() => setHostingServer(preset.url)}
                  className={
                    "flex w-full items-center justify-between rounded-[16px] border px-4 py-2.5 text-left text-sm transition-colors "
                    + (isActive
                      ? "border-emerald-400/40 bg-emerald-500/10 text-emerald-100"
                      : "border-white/10 bg-black/18 text-gray-200 hover:border-white/18 hover:bg-white/6")
                  }
                >
                  <span className="flex min-w-0 items-center gap-2 font-medium">
                    {preset.flag && (
                      <ServerFlag
                        flag={preset.flag}
                        className="h-3.5 w-auto rounded-[2px] shadow-sm ring-1 ring-black/20"
                      />
                    )}
                    {t(preset.labelKey)}
                  </span>
                  <span className="min-w-0 truncate pl-2 font-mono text-[10px] text-slate-500">
                    {preset.url.replace(/^wss?:\/\//, "")}
                  </span>
                </button>
              );
            })}
            {/* "None" bypasses the matchmaking broker entirely. `null` is the
             * direct-codes sentinel: no lobby is browsed and `MultiplayerPage`
             * forces P2P mode, so the UI lands directly on the direct-code
             * flow without a round-trip through the offline prompt. */}
            <button
              type="button"
              onClick={() => setHostingServer(null)}
              className={
                "flex w-full items-center justify-between rounded-[16px] border px-4 py-2.5 text-left text-sm transition-colors "
                + (hostingServer === null
                  ? "border-cyan-400/40 bg-cyan-500/10 text-cyan-100"
                  : "border-white/10 bg-black/18 text-gray-200 hover:border-white/18 hover:bg-white/6")
              }
            >
              <span className="font-medium">{t("serverPicker.noneLabel")}</span>
              <span className="shrink-0 pl-2 font-mono text-[10px] text-slate-500">
                {t("serverPicker.directCodes")}
              </span>
            </button>
          </div>
        </div>

        {/* Lobby sources — every authority whose open tables are merged into
            the list. Built-in entries are rebuilt per session and cannot be
            removed; hand-added ones persist. */}
        <div className="mt-4 border-t border-white/8 pt-4">
          <label className="block text-[0.6rem] uppercase tracking-[0.22em] text-slate-500">
            {t("serverPicker.sources")}
          </label>
          <p className="mt-1 text-xs text-slate-500">{t("serverPicker.sourcesHelp")}</p>
          <ul className="mt-2 flex flex-col gap-2">
            {/* Presets and hand-added entries first. The `filter` is what stops
                an ENABLED directory entry rendering twice — once from this
                selector and once from `directoryRows` below, which is the
                single authority for every directory row in this list. */}
            {sources
              .filter((source: LobbySource) => source.origin !== "directory")
              .map((source: LobbySource) => {
              const preset = SERVER_PRESETS.find((p) => p.url === source.url);
              const status = statusLabel(sourceStatus.get(source.url)?.state);
              return (
                <li
                  key={source.url}
                  className="flex items-center justify-between gap-2 rounded-[14px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-gray-200"
                >
                  <span className="flex min-w-0 flex-col">
                    <span className="truncate font-medium">
                      {preset ? t(preset.labelKey) : source.name}
                    </span>
                    <span className="truncate font-mono text-[10px] text-slate-500">
                      {source.url.replace(/^wss?:\/\//, "")}
                    </span>
                  </span>
                  <span className="flex shrink-0 items-center gap-2">
                    {status && <span className="text-[10px] text-slate-400">{status}</span>}
                    {source.origin === "user" && (
                      <>
                        {/* A hand-typed server is a legitimate hosting target,
                            not only something to browse. */}
                        <button
                          type="button"
                          onClick={() => setHostingServer(source.url)}
                          className={menuButtonClass({
                            tone: source.url === hostingServer ? "emerald" : "neutral",
                            size: "sm",
                          })}
                        >
                          {t("serverPicker.useForHosting")}
                        </button>
                        <button
                          type="button"
                          onClick={() => removeUserLobbySource(source.url)}
                          className={menuButtonClass({ tone: "neutral", size: "sm" })}
                        >
                          {t("serverPicker.remove")}
                        </button>
                      </>
                    )}
                  </span>
                </li>
              );
            })}
            {/* Directory listings, in the same list rather than a section of
                their own: one list, distinguished by an origin badge. A row
                this client cannot speak to carries no toggle at all — it is
                never dialed, so there is nothing to enable and nothing to
                switch off, and the greyed line with the server's version is the
                whole affordance. (It does stay in `lobbySources`, and the store
                refuses it a socket on every pass — the toggle is absent because
                it would be inert, not because the row is gone.) Directory rows
                never get `Remove` or `Use for hosting`: nothing is stored to
                remove, and hosting placement over a listing is a later phase's
                decision. */}
            {directoryRows.map(({ entry, enabled }) => {
              const incompatible = entry.rejection !== null;
              const status = enabled
                ? statusLabel(sourceStatus.get(entry.source.url)?.state)
                : null;
              return (
                <li
                  key={entry.source.url}
                  className={
                    "flex items-center justify-between gap-2 rounded-[14px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-gray-200"
                    + (incompatible ? " opacity-60" : "")
                  }
                >
                  <span className="flex min-w-0 flex-col">
                    <span className="truncate font-medium">{entry.source.name}</span>
                    <span className="truncate font-mono text-[10px] text-slate-500">
                      {entry.source.url.replace(/^wss?:\/\//, "")}
                    </span>
                    {incompatible && (
                      <span className="truncate text-[10px] text-amber-300/80">
                        {t("serverPicker.incompatibleVersion", {
                          version: entry.row.server_version,
                        })}
                      </span>
                    )}
                  </span>
                  <span className="flex shrink-0 items-center gap-2">
                    <span className="text-[10px] uppercase tracking-wider text-slate-500">
                      {t("serverPicker.directoryOrigin")}
                    </span>
                    {entry.source.score !== undefined && (
                      <span className="text-[10px] text-slate-400">
                        {t("serverPicker.sourceScore", { score: entry.source.score })}
                      </span>
                    )}
                    {status && <span className="text-[10px] text-slate-400">{status}</span>}
                    {!incompatible && (
                      <button
                        type="button"
                        onClick={() =>
                          setDirectorySourceEnabled(entry.source.url, !enabled)
                        }
                        className={menuButtonClass({ tone: "neutral", size: "sm" })}
                      >
                        {t(enabled ? "serverPicker.disable" : "serverPicker.enable")}
                      </button>
                    )}
                  </span>
                </li>
              );
            })}
          </ul>

          <label className="mt-4 block text-[0.6rem] uppercase tracking-[0.22em] text-slate-500">
            {t("serverPicker.addSource")}
          </label>
          <form
            onSubmit={(e) => {
              e.preventDefault();
              if (customUrl.trim()) addSource();
            }}
            className="mt-2 flex flex-col gap-2 sm:flex-row sm:items-center"
          >
            <input
              type="text"
              value={customUrl}
              onChange={(e) => {
                setCustomUrl(e.target.value);
                setError(null);
                setConnTest("idle");
              }}
              placeholder={t("serverPicker.customUrlPlaceholder")}
              className="min-w-0 flex-1 rounded-[14px] bg-black/18 px-3 py-1.5 font-mono text-xs text-white placeholder-gray-600 outline-none ring-1 ring-white/10 focus:ring-white/20"
            />
            <div className="flex shrink-0 gap-2">
              <button
                type="button"
                onClick={() => testUrl(customUrl)}
                disabled={!customUrl.trim() || connTest === "testing"}
                className={menuButtonClass({
                  tone: "neutral",
                  size: "sm",
                  disabled: !customUrl.trim() || connTest === "testing",
                  className: "flex-1 sm:flex-none",
                })}
              >
                {t("serverPicker.test")}
              </button>
              <button
                type="submit"
                disabled={!customUrl.trim()}
                className={menuButtonClass({
                  tone: "cyan",
                  size: "sm",
                  disabled: !customUrl.trim(),
                  className: "flex-1 sm:flex-none",
                })}
              >
                {t("serverPicker.use")}
              </button>
            </div>
          </form>
          {error && (
            <p className="mt-2 text-xs text-rose-300">{error}</p>
          )}
          {connTest === "ok" && (
            <p className="mt-2 text-xs text-emerald-300">{t("serverPicker.connected")}</p>
          )}
          {connTest === "fail" && (
            <p className="mt-2 text-xs text-rose-300">{t("serverPicker.connectionFailed")}</p>
          )}
          {connTest === "testing" && (
            <p className="mt-2 text-xs text-slate-400">{t("serverPicker.testing")}</p>
          )}
        </div>

        <div className="mt-5 flex justify-end">
          <button
            type="button"
            onClick={onClose}
            className={menuButtonClass({ tone: "neutral", size: "sm" })}
          >
            {t("common:actions.close")}
          </button>
        </div>
      </motion.div>
    </div>
  );
}
