import { useEffect, useRef, useState } from "react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import {
  SERVER_PRESETS,
  isValidWebSocketUrl,
} from "../../services/serverDetection";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { menuButtonClass } from "../menu/buttonStyles";
import { ServerFlag } from "./ServerFlag";

interface ServerPickerProps {
  onClose: () => void;
  /** Called with the new URL after validation. Caller is responsible for any
   * re-subscription the URL change implies (e.g. remounting the lobby view). */
  onApply: (url: string) => void;
}

type ConnTestState = "idle" | "testing" | "ok" | "fail";

export function ServerPicker({ onClose, onApply }: ServerPickerProps) {
  const { t } = useTranslation("multiplayer");
  const currentUrl = useMultiplayerStore((s) => s.serverAddress);
  const [customUrl, setCustomUrl] = useState(
    SERVER_PRESETS.some((p) => p.url === currentUrl) ? "" : currentUrl,
  );
  const [error, setError] = useState<string | null>(null);
  const [connTest, setConnTest] = useState<ConnTestState>("idle");
  const panelRef = useRef<HTMLDivElement>(null);

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

  const applyUrl = (url: string) => {
    const trimmed = url.trim();
    if (!isValidWebSocketUrl(trimmed)) {
      setError(t("serverPicker.urlError"));
      return;
    }
    if (trimmed !== currentUrl) {
      onApply(trimmed);
    }
    onClose();
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

        <div className="mt-4 flex flex-col gap-2">
          {SERVER_PRESETS.map((preset) => {
            const isActive = preset.url === currentUrl;
            return (
              <button
                key={preset.url}
                type="button"
                onClick={() => applyUrl(preset.url)}
                className={
                  "flex w-full items-center justify-between rounded-[16px] border px-4 py-2.5 text-left text-sm transition-colors " +
                  (isActive
                    ? "border-emerald-400/40 bg-emerald-500/10 text-emerald-100"
                    : "border-white/10 bg-black/18 text-gray-200 hover:border-white/18 hover:bg-white/6")
                }
              >
                <span className="flex min-w-0 items-center gap-2 font-medium">
                  <ServerFlag
                    flag={preset.flag}
                    className="h-3.5 w-auto rounded-[2px] shadow-sm ring-1 ring-black/20"
                  />
                  {preset.label}
                </span>
                <span className="min-w-0 truncate pl-2 font-mono text-[10px] text-slate-500">
                  {preset.url.replace(/^wss?:\/\//, "")}
                </span>
              </button>
            );
          })}
          {/* "None" bypasses the matchmaking broker entirely. Empty string is
           * the sentinel: `ensureSubscriptionSocket` rejects it via
           * `isValidWebSocketUrl` and `MultiplayerPage` forces P2P mode, so
           * the UI lands directly on the direct-code flow without a round-
           * trip through the offline prompt. */}
          <button
            type="button"
            onClick={() => {
              if (currentUrl !== "") onApply("");
              onClose();
            }}
            className={
              "flex w-full items-center justify-between rounded-[16px] border px-4 py-2.5 text-left text-sm transition-colors " +
              (currentUrl === ""
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

        <div className="mt-4 border-t border-white/8 pt-4">
          <label className="block text-[0.6rem] uppercase tracking-[0.22em] text-slate-500">
            {t("serverPicker.selfHosted")}
          </label>
          <form
            onSubmit={(e) => {
              e.preventDefault();
              if (customUrl.trim()) applyUrl(customUrl);
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
