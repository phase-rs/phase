export const AI_DIFFICULTIES = [
  { id: "VeryEasy", label: "Very Easy", shortLabel: "Very Easy" },
  { id: "Easy", label: "Easy", shortLabel: "Easy" },
  { id: "Medium", label: "Medium", shortLabel: "Medium" },
  { id: "Hard", label: "Hard", shortLabel: "Hard" },
  { id: "VeryHard", label: "Very Hard", shortLabel: "Very Hard" },
] as const;

export type AIDifficulty = (typeof AI_DIFFICULTIES)[number]["id"];

export const DEFAULT_AI_DIFFICULTY: AIDifficulty = "Medium";

export function getAiDifficultyLabel(difficulty: string): string {
  return AI_DIFFICULTIES.find((item) => item.id === difficulty)?.label ?? difficulty;
}
