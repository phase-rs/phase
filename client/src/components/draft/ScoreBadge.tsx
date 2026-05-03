import type { MatchScore } from "../../adapter/types";

// ── Props ─────────────────────────────────────────────────────────────

interface ScoreBadgeProps {
  /** The current match score (Bo3). */
  score: MatchScore;
  /** Which player's perspective to render (0 or 1). */
  player: 0 | 1;
  /** Optional size variant. Defaults to "sm". */
  size?: "sm" | "md";
}

// ── Component ─────────────────────────────────────────────────────────

/**
 * Arena-style match score indicator: filled dots for wins, empty dots for
 * remaining games needed to win (Bo3 = first to 2). Rendered inline next
 * to player names in the HUD and between-games screens.
 */
export function ScoreBadge({ score, player, size = "sm" }: ScoreBadgeProps) {
  const wins = player === 0 ? score.p0_wins : score.p1_wins;
  const winsNeeded = 2; // Bo3: first to 2

  const dotSize = size === "md" ? "h-2.5 w-2.5" : "h-2 w-2";

  return (
    <div className="flex items-center gap-0.5" aria-label={`Score: ${wins} wins`}>
      {Array.from({ length: winsNeeded }, (_, i) => (
        <span
          key={i}
          className={`${dotSize} rounded-full ${
            i < wins
              ? "bg-amber-400 shadow-[0_0_4px_rgba(251,191,36,0.6)]"
              : "border border-white/30 bg-transparent"
          }`}
        />
      ))}
    </div>
  );
}
