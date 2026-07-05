import { useState } from "react";
import { useTranslation } from "react-i18next";

import type { Zone } from "../../adapter/types.ts";
import { trackEvent } from "../../services/telemetry.ts";
import { useGameStore } from "../../stores/gameStore.ts";

/** Cards reported this page-load, keyed by `oracle_id ?? name`, so a card
 *  already reported renders its sent state when re-opened. This is UI-level
 *  dedup; the event-level session cap lives in telemetry.ts
 *  (`PER_EVENT_CAPS.card_report`). */
const reportedThisSession = new Set<string>();

/** Identity + parse-coverage context for a single reportable card face. Built
 *  at the CardPreview call site from the live `GameObject` and the parse tree
 *  the panel already holds (counts are not recomputed here). */
export interface CardReportContext {
  /** `""` for tokens/emblems (no `printed_ref`); `name` is the dedup fallback. */
  oracleId: string;
  faceName: string;
  name: string;
  zone: Zone;
  /** Supported / total parsed items for the displayed face. */
  supported: number;
  total: number;
}

/**
 * One-click "report this card" affordance. A single click sends a `card_report`
 * telemetry event — no modal, no confirm step, no free text. The existing
 * GitHub-issue and Discord report flows are untouched; this is the
 * low-friction, identity-free counter. Callers only build a
 * {@link CardReportContext} in a live, participating (non-spectate) game — the
 * guard lives at the call site so the button's styled wrapper elements stay
 * out of the DOM too, not just the button itself.
 *
 * Callers pass `key={oracleId || name}` so switching cards remounts the button
 * and re-derives the sent state from {@link reportedThisSession}.
 */
export function ReportCardButton({ oracleId, faceName, name, zone, supported, total }: CardReportContext) {
  const { t } = useTranslation("game");
  const gameMode = useGameStore((s) => s.gameMode);
  const turn = useGameStore((s) => s.gameState?.turn_number ?? null);
  const dedupKey = oracleId || name;
  const [sent, setSent] = useState(() => reportedThisSession.has(dedupKey));

  const handleClick = () => {
    if (sent) return;
    reportedThisSession.add(dedupKey);
    setSent(true);
    trackEvent("card_report", {
      oracle_id: oracleId,
      face_name: faceName,
      name,
      zone,
      game_mode: gameMode,
      turn,
      supported,
      total,
    });
  };

  return (
    <button
      type="button"
      onClick={handleClick}
      disabled={sent}
      className={
        sent
          ? "pointer-events-auto text-[11px] font-medium text-emerald-400"
          : "pointer-events-auto text-[11px] text-indigo-300 hover:text-indigo-200"
      }
    >
      {sent ? t("preview.reported") : t("preview.report")}
    </button>
  );
}
