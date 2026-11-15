import { useEffect, useState } from "react";
import { useNavigate } from "react-router";

import { persistedGameStateView } from "../adapter/types";
import { loadP2PSession } from "../services/p2pSession";
import { loadWsSession } from "../services/multiplayerSession";
import {
  canAttemptNativeEngine,
  nativeEngineKeyForCurrentOrigin,
} from "../services/nativeEngine";
import { usePreferencesStore } from "../stores/preferencesStore";
import {
  loadActiveQuickDraft,
  type ActiveQuickDraftMeta,
} from "../services/quickDraftPersistence";
import {
  loadActiveDraftPod,
  type ActiveDraftPodMeta,
} from "../services/draftPersistence";
import {
  ACTIVE_GAME_CHANGED_EVENT,
  activeGamePointerToken,
  clearActiveGameIfUnchanged,
} from "../services/gamePersistence";
import {
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
  /** A finished match whose result is retained locally and still owes the
   *  server an acknowledgement. Reopening it displays the result and retries
   *  the ACK; it is never offered as a playable resume. */
  retainedResult: ActiveGameMeta | null;
  quickDraft: ActiveQuickDraftMeta | null;
  pod: ActiveDraftPodMeta | null;
  /** Resume the saved match (mirrors the menu's resume routing). */
  resumeMatch: () => void;
  /** Re-open the retained result, which also retries its acknowledgement. */
  viewRetainedResult: () => void;
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
  const [retainedResult, setRetainedResult] = useState<ActiveGameMeta | null>(null);
  const [quickDraft, setQuickDraft] = useState<ActiveQuickDraftMeta | null>(null);
  const [pod, setPod] = useState<ActiveDraftPodMeta | null>(null);

  useEffect(() => {
    let cancelled = false;
    // Monotonic identity for one validation pass. A session/state lookup
    // belongs to the pass that started it; a storage notification supersedes
    // it, and a stale reply must neither write state nor clear the replacement
    // pointer the newer pass validated.
    let validation = 0;

    const validate = () => {
      const generation = ++validation;
      const isCurrent = () => !cancelled && generation === validation;

      setQuickDraft(loadActiveQuickDraft());
      setPod(loadActiveDraftPod());

      const pointerToken = activeGamePointerToken();
      const saved = loadActiveGame();
      setRetainedResult(null);
      if (!saved) {
        setMatch(null);
        setMatchSummary(null);
        return;
      }

      // Validate the saved match against the session/state that actually backs it,
      // clearing the pointer when the backing data is gone so we never offer a
      // resume that would fail. The session/state lookups are async, so guard their
      // setState against a mid-flight unmount (the cancelled pattern the other
      // async hooks already use).
      if (saved.mode === "online") {
        const wsSession = loadWsSession();
        if (wsSession?.terminalOnly === true) {
          // The seat's result is committed locally and still owes the server an
          // acknowledgement. It cannot resume gameplay, but it must stay
          // reachable: reopening it re-displays the result and retries the ACK.
          setMatch(null);
          setMatchSummary(null);
          setRetainedResult(saved);
        } else if (wsSession !== null) {
          setMatch(saved);
        } else {
          setMatch(null);
          setMatchSummary(null);
          clearActiveGameIfUnchanged(pointerToken);
        }
      } else if (saved.mode === "p2p-join" && saved.p2pRoomCode) {
        loadP2PSession(`phase-${saved.p2pRoomCode}`).then((session) => {
          if (!isCurrent()) return;
          if (session) {
            setMatch(saved);
          } else {
            setMatch(null);
            setMatchSummary(null);
            clearActiveGameIfUnchanged(pointerToken);
          }
        });
      } else if (saved.nativeSession) {
        // Native solo (AI) games are server-authoritative: their state lives in
        // the local phase-server, not IndexedDB, so there is no snapshot to
        // validate against here. The reconnect on resume is the real validation.
        // No matchSummary — turn/life aren't available client-side until we
        // reconnect, so the resume hero shows the match without a board snapshot.
        //
        // Only offer it when the native engine can actually be attached
        // (enabled + available for this origin). Resuming routes back through the
        // native path, which needs the engine; if it is unavailable the resume
        // would dead-end at a fresh in-browser game. Keep the pointer silently so
        // the resume reappears once native is available again — don't clear it.
        const nativeAvailable =
          canAttemptNativeEngine(usePreferencesStore.getState().nativeEngineEnabled)
          && nativeEngineKeyForCurrentOrigin() !== null;
        // A terminal-only session has finished and cannot resume gameplay. Its
        // retained result is offered instead (reopen + retry the ACK) whenever
        // the native engine can be attached; otherwise the pointer is kept
        // silently so the action reappears once native is available.
        if (saved.nativeSession.terminalOnly) {
          setMatch(null);
          setMatchSummary(null);
          if (nativeAvailable) setRetainedResult(saved);
          return;
        }
        if (nativeAvailable) {
          setMatch(saved);
        } else {
          setMatch(null);
          setMatchSummary(null);
        }
      } else {
        loadGame(saved.id).then((state) => {
          if (!isCurrent()) return;
          if (state) {
            const publicState = persistedGameStateView(state);
            setMatch(saved);
            // CR 800.4: eliminated players are out — only live seats count as
            // opponents. Seat 0 is the local human in AI/host matches.
            const you = publicState.players.find((p) => p.id === 0);
            const liveCount = publicState.players.filter((p) => !p.is_eliminated).length;
            setMatchSummary({
              turn: publicState.turn_number,
              isYourTurn: publicState.active_player === 0,
              yourLife: you && !you.is_eliminated ? you.life : null,
              opponentCount: Math.max(0, liveCount - 1),
            });
          } else {
            setMatch(null);
            setMatchSummary(null);
            clearActiveGameIfUnchanged(pointerToken);
          }
        });
      }
    };

    validate();
    window.addEventListener("storage", validate);
    window.addEventListener(ACTIVE_GAME_CHANGED_EVENT, validate);

    return () => {
      cancelled = true;
      window.removeEventListener("storage", validate);
      window.removeEventListener(ACTIVE_GAME_CHANGED_EVENT, validate);
    };
  }, []);

  /** Route to a saved match's game page (the routes `resumeMatch` mirrors). */
  const openMatch = (meta: ActiveGameMeta) => {
    useGameStore.setState({ gameId: meta.id });
    if (meta.mode === "online") {
      navigate(`/game/${meta.id}?mode=host`);
    } else if (meta.mode === "p2p-host") {
      navigate(`/game/${meta.id}?mode=p2p-host`);
    } else if (meta.mode === "p2p-join" && meta.p2pRoomCode) {
      navigate(`/game/${meta.id}?mode=p2p-join&code=${meta.p2pRoomCode}`);
    } else {
      // Multi-AI resume needs `players` so every AI seat respawns (one entry per
      // AI seat → +1 for the human). Older saves fall back to the 2-player default.
      const seatCount = meta.aiSeats?.length;
      const playersParam = seatCount && seatCount > 1 ? `&players=${seatCount + 1}` : "";
      navigate(`/game/${meta.id}?mode=${meta.mode}&difficulty=${meta.difficulty}${playersParam}`);
    }
  };

  const resumeMatch = () => {
    if (!match) return;
    const current = loadActiveGame();
    if (!current || current.id !== match.id) {
      setMatch(null);
      setMatchSummary(null);
      return;
    }
    if (current.nativeSession?.terminalOnly) {
      setMatch(null);
      setMatchSummary(null);
      return;
    }
    if (current.mode === "online") {
      const wsSession = loadWsSession();
      if (!wsSession || wsSession.terminalOnly) {
        setMatch(null);
        setMatchSummary(null);
        return;
      }
    }
    openMatch(match);
  };

  const viewRetainedResult = () => {
    if (!retainedResult) return;
    // The pointer is the durable record of the outstanding result. A replaced
    // or acknowledged pointer has no retained result left to show.
    const current = loadActiveGame();
    const stillRetained = current?.id === retainedResult.id
      && (current.mode === "online"
        ? loadWsSession()?.terminalOnly === true
        : current.nativeSession?.terminalOnly === true);
    if (!stillRetained) {
      setRetainedResult(null);
      return;
    }
    openMatch(retainedResult);
  };

  return {
    match,
    matchSummary,
    retainedResult,
    quickDraft,
    pod,
    resumeMatch,
    viewRetainedResult,
  };
}
