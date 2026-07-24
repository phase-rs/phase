import { useTranslation } from "react-i18next";

import { canAttemptNativeEngine } from "../../services/nativeEngine";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";

/**
 * Why this game is in-browser, in the vocabulary `setEngineMode` records.
 *
 * A null reason is not a failure: it is every game that never asked for the
 * native engine (draft matches, a chosen first player, resuming a WASM save),
 * where claiming the engine is unavailable would be false on a machine whose
 * native engine is working fine.
 */
function inBrowserTooltipKey(fallbackReason: string | null): string {
  switch (fallbackReason) {
    case "server_version_mismatch":
      return "engineBadge.versionMismatchTooltip";
    case "native_engine_unavailable":
      return "engineBadge.inBrowserTooltip";
    default:
      return "engineBadge.notAttemptedTooltip";
  }
}

/**
 * Which engine is driving the current game, shown beside the game menu button.
 *
 * The client silently falls back to the in-browser engine whenever the desktop
 * shell cannot provide its native server, which is otherwise invisible — the
 * game just plays slower. This badge is the standing signal; the fallback also
 * raises a one-shot toast from GameProvider.
 *
 * Rendered only where native is a real possibility (desktop shell, first-party
 * origin, preference on) and only for games that pick a transport at all —
 * `engineMode` stays null outside solo-AI games. Everywhere else every game is
 * in-browser by definition and the badge would be noise.
 */
export function EngineModeBadge() {
  const { t } = useTranslation();
  const engineMode = useGameStore((state) => state.engineMode);
  const fallbackReason = useGameStore((state) => state.nativeEngineFallbackReason);
  const nativeEngineEnabled = usePreferencesStore((state) => state.nativeEngineEnabled);

  if (engineMode === null || !canAttemptNativeEngine(nativeEngineEnabled)) return null;

  const isNative = engineMode === "native";
  const tooltip = isNative
    ? t("engineBadge.nativeTooltip")
    : t(inBrowserTooltipKey(fallbackReason));

  return (
    <span
      title={tooltip}
      className={`flex h-7 select-none items-center gap-1.5 rounded-full border px-2.5 text-[10px] font-semibold backdrop-blur-md ${
        isNative
          ? "border-cyan-200/45 bg-slate-950/84 text-cyan-100"
          : "border-amber-400/45 bg-amber-500/10 text-amber-200"
      }`}
    >
      <span className="h-1.5 w-1.5 rounded-full bg-current" aria-hidden />
      {isNative ? t("engineBadge.native") : t("engineBadge.inBrowser")}
    </span>
  );
}
