import { useState } from "react";
import { create } from "zustand";

import type { Zone } from "../adapter/types.ts";
import { trackEvent } from "../services/telemetry.ts";
import { useGameStore } from "../stores/gameStore.ts";

/** Identity + parse-coverage context for a single reportable card face. Built
 *  at each call site (CardPreview, the picker rows, the mobile action strip) from the
 *  live `GameObject` and the parse tree already held — counts are not recomputed
 *  here. */
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
 * Session (page-load)-scoped set of reported dedup keys (`oracle_id ?? name`).
 * Reactive so every mounted surface sharing a dedup key flips to its sent (✓)
 * state together, and the click-time guard in {@link useCardReport} makes
 * `report()` idempotent even across simultaneously-mounted duplicate rows in the
 * picker. This is the single UI-level dedup authority for every report call site;
 * the event-level session cap lives in telemetry.ts (`PER_EVENT_CAPS.card_report`).
 */
const useReportedCardsStore = create<{
  keys: Record<string, true>;
  mark: (key: string) => void;
}>()((set) => ({
  keys: {},
  mark: (key) => set((s) => (s.keys[key] ? s : { keys: { ...s.keys, [key]: true } })),
}));

/**
 * Where one surface's report for a card stands.
 * - `idle`: not reported yet; tapping sends.
 * - `sent`: a `card_report` event was queued this session. Shared by every
 *   surface showing the card, through the dedup store.
 * - `notSent`: this surface's last tap was dropped by telemetry (switched off,
 *   no ingest URL in this build, or a session cap reached), so nothing went out.
 *   Tapping again tries again. That succeeds once telemetry is switched back
 *   on; the build gate and the session caps hold for the rest of the session.
 */
export type CardReportStatus = "idle" | "sent" | "notSent";

/**
 * Shared "report this card" send logic. A single `report()` call sends one
 * `card_report` telemetry event — no modal, no confirm step, no free text. The
 * returned `status` reads `sent` from the reactive dedup store, so re-opening a
 * surface for an already-reported card shows its sent state, and multiple rows
 * sharing a dedup key flip together.
 *
 * The event shape is fixed (Grafana columns): blobs `oracle_id`, `face_name`,
 * `name`, `zone`, `game_mode`; doubles `turn`, `supported`, `total`.
 */
export function useCardReport(ctx: CardReportContext): { status: CardReportStatus; report: () => void } {
  const dedupKey = ctx.oracleId || ctx.name;
  const sent = useReportedCardsStore((s) => Boolean(s.keys[dedupKey]));
  const mark = useReportedCardsStore((s) => s.mark);
  const gameMode = useGameStore((s) => s.gameMode);
  const turn = useGameStore((s) => s.gameState?.turn_number ?? null);
  // A dropped report is local to this surface, not written to the shared store:
  // it is not a fact about the card, and a later tap (or another surface) may
  // still send it. Holding the key rather than a flag keeps the state from
  // following the surface onto a different card.
  const [droppedKey, setDroppedKey] = useState<string | null>(null);

  const report = () => {
    // Click-time guard (not just mount-init) — idempotent across duplicate rows
    // that share a dedup key and are mounted at once (4-ofs, same-named tokens).
    if (useReportedCardsStore.getState().keys[dedupKey]) return;
    const queued = trackEvent("card_report", {
      oracle_id: ctx.oracleId,
      face_name: ctx.faceName,
      name: ctx.name,
      zone: ctx.zone,
      game_mode: gameMode,
      turn,
      supported: ctx.supported,
      total: ctx.total,
    });
    // Mark the card reported only when the event was queued, so no surface
    // shows "Reported" for a report telemetry dropped (#11001).
    if (queued) {
      mark(dedupKey);
    } else {
      setDroppedKey(dedupKey);
    }
  };

  const status: CardReportStatus = sent
    ? "sent"
    : droppedKey === dedupKey
      ? "notSent"
      : "idle";

  return { status, report };
}
