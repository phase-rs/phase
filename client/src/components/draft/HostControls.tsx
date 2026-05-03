import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";
import { menuButtonClass } from "../menu/buttonStyles";

// ── Component ───────────────────────────────────────────────────────────

/**
 * Floating host-only control panel for tournament management.
 * Renders nothing when the local player is not the host.
 */
export function HostControls() {
  const role = useMultiplayerDraftStore((s) => s.role);
  const phase = useMultiplayerDraftStore((s) => s.phase);
  const podPolicy = useMultiplayerDraftStore((s) => s.view?.pod_policy);
  const paused = useMultiplayerDraftStore((s) => s.paused);
  const advanceRound = useMultiplayerDraftStore((s) => s.advanceRound);
  const requestPause = useMultiplayerDraftStore((s) => s.requestPause);
  const requestResume = useMultiplayerDraftStore((s) => s.requestResume);
  const pairings = useMultiplayerDraftStore((s) => s.pairings);
  const overrideMatchResult = useMultiplayerDraftStore(
    (s) => s.overrideMatchResult,
  );
  const replaceSeatWithBot = useMultiplayerDraftStore(
    (s) => s.replaceSeatWithBot,
  );
  const seats = useMultiplayerDraftStore((s) => s.view?.seats ?? []);

  if (role !== "host") return null;

  // Only show when there are contextual controls to display
  const showPauseResume = phase === "drafting";
  const showAdvanceRound =
    podPolicy === "Casual" && phase === "roundComplete";
  const showOverride =
    podPolicy === "Casual" &&
    phase === "matchInProgress" &&
    pairings.length > 0;
  const humanSeats = seats.filter((s) => !s.is_bot);
  const showKickReplace =
    humanSeats.length > 0 &&
    (phase === "matchInProgress" || phase === "roundComplete");

  if (
    !showPauseResume &&
    !showAdvanceRound &&
    !showOverride &&
    !showKickReplace
  )
    return null;

  return (
    <div className="fixed bottom-4 right-4 z-50 rounded-xl border border-white/10 bg-gray-900/95 backdrop-blur-sm p-3 flex flex-col gap-2 min-w-[180px]">
      <div className="text-xs text-white/40 uppercase tracking-wider">
        Host Controls
      </div>

      {/* Pause/Resume — available during drafting */}
      {showPauseResume && (
        <button
          onClick={paused ? requestResume : requestPause}
          className={menuButtonClass({
            tone: paused ? "emerald" : "neutral",
            size: "sm",
          })}
        >
          {paused ? "Resume Draft" : "Pause Draft"}
        </button>
      )}

      {/* Advance Round — Casual mode only, when round is complete */}
      {showAdvanceRound && (
        <button
          onClick={advanceRound}
          className={menuButtonClass({ tone: "blue", size: "sm" })}
        >
          Start Next Round
        </button>
      )}

      {/* Override match result — Casual mode, during matches */}
      {showOverride && (
        <div className="flex flex-col gap-1">
          <div className="text-xs text-white/40">Override Result</div>
          {pairings
            .filter((p) => p.status !== "Complete")
            .map((p) => (
              <div
                key={p.match_id}
                className="flex items-center gap-1 text-xs"
              >
                <span className="text-white/60 truncate">
                  {p.name_a} v {p.name_b}
                </span>
                <button
                  onClick={() => overrideMatchResult(p.match_id, p.seat_a)}
                  className="px-1 py-0.5 text-emerald-400/70 hover:text-emerald-300 text-xs"
                >
                  {p.name_a.split(" ")[0]}
                </button>
                <button
                  onClick={() => overrideMatchResult(p.match_id, p.seat_b)}
                  className="px-1 py-0.5 text-emerald-400/70 hover:text-emerald-300 text-xs"
                >
                  {p.name_b.split(" ")[0]}
                </button>
              </div>
            ))}
        </div>
      )}

      {/* Kick + Replace with Bot — D-08 */}
      {showKickReplace && (
        <div className="flex flex-col gap-1">
          <div className="text-xs text-white/40">Kick + Replace</div>
          {humanSeats.map((s) => (
            <button
              key={s.seat_index}
              onClick={() => replaceSeatWithBot(s.seat_index)}
              className="text-left px-2 py-1 text-xs text-red-400/70 hover:text-red-300 hover:bg-white/5 rounded transition-colors"
            >
              Replace {s.display_name} with Bot
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
