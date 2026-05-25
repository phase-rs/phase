// `id` is the engine difficulty enum; display labels are translated at render
// via `t("aiDifficulty.levels.<id>")` (menu namespace) — not stored here.
export const AI_DIFFICULTIES = [
  { id: "VeryEasy" },
  { id: "Easy" },
  { id: "Medium" },
  { id: "Hard" },
  { id: "VeryHard" },
] as const;

export type AIDifficulty = (typeof AI_DIFFICULTIES)[number]["id"];

export const DEFAULT_AI_DIFFICULTY: AIDifficulty = "Medium";
