import { useEffect, useState } from "react";

import type { FormatConfig, GameFormat, MatchType } from "../../adapter/types";
import { FORMAT_DEFAULTS, useMultiplayerStore } from "../../stores/multiplayerStore";
import type { AiSeatConfig, HostingSettings } from "../../stores/multiplayerStore";
import { MenuPanel } from "../menu/MenuShell";
import { menuButtonClass } from "../menu/buttonStyles";

export type { AiSeatConfig };
export type HostSettings = HostingSettings;

interface HostSetupProps {
  onHost: (settings: HostSettings) => void;
  onBack: () => void;
  connectionMode: "server" | "p2p";
  /** When true, the host-submit button is disabled (e.g. live deck check
   * says the active deck is illegal for the chosen format, or a check is
   * still in flight). The parent surfaces the *reason* via the legality
   * chip above the form, so this only needs to gate the submit itself. */
  hostDisabled?: boolean;
  hostDisabledReason?: string;
}

const TIMER_OPTIONS: { value: number | null; label: string }[] = [
  { value: null, label: "None" },
  { value: 30, label: "30s" },
  { value: 60, label: "60s" },
  { value: 120, label: "120s" },
];

// Descriptions are mirrored from `FormatPicker.tsx` so the two pickers
// explain each format the same way. Shown as a `title` tooltip on each
// button. Two-Headed Giant is intentionally omitted — the engine doesn't
// support team-based play yet, so advertising it here would ship a dead end.
const FORMAT_OPTIONS: { format: GameFormat; label: string; description: string }[] = [
  { format: "Standard", label: "Standard", description: "Rotating card pool, 60-card minimum, 4-of limit." },
  { format: "Pioneer", label: "Pioneer", description: "Non-rotating format with sets from 2012 onward." },
  { format: "Historic", label: "Historic", description: "Arena's eternal format — every Arena-legal card." },
  { format: "Pauper", label: "Pauper", description: "Commons only — every card must have been printed at common." },
  { format: "Commander", label: "Commander", description: "100-card singleton with a legendary commander, 2–4 players." },
  { format: "Brawl", label: "Brawl", description: "60-card Standard singleton with a legendary commander." },
  { format: "HistoricBrawl", label: "Historic Brawl", description: "60-card eternal singleton with a legendary commander." },
  { format: "FreeForAll", label: "Free-for-All", description: "3–6 player battle royale — last player standing wins." },
];

const DIFFICULTY_OPTIONS = ["VeryEasy", "Easy", "Medium", "Hard", "VeryHard"];

/** P2P's WebRTC mesh supports 2-4 peers (see `p2p-adapter.ts:165`). The
 * HostSetup UI clamps format player counts to this ceiling so multi-seat
 * formats like Commander can still be hosted while 6-player FreeForAll
 * can't advertise an unreachable configuration. */
const P2P_MAX_PEERS = 4;

