import { useState } from "react";

import type { PairingView } from "../../adapter/types";

interface PairingsListProps {
  pairings: PairingView[];
  playerKey: string | null;
  isOrganizer: boolean;
  onReport: (
    matchId: string,
    winnerKey: string | null,
    playerAWins: number,
    playerBWins: number,
  ) => void;
}

export function PairingsList({
  pairings,
  playerKey,
  isOrganizer,
  onReport,
}: PairingsListProps) {
  if (pairings.length === 0) {
    return (
      <p className="text-sm text-white/50">No pairings for the current round yet.</p>
    );
  }

  return (
    <ul className="space-y-3">
      {pairings.map((pairing) => (
        <PairingRow
          key={pairing.matchId}
          pairing={pairing}
          playerKey={playerKey}
          isOrganizer={isOrganizer}
          onReport={onReport}
        />
      ))}
    </ul>
  );
}

function PairingRow({
  pairing,
  playerKey,
  isOrganizer,
  onReport,
}: {
  pairing: PairingView;
  playerKey: string | null;
  isOrganizer: boolean;
  onReport: PairingsListProps["onReport"];
}) {
  const [aWins, setAWins] = useState(2);
  const [bWins, setBWins] = useState(0);
  const isBye = !pairing.playerBKey;
  const canReport =
    !pairing.reported &&
    !isBye &&
    (isOrganizer ||
      playerKey === pairing.playerAKey ||
      playerKey === pairing.playerBKey);

  return (
    <li className="rounded-xl border border-white/10 bg-black/20 p-4">
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs uppercase tracking-wide text-white/40">
          Table {pairing.table + 1}
        </span>
        {pairing.reported && (
          <span className="text-xs text-emerald-300">Reported</span>
        )}
      </div>
      <p className="mt-2 text-white">
        {isBye ? (
          <>{pairing.playerAName} — Bye</>
        ) : (
          <>
            {pairing.playerAName}
            <span className="mx-2 text-white/40">vs</span>
            {pairing.playerBName}
          </>
        )}
      </p>
      {canReport && pairing.playerBKey && (
        <div className="mt-3 flex flex-wrap items-end gap-2">
          <label className="text-xs text-white/50">
            {pairing.playerAName} wins
            <input
              type="number"
              min={0}
              max={3}
              value={aWins}
              onChange={(e) => setAWins(Number(e.target.value))}
              className="mt-1 block w-16 rounded border border-white/10 bg-black/40 px-2 py-1 text-white"
            />
          </label>
          <label className="text-xs text-white/50">
            {pairing.playerBName} wins
            <input
              type="number"
              min={0}
              max={3}
              value={bWins}
              onChange={(e) => setBWins(Number(e.target.value))}
              className="mt-1 block w-16 rounded border border-white/10 bg-black/40 px-2 py-1 text-white"
            />
          </label>
          <button
            type="button"
            className="rounded-lg bg-white/10 px-3 py-1.5 text-sm text-white hover:bg-white/20"
            onClick={() => {
              const winnerKey =
                aWins === bWins
                  ? null
                  : aWins > bWins
                    ? pairing.playerAKey
                    : pairing.playerBKey!;
              onReport(pairing.matchId, winnerKey, aWins, bWins);
            }}
          >
            Report result
          </button>
        </div>
      )}
    </li>
  );
}
