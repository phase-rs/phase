import type { FormatConfig, GameFormat } from "../adapter/types";

const STORAGE_KEY = "phase-game-presets";

export interface GamePreset {
  id: string;
  name: string;
  format: GameFormat;
  formatConfig: Partial<FormatConfig>;
  deckId: string | null;
  aiDifficulty: string | null;
  playerCount: number;
}

const DEFAULT_PRESETS: GamePreset[] = [
  {
    id: "default-standard-ai",
    name: "Quick Standard (vs Medium AI)",
    format: "Standard",
    formatConfig: {},
    deckId: null,
    aiDifficulty: "Medium",
    playerCount: 2,
  },
  {
    id: "default-commander-ffa",
    name: "Quick Commander (4-player FFA)",
    format: "Commander",
    formatConfig: {},
    deckId: null,
    aiDifficulty: "Medium",
    playerCount: 4,
  },
];

export function loadPresets(): GamePreset[] {
  const raw = localStorage.getItem(STORAGE_KEY);
  if (!raw) return DEFAULT_PRESETS;
  const saved = JSON.parse(raw) as GamePreset[];
  return saved.length > 0 ? saved : DEFAULT_PRESETS;
}

export function savePreset(preset: GamePreset): void {
  const presets = loadPresets().filter((p) => p.id !== preset.id);
  // Remove default presets when user saves custom ones
  const filtered = presets.filter((p) => !p.id.startsWith("default-"));
  filtered.push(preset);
  localStorage.setItem(STORAGE_KEY, JSON.stringify(filtered));
}

export function deletePreset(id: string): void {
  const presets = loadPresets().filter((p) => p.id !== id);
  localStorage.setItem(STORAGE_KEY, JSON.stringify(presets));
}