export function HostSetup({
  onHost,
  onBack,
  connectionMode,
  hostDisabled = false,
  hostDisabledReason,
}: HostSetupProps) {
  // Player name is edited in `PlayerIdentityBanner` above this form (see
  // MultiplayerPage). We read it here only to submit it and to seed the
  // room-name placeholder — this form itself intentionally has no
  // player-name field to avoid the two-inputs-for-one-value confusion.
  const displayName = useMultiplayerStore((s) => s.displayName);
  const setFormatConfig = useMultiplayerStore((s) => s.setFormatConfig);

  // Seed the format picker from whatever the user last selected (persisted
  // in the store). This means navigating away and back to host-setup keeps
  // the chosen format, and downstream views (the deck picker reached via
  // "Change Deck") can read the format from the store to filter decks.
  const storeFormatConfig = useMultiplayerStore((s) => s.formatConfig);
  const initialFormatConfig = storeFormatConfig ?? FORMAT_DEFAULTS.Standard;

  const [roomName, setRoomName] = useState("");
  const [isPublic, setIsPublic] = useState(true);
  const [showPassword, setShowPassword] = useState(false);
  const [password, setPassword] = useState("");
  const [timerSeconds, setTimerSeconds] = useState<number | null>(null);
  const [selectedFormat, setSelectedFormat] = useState<GameFormat>(
    initialFormatConfig.format,
  );
  const [formatConfig, setLocalFormatConfig] =
    useState<FormatConfig>(initialFormatConfig);
  const [playerCount, setPlayerCount] = useState(initialFormatConfig.min_players);
  const [matchType, setMatchType] = useState<MatchType>("Bo1");
  const [aiSeats, setAiSeats] = useState<AiSeatConfig[]>([]);

  // Mirror the in-flight format to the store on every change so sibling
  // views (the deck picker shown when the user clicks "Change Deck" out
  // of this form) can filter by the format the user is actively
  // configuring — not just the one they submitted last time. Mirror the
  // format-level invariants only; `max_players` is the format's ceiling
  // here (not the user's chosen count), so overwriting it with the local
  // `playerCount` would collapse the picker on re-entry. The submission
  // payload injects `playerCount` via `finalConfig` below.
  useEffect(() => {
    setFormatConfig(formatConfig);
  }, [formatConfig, setFormatConfig]);

  const isP2P = connectionMode === "p2p";
  const maxPlayers = isP2P
    ? Math.min(formatConfig.max_players, P2P_MAX_PEERS)
    : formatConfig.max_players;
  const accentTone = isP2P ? "cyan" : "emerald";

  const handleFormatSelect = (format: GameFormat) => {
    const defaults = FORMAT_DEFAULTS[format];
    setSelectedFormat(format);
    setLocalFormatConfig(defaults);
    // Let multi-seat formats start at their own minimum (e.g. Commander's
    // min is 2, so it still defaults to a duel but users can bump up to 4).
    const newCount = defaults.min_players;
    setPlayerCount(newCount);
    if (newCount !== 2) {
      setMatchType("Bo1");
    }
    setAiSeats([]);
  };

  const handlePlayerCountChange = (count: number) => {
    setPlayerCount(count);
    if (count !== 2) {
      setMatchType("Bo1");
    }
    // Remove AI seats that exceed the new count (seat 0 is always the host)
    setAiSeats((prev) => prev.filter((s) => s.seatIndex < count));
  };

  const toggleAiSeat = (seatIndex: number) => {
    setAiSeats((prev) => {
      const existing = prev.find((s) => s.seatIndex === seatIndex);
      if (existing) {
        return prev.filter((s) => s.seatIndex !== seatIndex);
      }
      return [...prev, { seatIndex, difficulty: "Medium", deckName: null }];
    });
  };

  const setAiDifficulty = (seatIndex: number, difficulty: string) => {
    setAiSeats((prev) =>
      prev.map((s) => (s.seatIndex === seatIndex ? { ...s, difficulty } : s)),
    );
  };

  const handleHost = () => {
    // `finalConfig` is the submission payload — `max_players` here is the
    // user's chosen count, not the format ceiling. Do NOT mirror this
    // into the store: the store tracks the format's invariants (so the
    // deck picker can filter), and overwriting `max_players` there would
    // collapse the picker on re-entry. The live mirror effect above
    // keeps the store in sync with the format itself.
    const finalConfig = { ...formatConfig, max_players: playerCount };
    const trimmedRoomName = roomName.trim();
    // Default to the placeholder value when the field is blank so the
    // lobby title matches what the user was shown. Falls back to null
    // (server uses host name) only if the user has no display name set.
    const resolvedRoomName =
      trimmedRoomName.length > 0
        ? trimmedRoomName
        : displayName
          ? `${displayName}'s table`
          : null;
    onHost({
      displayName,
      public: isPublic,
      password: showPassword ? password : "",
      timerSeconds,
      formatConfig: finalConfig,
      matchType: playerCount === 2 ? matchType : "Bo1",
      aiSeats,
      roomName: resolvedRoomName,
    });
  };

  // Filter formats: P2P supports 2-4 peers via WebRTC mesh, so any format
  // whose minimum is reachable from that ceiling is listable. Multi-seat
  // formats that need more than 4 players (e.g. 6-player FreeForAll) are
  // hidden here to avoid advertising a configuration we can't actually host.
  const availableFormats = isP2P
    ? FORMAT_OPTIONS.filter(
        (f) => FORMAT_DEFAULTS[f.format].min_players <= P2P_MAX_PEERS,
      )
    : FORMAT_OPTIONS;

  return (
    <form
      onSubmit={(e) => { e.preventDefault(); handleHost(); }}
      className="relative z-10 flex w-full max-w-xl flex-col items-center gap-6"
    >
      <MenuPanel className="flex w-full flex-col gap-6">
        <div className="space-y-2">
          <h2 className="menu-display text-[1.9rem] leading-tight text-white">
            {isP2P ? "Host Direct Match" : "Host Match"}
          </h2>
          {isP2P && (
            <p className="text-sm leading-6 text-slate-400">
              Dedicated server is unavailable, so this room will use a direct connection.
            </p>
          )}
        </div>

        <div className="flex w-full flex-col gap-4">
        {/* Room name — per-match label. Distinct from the player's name
            (edited in the `PlayerIdentityBanner` above this form). Blank
            falls back to the player's name on the server side. */}
        <div>
          <label className="mb-1.5 block text-xs font-medium uppercase tracking-wider text-gray-400">
            Room Name <span className="text-slate-600">(optional)</span>
          </label>
          <input
            type="text"
            value={roomName}
            onChange={(e) => setRoomName(e.target.value)}
            placeholder={
              displayName ? `${displayName}'s table` : "e.g. Friday Night Commander"
            }
            maxLength={40}
            className="w-full rounded-[16px] bg-black/18 px-3 py-2 text-sm text-white placeholder-gray-500 outline-none ring-1 ring-white/10 focus:ring-white/20"
          />
          <p className="mt-1 text-[11px] text-slate-500">
            Shown to players browsing the lobby.
            {displayName ? ` Defaults to "${displayName}'s table".` : ""}
          </p>
        </div>

        {/* Format selection */}
        <div>
          <label className="mb-1.5 block text-xs font-medium uppercase tracking-wider text-gray-400">
            Format
          </label>
          <div className="grid grid-cols-2 gap-2">
            {availableFormats.map((opt) => {
              const isSelected = selectedFormat === opt.format;
              return (
                <button
                  type="button"
                  key={opt.format}
                  onClick={() => handleFormatSelect(opt.format)}
                  title={opt.description}
                  aria-label={`${opt.label} — ${opt.description}`}
                  className={`rounded-[16px] border px-3 py-2.5 text-sm font-medium transition-colors ${
                    isSelected
                      ? "border-white/18 bg-white/10 text-white"
                      : "border-white/10 bg-black/18 text-slate-400 hover:border-white/18 hover:text-white"
                  }`}
                >
                  {opt.label}
                </button>
              );
            })}
          </div>
        </div>

        {/* Format-specific settings */}
        <div className="rounded-[18px] border border-white/10 bg-black/18 p-3">
          <div className="flex flex-col gap-3">
            {/* Starting life */}
            <div className="flex items-center justify-between">
              <span className="text-xs text-gray-400">Starting Life</span>
              <input
                type="number"
                value={formatConfig.starting_life}
                onChange={(e) =>
                  setLocalFormatConfig((prev) => ({
                    ...prev,
                    starting_life: Math.max(1, parseInt(e.target.value) || 1),
                  }))
                }
                min={1}
                className="w-16 rounded-xl bg-black/18 px-2 py-1 text-center text-sm text-white outline-none ring-1 ring-white/10 focus:ring-white/20"
              />
            </div>

            {/* Player count — hidden for fixed-seat formats like Standard
                (min==max==2). Shown in both server and P2P modes when the
                format supports a range; `maxPlayers` already clamps to the
                P2P mesh ceiling so the P2P picker never offers a seat the
                transport can't host. */}
            {formatConfig.min_players !== maxPlayers && (
              <div className="flex items-center justify-between">
                <span className="text-xs text-gray-400">Players</span>
                <div className="flex rounded-[14px] bg-black/18 p-0.5 ring-1 ring-white/10">
                  {Array.from(
                    { length: maxPlayers - formatConfig.min_players + 1 },
                    (_, i) => formatConfig.min_players + i,
                  ).map((count) => (
                    <button
                      type="button"
                      key={count}
                      onClick={() => handlePlayerCountChange(count)}
                      className={`rounded px-3 py-1 text-xs font-medium transition-colors ${
                        playerCount === count
                          ? "bg-white/10 text-white"
                          : "text-gray-400 hover:text-gray-200"
                      }`}
                    >
                      {count}
                    </button>
                  ))}
                </div>
              </div>
            )}

            <div className="flex items-center justify-between">
              <span className="text-xs text-gray-400">Match Type</span>
              <div className="flex rounded-[14px] bg-black/18 p-0.5 ring-1 ring-white/10">
                <button
                  type="button"
                  onClick={() => setMatchType("Bo1")}
                  className={`rounded px-3 py-1 text-xs font-medium transition-colors ${
                    matchType === "Bo1"
                      ? "bg-white/10 text-white"
                      : "text-gray-400 hover:text-gray-200"
                  }`}
                >
                  BO1
                </button>
                <button
                  type="button"
                  onClick={() => setMatchType("Bo3")}
                  disabled={playerCount !== 2}
                  className={`rounded px-3 py-1 text-xs font-medium transition-colors ${
                    matchType === "Bo3"
                      ? "bg-white/10 text-white"
                      : "text-gray-400 hover:text-gray-200"
                  } ${playerCount !== 2 ? "cursor-not-allowed opacity-40" : ""}`}
                >
                  BO3
                </button>
              </div>
            </div>
            {playerCount !== 2 && (
              <p className="text-xs text-gray-500">BO3 is available only for 2-player matches.</p>
            )}

            {/* Commander damage threshold (Commander only) */}
            {formatConfig.command_zone && (
              <div className="flex items-center justify-between">
                <span className="text-xs text-gray-400">Commander Damage</span>
                <input
                  type="number"
                  value={formatConfig.commander_damage_threshold ?? 21}
                  onChange={(e) =>
                    setLocalFormatConfig((prev) => ({
                      ...prev,
                      commander_damage_threshold: Math.max(1, parseInt(e.target.value) || 21),
                    }))
                  }
                  min={1}
                  className="w-16 rounded-xl bg-black/18 px-2 py-1 text-center text-sm text-white outline-none ring-1 ring-white/10 focus:ring-white/20"
                />
              </div>
            )}
          </div>
        </div>

        {/* AI seat configuration */}
        {playerCount > 1 && (
          <div>
            <label className="mb-1.5 block text-xs font-medium uppercase tracking-wider text-gray-400">
              Player Seats
            </label>
            <div className="flex flex-col gap-1.5">
              {/* Seat 0 is always the host */}
              <div className="flex items-center gap-2 rounded-[16px] border border-white/10 bg-black/18 px-3 py-2">
                <span className="text-xs font-medium text-emerald-400">Seat 1</span>
                <span className="flex-1 text-xs text-gray-300">You (Host)</span>
              </div>
              {/* Seats 1..playerCount-1 */}
              {Array.from({ length: playerCount - 1 }, (_, i) => i + 1).map((seatIndex) => {
                const aiSeat = aiSeats.find((s) => s.seatIndex === seatIndex);
                return (
                  <div
                    key={seatIndex}
                    className="flex items-center gap-2 rounded-[16px] border border-white/10 bg-black/18 px-3 py-2"
                  >
                    <span className="text-xs font-medium text-gray-400">Seat {seatIndex + 1}</span>
                    <button
                      type="button"
                      onClick={() => toggleAiSeat(seatIndex)}
                      className={`rounded px-2 py-0.5 text-xs font-medium transition-colors ${
                        aiSeat
                          ? "bg-amber-500/20 text-amber-300"
                          : "bg-cyan-500/20 text-cyan-300"
                      }`}
                    >
                      {aiSeat ? "AI" : "Human"}
                    </button>
                    {aiSeat && (
                      <select
                        value={aiSeat.difficulty}
                        onChange={(e) => setAiDifficulty(seatIndex, e.target.value)}
                        className="rounded bg-gray-700 px-1.5 py-0.5 text-xs text-white outline-none"
                      >
                        {DIFFICULTY_OPTIONS.map((d) => (
                          <option key={d} value={d}>
                            {d}
                          </option>
                        ))}
                      </select>
                    )}
                    {!aiSeat && (
                      <span className="flex-1 text-right text-xs text-gray-500">
                        Waiting for player
                      </span>
                    )}
                  </div>
                );
              })}
            </div>
          </div>
        )}

        {/* List in lobby (server mode only) */}
        {!isP2P && (
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={isPublic}
              onChange={(e) => setIsPublic(e.target.checked)}
              className="accent-emerald-500"
            />
            <span className="text-sm text-gray-300">List in lobby</span>
          </label>
        )}

        {/* Password toggle and input */}
        <div>
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={showPassword}
              onChange={(e) => {
                setShowPassword(e.target.checked);
                if (!e.target.checked) setPassword("");
              }}
              className={isP2P ? "accent-cyan-500" : "accent-emerald-500"}
            />
            <span className="text-sm text-gray-300">Set password</span>
          </label>
          {showPassword && (
            <input
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              placeholder="Game password"
              maxLength={32}
              className="mt-2 w-full rounded-[16px] bg-black/18 px-3 py-2 text-sm text-white placeholder-gray-500 outline-none ring-1 ring-white/10 focus:ring-white/20"
            />
          )}
        </div>

        {/* Timer select */}
        <div>
          <label className="mb-1.5 block text-xs font-medium uppercase tracking-wider text-gray-400">
            Turn Timer
          </label>
          <div className="flex rounded-[14px] bg-black/18 p-0.5 ring-1 ring-white/10">
            {TIMER_OPTIONS.map((opt) => (
              <button
                type="button"
                key={opt.label}
                onClick={() => setTimerSeconds(opt.value)}
                className={`flex-1 rounded px-3 py-1 text-xs font-medium capitalize transition-colors ${
                  timerSeconds === opt.value
                    ? "bg-white/10 text-white"
                    : "text-gray-400 hover:text-gray-200"
                }`}
              >
                {opt.label}
              </button>
            ))}
          </div>
        </div>
      </div>

        <div className="flex justify-end gap-3">
          <button
            type="button"
            onClick={onBack}
            className={menuButtonClass({ tone: "neutral", size: "sm" })}
          >
            Back
          </button>
          <button
            type="submit"
            disabled={hostDisabled}
            title={hostDisabled ? hostDisabledReason : undefined}
            aria-disabled={hostDisabled || undefined}
            className={`${menuButtonClass({ tone: accentTone, size: "md" })} disabled:cursor-not-allowed disabled:opacity-50`}
          >
            {isP2P ? "Host P2P Game" : "Host Game"}
          </button>
        </div>
      </MenuPanel>
    </form>
  );
}
