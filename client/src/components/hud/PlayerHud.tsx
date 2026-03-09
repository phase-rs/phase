import { LifeTotal } from "../controls/LifeTotal.tsx";
import { ManaPoolSummary } from "./ManaPoolSummary.tsx";
import { usePhaseInfo } from "../../hooks/usePhaseInfo.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";

interface PlayerHudProps {
  onSettingsClick?: () => void;
}

export function PlayerHud({ onSettingsClick }: PlayerHudProps = {}) {
  const { phaseLabel } = usePhaseInfo();
  const hudLayout = usePreferencesStore((s) => s.hudLayout);

  const content = (
    <div className="flex items-center gap-3 rounded-full bg-gray-800/60 px-4 py-1">
      <LifeTotal playerId={0} size="lg" />
      <ManaPoolSummary playerId={0} />
      <span className="rounded bg-white/10 px-2 py-0.5 text-xs font-semibold text-gray-300">
        {phaseLabel}
      </span>
      <button
        onClick={onSettingsClick}
        className="ml-auto rounded p-1 text-gray-500 transition-colors hover:bg-white/10 hover:text-gray-300"
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
  );

  if (hudLayout === "floating") {
    return (
      <div className="fixed bottom-4 left-4 z-30 rounded-lg bg-gray-900/90 px-3 py-2 shadow-lg ring-1 ring-gray-700">
        {content}
      </div>
    );
  }

  return content;
}
