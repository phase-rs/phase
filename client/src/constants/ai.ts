// `id` is the engine difficulty enum; display labels are translated at render
// via `t("aiDifficulty.levels.<id>")` (menu namespace) — not stored here.
//
// cEDH is intentionally absent from this list. It is a table-wide property
// toggled via `cedhMode` (preferencesStore), NOT a per-seat difficulty —
// selecting it per-seat used to cascade-lock every opponent to cEDH. The engine
// still receives the "CEDH" difficulty string for every seat when cEDH mode is
// on (mapped at game start via `effectiveAiDifficulty`), so `AIDifficulty` keeps
// "CEDH" as a valid engine-contract value even though it is not selectable here.
export const AI_DIFFICULTIES = [
  { id: "VeryEasy" },
  { id: "Easy" },
  { id: "Medium" },
  { id: "Hard" },
  { id: "VeryHard" },
] as const;

/** The user-selectable per-seat difficulty levels (excludes table-wide cEDH). */
export type SelectableAiDifficulty = (typeof AI_DIFFICULTIES)[number]["id"];

/** Engine difficulty contract: the selectable levels plus the table-wide "CEDH"
 *  value every seat receives when cEDH mode is enabled. */
export type AIDifficulty = SelectableAiDifficulty | "CEDH";

export const DEFAULT_AI_DIFFICULTY: AIDifficulty = "Medium";
