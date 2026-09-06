import { useTranslation } from "react-i18next";

import type { ConnectionMode } from "../../stores/multiplayerStore";

/**
 * Selected-segment styling per mode. This is the app's EXISTING colour
 * language for the two connection modes — emerald for dedicated hosting,
 * cyan for P2P (`HostSetup`'s `accentTone`, which the rest of this form
 * follows) — which until now was only ever shown and never explained.
 */
const SELECTED_TONE: Record<ConnectionMode, string> = {
  server:
    "bg-emerald-400/15 text-emerald-100 shadow-[inset_0_0_0_1px] shadow-emerald-300/25",
  p2p: "bg-cyan-400/15 text-cyan-100 shadow-[inset_0_0_0_1px] shadow-cyan-300/25",
};

/** Render order. Neither mode is recommended over the other; server is first
 *  only because it is the auto-assigned default. */
const MODES: readonly ConnectionMode[] = ["server", "p2p"];

/**
 * The authority on which transport a HOSTED session uses: a dedicated server
 * or a direct peer-to-peer connection. Rendered at the top of Host Game and
 * nowhere else — it configures a game being created, alongside the room name,
 * password and lobby listing. Browsing and joining are deliberately not
 * governed by it: `MultiplayerPage` routes a join on the shape of the code.
 *
 * Buttons with `aria-pressed`, matching the incumbent button-based segmented
 * control (`components/draft/BotDifficultySelector`). `role="radio"` is
 * deliberately NOT used: without roving tabindex and arrow-key handling it
 * reads worse than a pressed-button group. The group carries its own
 * `aria-label`, so a surface may render a visible label beside it without
 * having to associate one.
 */
export function ConnectionModeSwitch({
  value,
  onChange,
}: {
  value: ConnectionMode;
  onChange: (mode: ConnectionMode) => void;
}) {
  const { t } = useTranslation("multiplayer");

  return (
    <div
      role="group"
      aria-label={t("connectionMode.label")}
      className="flex rounded-xl border border-white/10 bg-black/18 p-1 backdrop-blur-md"
    >
      {MODES.map((mode) => {
        const selected = value === mode;
        return (
          <button
            key={mode}
            type="button"
            onClick={() => onChange(mode)}
            aria-pressed={selected}
            className={`min-h-11 flex-1 cursor-pointer whitespace-nowrap rounded-lg px-2 py-2 text-xs font-medium transition-colors ${
              selected
                ? SELECTED_TONE[mode]
                : "text-white/45 hover:bg-white/[0.05] hover:text-white/70"
            }`}
          >
            {t(`connectionMode.${mode}`)}
          </button>
        );
      })}
    </div>
  );
}
