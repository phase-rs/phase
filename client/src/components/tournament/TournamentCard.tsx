import type { TournamentSummary } from "../../adapter/types";

interface TournamentCardProps {
  tournament: TournamentSummary;
  onJoin: (code: string) => void;
}

function statusLabel(status: TournamentSummary["status"]): string {
  switch (status) {
    case "registration":
      return "Registration";
    case "in_progress":
      return "In Progress";
    case "completed":
      return "Completed";
    default:
      return status;
  }
}

export function TournamentCard({ tournament, onJoin }: TournamentCardProps) {
  return (
    <article className="rounded-xl border border-white/10 bg-black/30 p-4 backdrop-blur-sm">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h3 className="text-lg font-semibold text-white">{tournament.name}</h3>
          <p className="text-sm text-white/60">
            Hosted by {tournament.organizerName}
          </p>
        </div>
        <span className="rounded-full bg-white/10 px-2 py-0.5 text-xs uppercase tracking-wide text-white/80">
          {statusLabel(tournament.status)}
        </span>
      </div>
      <dl className="mt-3 grid grid-cols-3 gap-2 text-sm text-white/70">
        <div>
          <dt className="text-white/40">Players</dt>
          <dd>{tournament.playerCount}</dd>
        </div>
        <div>
          <dt className="text-white/40">Rounds</dt>
          <dd>
            {tournament.currentRound}/{tournament.totalRounds}
          </dd>
        </div>
        <div>
          <dt className="text-white/40">Code</dt>
          <dd className="font-mono">{tournament.tournamentCode}</dd>
        </div>
      </dl>
      {tournament.status === "registration" && (
        <button
          type="button"
          className="mt-4 w-full rounded-lg bg-amber-500/90 px-3 py-2 text-sm font-medium text-black transition hover:bg-amber-400"
          onClick={() => onJoin(tournament.tournamentCode)}
        >
          Join tournament
        </button>
      )}
    </article>
  );
}
