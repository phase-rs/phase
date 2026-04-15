import { useEffect, useRef, useState } from "react";
import { motion } from "framer-motion";

import { isValidWebSocketUrl } from "../../services/serverDetection";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { menuButtonClass } from "../menu/buttonStyles";

interface ServerPreset {
  label: string;
  url: string;
  hint?: string;
}

/**
 * Canonical regions. Kept as a static list rather than fetched because the
 * set moves slowly and the cost of a client update is lower than the cost of
 * a runtime dependency on a region manifest endpoint.
 */
const PRESETS: ServerPreset[] = [
  { label: "US (default)", url: "wss://us.phase-rs.dev/ws" },
  { label: "EU", url: "wss://eu.phase-rs.dev/ws" },
];

interface ServerPickerProps {
  onClose: () => void;
  /** Called with the new URL after validation. Caller is responsible for any
   * re-subscription the URL change implies (e.g. remounting the lobby view). */
  onApply: (url: string) => void;
}

export function ServerPicker({ onClose, onApply }: ServerPickerProps) {
  const currentUrl = useMultiplayerStore((s) => s.serverAddress);
  const [customUrl, setCustomUrl] = useState(
    PRESETS.some((p) => p.url === currentUrl) ? "" : currentUrl,
  );
  const [error, setError] = useState<string | null>(null);
  const panelRef = useRef<HTMLDivElement>(null);

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
      setError("URL must start with ws:// or wss://");
      return;
    }
    if (trimmed !== currentUrl) {
      onApply(trimmed);
    }
    onClose();
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/60" />
      <motion.div
        ref={panelRef}
        initial={{ opacity: 0, scale: 0.97 }}
        animate={{ opacity: 1, scale: 1 }}
        transition={{ duration: 0.15 }}
        className="relative z-10 w-full max-w-md rounded-[22px] border border-white/10 bg-[#0b1020]/96 p-6 shadow-2xl backdrop-blur-md"
      >
        <h2 className="text-base font-semibold text-white">Server</h2>
        <p className="mt-1 text-xs text-slate-400">
          Pick a region, or connect to a self-hosted instance.
        </p>

        <div className="mt-4 flex flex-col gap-2">
          {PRESETS.map((preset) => {
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
                <span className="font-medium">{preset.label}</span>
                <span className="font-mono text-[10px] text-slate-500">
                  {preset.url.replace(/^wss?:\/\//, "")}
                </span>
              </button>
            );
          })}
        </div>

        <div className="mt-4 border-t border-white/8 pt-4">
          <label className="block text-[0.6rem] uppercase tracking-[0.22em] text-slate-500">
            Self-hosted
          </label>
          <form
            onSubmit={(e) => {
              e.preventDefault();
              if (customUrl.trim()) applyUrl(customUrl);
            }}
            className="mt-2 flex gap-2"
          >
            <input
              type="text"
              value={customUrl}
              onChange={(e) => {
                setCustomUrl(e.target.value);
                setError(null);
              }}
              placeholder="wss://your-server.example/ws"
              className="flex-1 rounded-[14px] bg-black/18 px-3 py-1.5 font-mono text-xs text-white placeholder-gray-600 outline-none ring-1 ring-white/10 focus:ring-white/20"
            />
            <button
              type="submit"
              disabled={!customUrl.trim()}
              className={menuButtonClass({
                tone: "cyan",
                size: "sm",
                disabled: !customUrl.trim(),
              })}
            >
              Use
            </button>
          </form>
          {error && (
            <p className="mt-2 text-xs text-rose-300">{error}</p>
          )}
        </div>

        <div className="mt-5 flex justify-end">
          <button
            type="button"
            onClick={onClose}
            className={menuButtonClass({ tone: "neutral", size: "sm" })}
          >
            Close
          </button>
        </div>
      </motion.div>
    </div>
  );
}
