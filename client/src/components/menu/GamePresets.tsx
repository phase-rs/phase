import { useEffect, useState } from "react";

import type { GamePreset } from "../../services/presets";
import { deletePreset, loadPresets } from "../../services/presets";

const FORMAT_ICONS: Record<string, string> = {
  Standard: "\u2694",
  Commander: "\uD83D\uDC51",
  FreeForAll: "\uD83D\uDD25",
  TwoHeadedGiant: "\uD83D\uDEE1",
};

interface GamePresetsProps {
  onSelectPreset: (preset: GamePreset) => void;
}

export function GamePresets({ onSelectPreset }: GamePresetsProps) {
  const [presets, setPresets] = useState<GamePreset[]>([]);

  useEffect(() => {
    setPresets(loadPresets());
  }, []);

  const handleDelete = (e: React.MouseEvent, id: string) => {
    e.stopPropagation();
    deletePreset(id);
    setPresets(loadPresets());
  };

  if (presets.length === 0) return null;

  return (
    <div className="flex flex-col items-center gap-3">
      <h3 className="text-sm font-medium text-gray-500 uppercase tracking-wider">Quick Start</h3>
      <div className="flex flex-wrap justify-center gap-3">
        {presets.map((preset) => (
          <button
            key={preset.id}
            onClick={() => onSelectPreset(preset)}
            className="group relative flex items-center gap-2 rounded-lg border border-gray-700 bg-gray-800/40 px-4 py-2.5 text-sm text-gray-300 transition-colors hover:border-gray-500 hover:bg-gray-800/60 hover:text-white"
          >
            <span>{FORMAT_ICONS[preset.format] ?? ""}</span>
            <span>{preset.name}</span>
            {!preset.id.startsWith("default-") && (
              <span
                onClick={(e) => handleDelete(e, preset.id)}
                className="ml-1 text-gray-600 transition-colors hover:text-red-400"
                role="button"
                tabIndex={-1}
              >
                x
              </span>
            )}
          </button>
        ))}
      </div>
    </div>
  );
}
