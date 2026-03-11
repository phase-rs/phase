import { useState } from "react";

import type { FormatConfig, GameFormat } from "../../adapter/types";
import { FORMAT_DEFAULTS, useMultiplayerStore } from "../../stores/multiplayerStore";
import { menuButtonClass } from "../menu/buttonStyles";

export interface HostSettings {
  displayName: string;
  public: boolean;
  password: string;
  timerSeconds: number | null;
  formatConfig: FormatConfig;
  aiSeats: AiSeatConfig[];
}

export interface AiSeatConfig {
  seatIndex: number;
  difficulty: string;
  deckName: string | null;
}

interface HostSetupProps {
  onHost: (settings: HostSettings) => void;
  onBack: () => void;
  connectionMode: "server" | "p2p";
}

const TIMER_OPTIONS: { value: number | null; label: string }[] = [
  { value: null, label: "None" },
  { value: 30, label: "30s" },
  { value: 60, label: "60s" },
  { value: 120, label: "120s" },
];

const FORMAT_OPTIONS: { format: GameFormat; label: string; color: string }[] = [
  { format: "Standard", label: "Standard", color: "blue" },
  { format: "Commander", label: "Commander", color: "indigo" },
  { format: "FreeForAll", label: "Free-for-All", color: "amber" },
  { format: "TwoHeadedGiant", label: "Two-Headed Giant", color: "emerald" },
];

const DIFFICULTY_OPTIONS = ["VeryEasy", "Easy", "Medium", "Hard", "VeryHard"];

const FORMAT_BUTTON_CLASSES: Record<string, { active: string; inactive: string }> = {
  blue: {
    active: "border-blue-400 bg-blue-500/30 text-blue-100",
    inactive: "border-gray-600 bg-gray-800/60 text-gray-400 hover:border-gray-500 hover:text-gray-300",
  },
  indigo: {
    active: "border-indigo-400 bg-indigo-500/30 text-indigo-100",
    inactive: "border-gray-600 bg-gray-800/60 text-gray-400 hover:border-gray-500 hover:text-gray-300",
  },
  amber: {
    active: "border-amber-400 bg-amber-500/30 text-amber-100",
    inactive: "border-gray-600 bg-gray-800/60 text-gray-400 hover:border-gray-500 hover:text-gray-300",
  },
  emerald: {
    active: "border-emerald-400 bg-emerald-500/30 text-emerald-100",
    inactive: "border-gray-600 bg-gray-800/60 text-gray-400 hover:border-gray-500 hover:text-gray-300",
  },
};

