import { useState } from "react";

import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { menuButtonClass } from "../menu/buttonStyles";

export interface HostSettings {
  displayName: string;
  public: boolean;
  password: string;
  timerSeconds: number | null;
}

interface HostSetupProps {
  onHost: (settings: HostSettings) => void;
  onBack: () => void;
}

const TIMER_OPTIONS: { value: number | null; label: string }[] = [
  { value: null, label: "None" },
  { value: 30, label: "30s" },
  { value: 60, label: "60s" },
  { value: 120, label: "120s" },
];

export function HostSetup({ onHost, onBack }: HostSetupProps) {
  const storeDisplayName = useMultiplayerStore((s) => s.displayName);
  const setStoreDisplayName = useMultiplayerStore((s) => s.setDisplayName);

  const [displayName, setDisplayName] = useState(storeDisplayName);
  const [isPublic, setIsPublic] = useState(true);
  const [showPassword, setShowPassword] = useState(false);
  const [password, setPassword] = useState("");
  const [timerSeconds, setTimerSeconds] = useState<number | null>(null);

  const handleHost = () => {
    // Save display name back to store
    if (displayName !== storeDisplayName) {
      setStoreDisplayName(displayName);
    }
    onHost({
      displayName,
      public: isPublic,
      password: showPassword ? password : "",
      timerSeconds,
    });
  };

  return (
    <div className="relative z-10 flex w-full max-w-sm flex-col items-center gap-6 px-4">
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
