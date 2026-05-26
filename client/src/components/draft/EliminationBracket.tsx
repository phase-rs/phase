import { useTranslation } from "react-i18next";

import type { PairingView } from "../../adapter/draft-adapter";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";

// ── Round labels ────────────────────────────────────────────────────────

const ROUND_LABEL_KEYS = ["quarterfinals", "semifinals", "final"] as const;

// ── Match Card ──────────────────────────────────────────────────────────

function MatchCard({ pairing }: { pairing: PairingView }) {
  const { t } = useTranslation("draft");
  return (
    <div className="rounded-[14px] border border-white/10 bg-black/18 px-3 py-2 text-sm">
      <div
        className={
          pairing.winner_seat === pairing.seat_a
            ? "text-green-300"
            : "text-white/80"
        }
      >
        {pairing.name_a}
      </div>
      <div className="text-white/20 text-xs my-0.5">{t("bracket.versus")}</div>
      <div
        className={
          pairing.winner_seat === pairing.seat_b
            ? "text-green-300"
            : "text-white/80"
        }
      >
        {pairing.name_b}
      </div>
    </div>
  );
}

// ── Component ───────────────────────────────────────────────────────────

/** CSS grid single-elimination bracket for 8 players (3 rounds). */
export function EliminationBracket() {
  const { t } = useTranslation("draft");
  const tournamentFormat = useMultiplayerDraftStore(
    (s) => s.view?.tournament_format,
  );
  const pairings = useMultiplayerDraftStore((s) => s.pairings);

  if (tournamentFormat !== "SingleElimination") return null;

  // Group pairings by round
  const byRound = new Map<number, PairingView[]>();
  for (const p of pairings) {
    const list = byRound.get(p.round) ?? [];
    list.push(p);
    byRound.set(p.round, list);
  }

  return (
    <div className="rounded-[20px] border border-white/10 bg-black/18 p-4 shadow-[0_18px_54px_rgba(0,0,0,0.22)] backdrop-blur-md">
      <h3 className="text-lg font-medium text-white mb-4">{t("bracket.title")}</h3>
      <div className="grid grid-cols-3 gap-8">
        {ROUND_LABEL_KEYS.map((labelKey, round) => (
          <div key={round} className="flex flex-col gap-4 justify-around">
            <div className="text-xs text-white/40 text-center mb-1">
              {t(`bracket.${labelKey}`)}
            </div>
            {(byRound.get(round) ?? []).map((p) => (
              <MatchCard key={p.match_id} pairing={p} />
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}
