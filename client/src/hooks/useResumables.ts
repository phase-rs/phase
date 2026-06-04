import { useEffect, useState } from "react";
import { useNavigate } from "react-router";

import { loadP2PSession } from "../services/p2pSession";
import { loadWsSession } from "../services/multiplayerSession";
import {
  loadActiveQuickDraft,
  type ActiveQuickDraftMeta,
} from "../services/quickDraftPersistence";
import {
  loadActiveDraftPod,
  type ActiveDraftPodMeta,
} from "../services/draftPersistence";
import {
  clearActiveGame,
  loadActiveGame,
  loadGame,
  useGameStore,
  type ActiveGameMeta,
} from "../stores/gameStore";

/** Live state of the saved local/AI match, read from its persisted GameState.
 *  Absent for online/P2P-guest matches (their state isn't held locally).
 *  Carries human-readable context for the resume hero, not raw board data. */
export interface MatchSummary {
  turn: number;
  /** True when the local human (seat 0) has the active turn. */
  isYourTurn: boolean;
  /** Life of the local human (seat 0); null when eliminated or absent. */
  yourLife: number | null;
  /** Non-eliminated opponents (every live seat except the human). */
  opponentCount: number;
}

export interface Resumables {
  /** The saved in-progress match, validated against its persisted session. */
  match: ActiveGameMeta | null;
  /** Turn/life snapshot of the saved match (local/AI only). */
  matchSummary: MatchSummary | null;
  quickDraft: ActiveQuickDraftMeta | null;
  pod: ActiveDraftPodMeta | null;
  /** Resume the saved match (mirrors the menu's resume routing). */
  resumeMatch: () => void;
}

/**
 * Single source of truth for "what can I resume?" — the saved AI/online/P2P
 * match (validated against its persisted session, clearing stale entries) plus
 * any in-progress quick draft or draft pod. Shared by the home dashboard and the
 * draft landing page so the detection logic lives in one place.
 */
export function useResumables(): Resumables {
  const navigate = useNavigate();
  const [match, setMatch] = useState<ActiveGameMeta | null>(null);
  const [matchSummary, setMatchSummary] = useState<MatchSummary | null>(null);
  const [quickDraft, setQuickDraft] = useState<ActiveQuickDraftMeta | null>(null);
  const [pod, setPod] = useState<ActiveDraftPodMeta | null>(null);

  useEffect(() => {
    setQuickDraft(loadActiveQuickDraft());
    setPod(loadActiveDraftPod());

    const saved = loadActiveGame();
    if (!saved) return;

    // Validate the saved match against the session/state that actually backs it,
    // clearing the pointer when the backing data is gone so we never offer a
    // resume that would fail.
    if (saved.mode === "online") {
      if (loadWsSession() !== null) setMatch(saved);
      else clearActiveGame();
    } else if (saved.mode === "p2p-join" && saved.p2pRoomCode) {
      loadP2PSession(`phase-${saved.p2pRoomCode}`).then((session) => {
        if (session) setMatch(saved);
        else clearActiveGame();
      });
    } else {
      loadGame(saved.id).then((state) => {
        if (state) {
          setMatch(saved);
          // CR 800.4: eliminated players are out — only live seats count as
          // opponents. Seat 0 is the local human in AI/host matches.
          const you = state.players.find((p) => p.id === 0);
          const liveCount = state.players.filter((p) => !p.is_eliminated).length;
          setMatchSummary({
            turn: state.turn_number,
            isYourTurn: state.active_player === 0,
            yourLife: you && !you.is_eliminated ? you.life : null,
            opponentCount: Math.max(0, liveCount - 1),
          });
        } else {
          clearActiveGame();
        }
      });
    }
  }, []);

  const resumeMatch = () => {
    if (!match) return;
    useGameStore.setState({ gameId: match.id });
    if (match.mode === "online") {
      navigate(`/game/${match.id}?mode=host`);
    } else if (match.mode === "p2p-host") {
      navigate(`/game/${match.id}?mode=p2p-host`);
    } else if (match.mode === "p2p-join" && match.p2pRoomCode) {
      navigate(`/game/${match.id}?mode=p2p-join&code=${match.p2pRoomCode}`);
    } else {
      // Multi-AI resume needs `players` so every AI seat respawns (one entry per
      // AI seat → +1 for the human). Older saves fall back to the 2-player default.
      const seatCount = match.aiSeats?.length;
      const playersParam = seatCount && seatCount > 1 ? `&players=${seatCount + 1}` : "";
      navigate(`/game/${match.id}?mode=${match.mode}&difficulty=${match.difficulty}${playersParam}`);
    }
  };

  return { match, matchSummary, quickDraft, pod, resumeMatch };
}