export function HostSetup({ onHost, onBack, connectionMode }: HostSetupProps) {
  const storeDisplayName = useMultiplayerStore((s) => s.displayName);
  const setStoreDisplayName = useMultiplayerStore((s) => s.setDisplayName);
  const setFormatConfig = useMultiplayerStore((s) => s.setFormatConfig);

  const [displayName, setDisplayName] = useState(storeDisplayName);
  const [isPublic, setIsPublic] = useState(true);
  const [showPassword, setShowPassword] = useState(false);
  const [password, setPassword] = useState("");
  const [timerSeconds, setTimerSeconds] = useState<number | null>(null);
  const [selectedFormat, setSelectedFormat] = useState<GameFormat>("Standard");
  const [formatConfig, setLocalFormatConfig] = useState<FormatConfig>(FORMAT_DEFAULTS.Standard);
  const [playerCount, setPlayerCount] = useState(FORMAT_DEFAULTS.Standard.min_players);
  const [aiSeats, setAiSeats] = useState<AiSeatConfig[]>([]);

  const isP2P = connectionMode === "p2p";
  const maxPlayers = isP2P ? 2 : formatConfig.max_players;

  const handleFormatSelect = (format: GameFormat) => {
    const defaults = FORMAT_DEFAULTS[format];
    setSelectedFormat(format);
    setLocalFormatConfig(defaults);
    const newCount = isP2P ? 2 : defaults.min_players;
    setPlayerCount(newCount);
    setAiSeats([]);
  };

  const handlePlayerCountChange = (count: number) => {
    setPlayerCount(count);
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
    if (displayName !== storeDisplayName) {
      setStoreDisplayName(displayName);
    }
    const finalConfig = { ...formatConfig, max_players: playerCount };
    setFormatConfig(finalConfig);
    onHost({
      displayName,
      public: isPublic,
      password: showPassword ? password : "",
      timerSeconds,
      formatConfig: finalConfig,
      aiSeats,
    });
  };

  // Filter formats: P2P only shows 2-player formats
  const availableFormats = isP2P
    ? FORMAT_OPTIONS.filter((f) => FORMAT_DEFAULTS[f.format].max_players <= 2)
    : FORMAT_OPTIONS;

  return (
    <div className="relative z-10 flex w-full max-w-md flex-col items-center gap-6 px-4">
      <h2 className="text-2xl font-bold tracking-tight text-white">Host Game</h2>

      <div className="flex w-full flex-col gap-4">
        {/* Display name */}
        <div>
          <label className="mb-1.5 block text-xs font-medium uppercase tracking-wider text-gray-400">
            Display Name
          </label>
          <input
            type="text"
            value={displayName}
            onChange={(e) => setDisplayName(e.target.value)}
            placeholder="Enter your name"
            maxLength={20}
            className="w-full rounded-lg bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 outline-none ring-1 ring-gray-700 focus:ring-cyan-500"
          />
        </div>

        {/* Format selection */}
        <div>
          <label className="mb-1.5 block text-xs font-medium uppercase tracking-wider text-gray-400">
            Format
          </label>
          <div className="grid grid-cols-2 gap-2">
            {availableFormats.map((opt) => {
              const classes = FORMAT_BUTTON_CLASSES[opt.color];
              const isSelected = selectedFormat === opt.format;
              return (
                <button
                  key={opt.format}
                  onClick={() => handleFormatSelect(opt.format)}
                  className={`rounded-lg border px-3 py-2.5 text-sm font-medium transition-colors ${
                    isSelected ? classes.active : classes.inactive
                  }`}
                >
                  {opt.label}
                </button>
              );
            })}
          </div>
        </div>

        {/* Format-specific settings */}
        <div className="rounded-lg border border-gray-700 bg-gray-800/40 p-3">
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
                className="w-16 rounded bg-gray-700 px-2 py-1 text-center text-sm text-white outline-none ring-1 ring-gray-600 focus:ring-cyan-500"
              />
            </div>

            {/* Player count (only show if format supports variable player counts) */}
            {!isP2P && formatConfig.min_players !== formatConfig.max_players && (
              <div className="flex items-center justify-between">
                <span className="text-xs text-gray-400">Players</span>
                <div className="flex rounded bg-gray-700 p-0.5 ring-1 ring-gray-600">
                  {Array.from(
                    { length: maxPlayers - formatConfig.min_players + 1 },
                    (_, i) => formatConfig.min_players + i,
                  ).map((count) => (
                    <button
                      key={count}
                      onClick={() => handlePlayerCountChange(count)}
                      className={`rounded px-3 py-1 text-xs font-medium transition-colors ${
                        playerCount === count
                          ? "bg-cyan-600 text-white"
                          : "text-gray-400 hover:text-gray-200"
                      }`}
                    >
                      {count}
                    </button>
                  ))}
                </div>
              </div>
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
                  className="w-16 rounded bg-gray-700 px-2 py-1 text-center text-sm text-white outline-none ring-1 ring-gray-600 focus:ring-cyan-500"
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
              <div className="flex items-center gap-2 rounded-lg border border-gray-700 bg-gray-800/40 px-3 py-2">
                <span className="text-xs font-medium text-emerald-400">Seat 1</span>
                <span className="flex-1 text-xs text-gray-300">You (Host)</span>
              </div>
              {/* Seats 1..playerCount-1 */}
              {Array.from({ length: playerCount - 1 }, (_, i) => i + 1).map((seatIndex) => {
                const aiSeat = aiSeats.find((s) => s.seatIndex === seatIndex);
                return (
                  <div
                    key={seatIndex}
                    className="flex items-center gap-2 rounded-lg border border-gray-700 bg-gray-800/40 px-3 py-2"
                  >
                    <span className="text-xs font-medium text-gray-400">Seat {seatIndex + 1}</span>
                    <button
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

        {/* List in lobby */}
        <label className="flex items-center gap-2">
          <input
            type="checkbox"
            checked={isPublic}
            onChange={(e) => setIsPublic(e.target.checked)}
            className="accent-emerald-500"
          />
          <span className="text-sm text-gray-300">List in lobby</span>
        </label>

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
              className="accent-emerald-500"
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
              className="mt-2 w-full rounded-lg bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 outline-none ring-1 ring-gray-700 focus:ring-cyan-500"
            />
          )}
        </div>

        {/* Timer select */}
        <div>
          <label className="mb-1.5 block text-xs font-medium uppercase tracking-wider text-gray-400">
            Turn Timer
          </label>
          <div className="flex rounded bg-gray-800 p-0.5 ring-1 ring-gray-700">
            {TIMER_OPTIONS.map((opt) => (
              <button
                key={opt.label}
                onClick={() => setTimerSeconds(opt.value)}
                className={`flex-1 rounded px-3 py-1 text-xs font-medium capitalize transition-colors ${
                  timerSeconds === opt.value
                    ? "bg-emerald-600 text-white"
                    : "text-gray-400 hover:text-gray-200"
                }`}
              >
                {opt.label}
              </button>
            ))}
          </div>
        </div>
      </div>

      {/* Action buttons */}
      <div className="flex gap-3">
        <button
          onClick={onBack}
          className={menuButtonClass({ tone: "neutral", size: "sm" })}
        >
          Back
        </button>
        <button
          onClick={handleHost}
          className={menuButtonClass({ tone: "emerald", size: "md" })}
        >
          Host Game
        </button>
      </div>
    </div>
  );
}
