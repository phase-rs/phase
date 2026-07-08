import { create } from "zustand";

import type { Zone } from "../adapter/types.ts";
import { trackEvent } from "../services/telemetry.ts";
import { useGameStore } from "../stores/gameStore.ts";

/** Identity + parse-coverage context for a single reportable card face. Built
 *  at each call site (CardPreview, the picker rows, the mobile pill) from the
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
 * Shared "report this card" send logic. A single `report()` call sends one
 * `card_report` telemetry event — no modal, no confirm step, no free text. The
 * returned `sent` reflects the reactive dedup store, so re-opening a surface for
 * an already-reported card shows its sent state, and multiple rows sharing a
 * dedup key flip together.
 *
 * The event shape is fixed (Grafana columns): blobs `oracle_id`, `face_name`,
 * `name`, `zone`, `game_mode`; doubles `turn`, `supported`, `total`.
 */
export function useCardReport(ctx: CardReportContext): { sent: boolean; report: () => void } {
  const dedupKey = ctx.oracleId || ctx.name;
  const sent = useReportedCardsStore((s) => Boolean(s.keys[dedupKey]));
  const mark = useReportedCardsStore((s) => s.mark);
  const gameMode = useGameStore((s) => s.gameMode);
  const turn = useGameStore((s) => s.gameState?.turn_number ?? null);

  const report = () => {
    // Click-time guard (not just mount-init) — idempotent across duplicate rows
    // that share a dedup key and are mounted at once (4-ofs, same-named tokens).
    if (useReportedCardsStore.getState().keys[dedupKey]) return;
    mark(dedupKey);
    trackEvent("card_report", {
      oracle_id: ctx.oracleId,
      face_name: ctx.faceName,
      name: ctx.name,
      zone: ctx.zone,
      game_mode: gameMode,
      turn,
      supported: ctx.supported,
      total: ctx.total,
    });
  };

  return { sent, report };
}
