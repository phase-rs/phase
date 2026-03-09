import { LifeTotal } from "../controls/LifeTotal.tsx";
import { ManaPoolSummary } from "./ManaPoolSummary.tsx";
import { PhaseIndicatorLeft, PhaseIndicatorRight } from "../controls/PhaseStopBar.tsx";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";

interface PlayerHudProps {
  onSettingsClick?: () => void;
}

export function PlayerHud({ onSettingsClick }: PlayerHudProps = {}) {
  const masterMuted = usePreferencesStore((s) => s.masterMuted);
  const setMasterMuted = usePreferencesStore((s) => s.setMasterMuted);

  return (
    <div className="relative z-20 flex shrink-0 items-center justify-center gap-3 py-1">
      <PhaseIndicatorLeft />
      <div className="flex items-center gap-2 rounded-full bg-black/50 px-3 py-1">
        <LifeTotal playerId={0} size="lg" />
        <ManaPoolSummary playerId={0} />
        <button
          onClick={() => setMasterMuted(!masterMuted)}
          className={`rounded p-1 transition-colors hover:bg-white/10 hover:text-gray-300 ${
            masterMuted ? "text-red-400" : "text-gray-500"
          }`}
          aria-label={masterMuted ? "Unmute audio" : "Mute audio"}
        >
          {masterMuted ? (
            <svg
              xmlns="http://www.w3.org/2000/svg"
              viewBox="0 0 20 20"
              fill="currentColor"
              className="h-4 w-4"
            >
              <path d="M9.547 3.062A.75.75 0 0 1 10 3.75v12.5a.75.75 0 0 1-1.264.546L5.203 13.5H2.667a.75.75 0 0 1-.7-.48A6.985 6.985 0 0 1 1.5 10c0-.85.151-1.665.429-2.42a.75.75 0 0 1 .737-.58h2.499l3.533-3.296a.75.75 0 0 1 .849-.142ZM13.28 7.22a.75.75 0 1 0-1.06 1.06L13.94 10l-1.72 1.72a.75.75 0 0 0 1.06 1.06L15 11.06l1.72 1.72a.75.75 0 1 0 1.06-1.06L16.06 10l1.72-1.72a.75.75 0 0 0-1.06-1.06L15 8.94l-1.72-1.72Z" />
            </svg>
          ) : (
            <svg
              xmlns="http://www.w3.org/2000/svg"
              viewBox="0 0 20 20"
              fill="currentColor"
              className="h-4 w-4"
            >
              <path d="M10 3.75a.75.75 0 0 0-1.264-.546L5.203 6.5H2.667a.75.75 0 0 0-.7.48A6.985 6.985 0 0 0 1.5 10c0 .85.151 1.665.429 2.42a.75.75 0 0 0 .737.58h2.499l3.533 3.296A.75.75 0 0 0 10 15.75V3.75ZM15.95 5.05a.75.75 0 0 0-1.06 1.06 5.5 5.5 0 0 1 0 7.78.75.75 0 0 0 1.06 1.06 7 7 0 0 0 0-9.9Z" />
              <path d="M13.829 7.172a.75.75 0 0 0-1.06 1.06 2.5 2.5 0 0 1 0 3.536.75.75 0 0 0 1.06 1.06 4 4 0 0 0 0-5.656Z" />
            </svg>
          )}
        </button>
        <button
          onClick={onSettingsClick}
          className="rounded p-1 text-gray-500 transition-colors hover:bg-white/10 hover:text-gray-300"
          aria-label="Settings"
        >
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 20 20"
            fill="currentColor"
            className="h-4 w-4"
          >
            <path
              fillRule="evenodd"
              d="M7.84 1.804A1 1 0 0 1 8.82 1h2.36a1 1 0 0 1 .98.804l.331 1.652a6.993 6.993 0 0 1 1.929 1.115l1.598-.54a1 1 0 0 1 1.186.447l1.18 2.044a1 1 0 0 1-.205 1.251l-1.267 1.113a7.047 7.047 0 0 1 0 2.228l1.267 1.113a1 1 0 0 1 .206 1.25l-1.18 2.045a1 1 0 0 1-1.187.447l-1.598-.54a6.993 6.993 0 0 1-1.929 1.115l-.33 1.652a1 1 0 0 1-.98.804H8.82a1 1 0 0 1-.98-.804l-.331-1.652a6.993 6.993 0 0 1-1.929-1.115l-1.598.54a1 1 0 0 1-1.186-.447l-1.18-2.044a1 1 0 0 1 .205-1.251l1.267-1.114a7.05 7.05 0 0 1 0-2.227L1.821 7.773a1 1 0 0 1-.206-1.25l1.18-2.045a1 1 0 0 1 1.187-.447l1.598.54A6.993 6.993 0 0 1 7.51 3.456l.33-1.652ZM10 13a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z"
              clipRule="evenodd"
            />
          </svg>
        </button>
      </div>
      <PhaseIndicatorRight />
    </div>
  );
}
