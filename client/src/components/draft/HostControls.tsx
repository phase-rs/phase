import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";
import { useDraftPodStore } from "../../stores/draftPodStore";
import { menuButtonClass } from "../menu/buttonStyles";

const EMPTY_SEATS: Array<{ seat_index: number; display_name: string; is_bot: boolean; connected: boolean }> = [];

function winnerChoiceClass(selected: boolean): string {
  return menuButtonClass({
    tone: selected ? "emerald" : "neutral",
    size: "xs",
    className: "min-w-0 flex-1 justify-center px-2",
  });
}

// ── Component ───────────────────────────────────────────────────────────

/**
 * Floating host-only control panel for tournament management.
 * Renders nothing when the local player is not the host.
 */
export function HostControls() {
  const { t } = useTranslation("draft");
  const navigate = useNavigate();
  const [endingDraft, setEndingDraft] = useState(false);
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
  const leave = useMultiplayerDraftStore((s) => s.leave);
  const resetPod = useDraftPodStore((s) => s.reset);
  const replaceSeatWithBot = useMultiplayerDraftStore(
    (s) => s.replaceSeatWithBot,
  );
  const seats = useMultiplayerDraftStore((s) => s.view?.seats ?? EMPTY_SEATS);

  if (role !== "host") return null;

  // Only show when there are contextual controls to display
  const showPauseResume = phase === "drafting";
  const showAdvanceRound =
    podPolicy === "Casual" && phase === "roundComplete";
  const showOverride =
    podPolicy === "Casual" &&
    (phase === "matchInProgress" || phase === "roundComplete") &&
    pairings.length > 0;
  const humanSeats = seats.filter((s) => !s.is_bot);
  const showKickReplace =
    humanSeats.length > 0 &&
    (phase === "matchInProgress" || phase === "roundComplete");
  const showEndDraft = ![
    "idle",
    "connecting",
    "complete",
    "error",
    "kicked",
    "hostLeft",
  ].includes(phase);

  const handleEndDraft = async () => {
    if (endingDraft) return;
    if (!window.confirm(t("hostControls.endDraftConfirm"))) return;

    setEndingDraft(true);
    try {
      await leave(false);
      resetPod();
      navigate("/");
    } catch (err) {
      console.error("[HostControls] failed to end draft:", err);
      setEndingDraft(false);
    }
  };

  if (
    !showPauseResume &&
    !showAdvanceRound &&
    !showOverride &&
    !showKickReplace &&
    !showEndDraft
  )
    return null;

  return (
    <div className="fixed bottom-4 right-4 z-50 flex min-w-[180px] flex-col gap-2 rounded-[18px] border border-white/10 bg-black/18 p-3 shadow-[0_18px_54px_rgba(0,0,0,0.22)] backdrop-blur-md">
      <div className="text-[0.68rem] uppercase tracking-[0.18em] text-white/40">
        {t("hostControls.title")}
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
          {paused ? t("hostControls.resumeDraft") : t("hostControls.pauseDraft")}
        </button>
      )}

      {/* Advance Round — Casual mode only, when round is complete */}
      {showAdvanceRound && (
        <button
          onClick={advanceRound}
          className={menuButtonClass({ tone: "blue", size: "sm" })}
        >
          {t("hostControls.startNextRound")}
        </button>
      )}

      {/* Override match result — Casual mode, during matches */}
      {showOverride && (
        <div className="flex flex-col gap-2">
          <div className="text-xs text-white/40">{t("hostControls.overrideResult")}</div>
          {pairings.map((p) => {
            const seatAWon = p.winner_seat === p.seat_a;
            const seatBWon = p.winner_seat === p.seat_b;

            return (
              <div key={p.match_id} className="flex flex-col gap-1">
                <div className="grid grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-center gap-1 text-xs">
                  <button
                    onClick={() => overrideMatchResult(p.match_id, p.seat_a)}
                    aria-pressed={seatAWon}
                    className={winnerChoiceClass(seatAWon)}
                    title={p.name_a}
                  >
                    {seatAWon && <span aria-hidden="true">✓</span>}
                    <span className="truncate">{p.name_a}</span>
                  </button>
                  <span className="px-1 text-[0.62rem] uppercase tracking-[0.14em] text-white/30">
                    {t("standings.versus")}
                  </span>
                  <button
                    onClick={() => overrideMatchResult(p.match_id, p.seat_b)}
                    aria-pressed={seatBWon}
                    className={winnerChoiceClass(seatBWon)}
                    title={p.name_b}
                  >
                    {seatBWon && <span aria-hidden="true">✓</span>}
                    <span className="truncate">{p.name_b}</span>
                  </button>
                </div>
                <div
                  className="truncate text-[0.64rem] uppercase tracking-[0.14em] text-white/30"
                  title={`${p.match_id} ${p.status}`}
                >
                  {p.match_id} - {p.status}
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* Kick + Replace with Bot — D-08 */}
      {showKickReplace && (
        <div className="flex flex-col gap-1">
          <div className="text-xs text-white/40">{t("hostControls.kickReplace")}</div>
          {humanSeats.map((s) => (
            <button
              key={s.seat_index}
              onClick={() => replaceSeatWithBot(s.seat_index)}
              className="text-left px-2 py-1 text-xs text-red-400/70 hover:text-red-300 hover:bg-white/5 rounded transition-colors"
            >
              {t("hostControls.replaceWithBot", { name: s.display_name })}
            </button>
          ))}
        </div>
      )}

      {showEndDraft && (
        <button
          onClick={() => void handleEndDraft()}
          disabled={endingDraft}
          className={menuButtonClass({
            tone: "red",
            size: "sm",
            disabled: endingDraft,
            className: "mt-1",
          })}
        >
          {t("hostControls.endDraft")}
        </button>
      )}
    </div>
  );
}
