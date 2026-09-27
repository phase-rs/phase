import type { AIDifficulty } from "../constants/ai";
import type { CommanderBracketTier } from "../types/bracketEstimate";

// The Rust engine's `AiDifficulty::for_bracket` is the canonical source of
// truth. This synchronous mirror is for render-time labels before WASM loads;
// the integration test compares it with `getBracketDifficultyTable`.
export const BRACKET_DIFFICULTY_DEFAULT: Record<CommanderBracketTier, AIDifficulty> = {
  exhibition: "Easy",
  core: "Medium",
  upgraded: "Hard",
  optimized: "VeryHard",
  cedh: "CEDH",
};
