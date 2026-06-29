import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";

import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { MenuShell } from "../components/menu/MenuShell";
import { TournamentCard } from "../components/tournament/TournamentCard";
import type { TournamentSummary } from "../adapter/types";
import { openTournamentClient } from "../services/tournamentClient";
import { useMultiplayerStore } from "../stores/multiplayerStore";

export function TournamentLandingPage() {
  const navigate = useNavigate();
  const brokerUrl = useMultiplayerStore((s) => s.serverAddress);
  const [tournaments, setTournaments] = useState<TournamentSummary[]>([]);
  const [name, setName] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [rounds, setRounds] = useState(3);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const connect = useCallback(async () => {
    if (!brokerUrl) {
      setError("No lobby broker configured. Set a broker URL in multiplayer settings.");
      return null;
    }
    return openTournamentClient(brokerUrl);
  }, [brokerUrl]);

  useEffect(() => {
    let cleanup: (() => void) | undefined;
    let cancelled = false;
    void (async () => {
      try {
        const client = await connect();
        if (!client || cancelled) return;
        cleanup = client.subscribeTournaments(
          setTournaments,
          () => {},
          (code) => setTournaments((prev) => prev.filter((t) => t.tournamentCode !== code)),
        );
        const list = await client.listTournaments();
        if (!cancelled) setTournaments(list);
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
  }, [connect]);

  const handleCreate = async () => {
    setLoading(true);
    setError(null);
    try {
      const client = await connect();
      if (!client) return;
      const created = await client.createTournament({
        name: name.trim(),
        displayName: displayName.trim(),
        totalRounds: rounds,
      });
      client.close();
      navigate(`/tournament/${created.tournamentCode}`, {
        state: { playerKey: created.playerKey, organizer: true },
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  const handleJoin = (code: string) => {
    navigate(`/tournament/${code}`);
  };

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <ScreenChrome onBack={() => navigate("/multiplayer")} />
      <div className="menu-scene__vignette" />
      <div className="menu-scene__haze" />

      <MenuShell
        eyebrow="Swiss events"
        title="Tournaments"
        subtitle="Organize multi-round Swiss events for your playgroup — pairings and standings without a spreadsheet."
      >
        <section className="grid gap-6 lg:grid-cols-2">
          <div className="rounded-2xl border border-white/10 bg-black/25 p-5">
            <h2 className="text-lg font-semibold text-white">Create tournament</h2>
            <div className="mt-4 space-y-3">
              <label className="block text-sm text-white/70">
                Event name
                <input
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  className="mt-1 w-full rounded-lg border border-white/10 bg-black/40 px-3 py-2 text-white"
                  placeholder="Friday Night Swiss"
                />
              </label>
              <label className="block text-sm text-white/70">
                Your display name
                <input
                  value={displayName}
                  onChange={(e) => setDisplayName(e.target.value)}
                  className="mt-1 w-full rounded-lg border border-white/10 bg-black/40 px-3 py-2 text-white"
                  placeholder="Alice"
                />
              </label>
              <label className="block text-sm text-white/70">
                Swiss rounds
                <input
                  type="number"
                  min={1}
                  max={15}
                  value={rounds}
                  onChange={(e) => setRounds(Number(e.target.value))}
                  className="mt-1 w-24 rounded-lg border border-white/10 bg-black/40 px-3 py-2 text-white"
                />
              </label>
              <button
                type="button"
                disabled={loading || !name.trim() || !displayName.trim()}
                onClick={() => void handleCreate()}
                className="w-full rounded-lg bg-amber-500/90 py-2 font-medium text-black disabled:opacity-40"
              >
                {loading ? "Creating…" : "Create & organize"}
              </button>
            </div>
          </div>

          <div>
            <h2 className="mb-3 text-lg font-semibold text-white">Open tournaments</h2>
            {tournaments.length === 0 ? (
              <p className="text-sm text-white/50">No open tournaments right now.</p>
            ) : (
              <div className="space-y-3">
                {tournaments.map((t) => (
                  <TournamentCard key={t.tournamentCode} tournament={t} onJoin={handleJoin} />
                ))}
              </div>
            )}
          </div>
        </section>

        {error && (
          <p className="mt-4 rounded-lg border border-red-400/30 bg-red-950/40 px-3 py-2 text-sm text-red-200">
            {error}
          </p>
        )}
      </MenuShell>
    </div>
  );
}
