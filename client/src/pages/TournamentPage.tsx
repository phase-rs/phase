import { useCallback, useEffect, useState } from "react";
import { useLocation, useNavigate, useParams } from "react-router";

import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { MenuShell } from "../components/menu/MenuShell";
import { PairingsList } from "../components/tournament/PairingsList";
import { StandingsTable } from "../components/tournament/StandingsTable";
import type { TournamentView } from "../adapter/types";
import { openTournamentClient } from "../services/tournamentClient";
import { useMultiplayerStore } from "../stores/multiplayerStore";

interface LocationState {
  playerKey?: string;
  organizer?: boolean;
}

export function TournamentPage() {
  const { code } = useParams<{ code: string }>();
  const navigate = useNavigate();
  const location = useLocation();
  const state = (location.state ?? {}) as LocationState;
  const brokerUrl = useMultiplayerStore((s) => s.serverAddress);

  const [tournament, setTournament] = useState<TournamentView | null>(null);
  const [playerKey, setPlayerKey] = useState<string | null>(state.playerKey ?? null);
  const [isOrganizer, setIsOrganizer] = useState(Boolean(state.organizer));
  const [displayName, setDisplayName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [joining, setJoining] = useState(false);

  const connect = useCallback(async () => {
    if (!brokerUrl) {
      throw new Error("No lobby broker configured.");
    }
    return openTournamentClient(brokerUrl);
  }, [brokerUrl]);

  useEffect(() => {
    if (!code) return;
    let cleanup: (() => void) | undefined;
    let cancelled = false;
    void (async () => {
      try {
        const client = await connect();
        cleanup = client.subscribeTournaments(
          () => {},
          (view) => {
            if (view.tournamentCode === code && !cancelled) {
              setTournament(view);
            }
          },
          (completed) => {
            if (completed === code && !cancelled) {
              setTournament((prev) =>
                prev ? { ...prev, status: "completed" } : prev,
              );
            }
          },
        );
      } catch (err) {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : String(err));
        }
      }
    })();
    return () => {
      cancelled = true;
      cleanup?.();
    };
  }, [code, connect]);

  const handleJoin = async () => {
    if (!code) return;
    setJoining(true);
    setError(null);
    try {
      const client = await connect();
      const view = await client.joinTournament(code, displayName.trim());
      setTournament(view);
      const self = view.standings.find(
        (s) => s.displayName.toLowerCase() === displayName.trim().toLowerCase(),
      );
      if (self) setPlayerKey(self.playerKey);
      client.close();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setJoining(false);
    }
  };

  const handleStartRound = async () => {
    if (!code) return;
    try {
      const client = await connect();
      await client.startRound(code);
      client.close();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleReport = async (
    matchId: string,
    winnerKey: string | null,
    aWins: number,
    bWins: number,
  ) => {
    if (!code) return;
    try {
      const client = await connect();
      await client.reportMatchResult(code, matchId, winnerKey, aWins, bWins);
      client.close();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleDrop = async () => {
    if (!code) return;
    try {
      const client = await connect();
      await client.dropFromTournament(code);
      client.close();
      navigate("/tournament");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleEnd = async () => {
    if (!code) return;
    try {
      const client = await connect();
      await client.endTournament(code);
      client.close();
      navigate("/tournament");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const needsJoin = !playerKey && tournament?.status === "registration";

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <ScreenChrome onBack={() => navigate("/tournament")} />
      <div className="menu-scene__vignette" />
      <div className="menu-scene__haze" />

      <MenuShell
        eyebrow={code ?? ""}
        title={tournament?.name ?? "Tournament"}
        subtitle={
          tournament
            ? `Round ${tournament.currentRound} of ${tournament.totalRounds} · ${tournament.playerCount} players`
            : "Loading…"
        }
      >
        {needsJoin && (
          <section className="mb-6 rounded-2xl border border-amber-400/20 bg-amber-950/20 p-4">
            <h2 className="font-medium text-white">Join this event</h2>
            <div className="mt-3 flex flex-wrap gap-2">
              <input
                value={displayName}
                onChange={(e) => setDisplayName(e.target.value)}
                placeholder="Display name"
                className="flex-1 rounded-lg border border-white/10 bg-black/40 px-3 py-2 text-white"
              />
              <button
                type="button"
                disabled={joining || !displayName.trim()}
                onClick={() => void handleJoin()}
                className="rounded-lg bg-amber-500/90 px-4 py-2 font-medium text-black disabled:opacity-40"
              >
                Join
              </button>
            </div>
          </section>
        )}

        {isOrganizer && tournament && tournament.status !== "completed" && (
          <div className="mb-6 flex flex-wrap gap-2">
            <button
              type="button"
              onClick={() => void handleStartRound()}
              className="rounded-lg bg-emerald-600/90 px-4 py-2 text-sm font-medium text-white"
            >
              {tournament.status === "registration"
                ? "Start tournament"
                : "Start next round"}
            </button>
            <button
              type="button"
              onClick={() => void handleEnd()}
              className="rounded-lg border border-white/20 px-4 py-2 text-sm text-white/80"
            >
              End tournament
            </button>
          </div>
        )}

        {playerKey && tournament?.status !== "completed" && (
          <button
            type="button"
            onClick={() => void handleDrop()}
            className="mb-6 text-sm text-red-300 underline"
          >
            Drop from tournament
          </button>
        )}

        <div className="grid gap-8 lg:grid-cols-2">
          <section>
            <h2 className="mb-3 text-lg font-semibold text-white">Standings</h2>
            <StandingsTable
              standings={tournament?.standings ?? []}
              highlightPlayerKey={playerKey}
            />
          </section>
          <section>
            <h2 className="mb-3 text-lg font-semibold text-white">Current pairings</h2>
            <PairingsList
              pairings={tournament?.pairings ?? []}
              playerKey={playerKey}
              isOrganizer={isOrganizer}
              onReport={handleReport}
            />
            <p className="mt-4 text-xs text-white/40">
              Play your match at your own table using the normal P2P host flow, then
              report the result here.
            </p>
          </section>
        </div>

        {error && (
          <p className="mt-4 rounded-lg border border-red-400/30 bg-red-950/40 px-3 py-2 text-sm text-red-200">
            {error}
          </p>
        )}
      </MenuShell>
    </div>
  );
}
