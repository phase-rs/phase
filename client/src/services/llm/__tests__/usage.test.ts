import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { llmUsageTotals, recordLlmUsage, resetLlmUsage } from "../usage";

beforeEach(() => {
  resetLlmUsage();
  vi.spyOn(console, "info").mockImplementation(() => {});
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("recordLlmUsage", () => {
  it("totals and prints the tokens written to the prompt cache", () => {
    recordLlmUsage("game", "seat 0", 4_000, {
      inputTokens: 12,
      outputTokens: 30,
      cachedInputTokens: 2_000,
      cacheCreationInputTokens: 900,
    });
    recordLlmUsage("game", "seat 0", 600, {
      inputTokens: 8,
      outputTokens: 25,
      cacheCreationInputTokens: 300,
    });

    expect(llmUsageTotals("game")).toEqual({
      calls: 2,
      promptChars: 4_600,
      inputTokens: 20,
      outputTokens: 55,
      cachedInputTokens: 2_000,
      cacheCreationInputTokens: 1_200,
    });
    expect(console.info).toHaveBeenCalledWith(
      expect.stringContaining("12 in / 30 out tokens (2000 cached) (900 written to cache)"),
    );
  });

  it("leaves a provider that reports no cache writes unchanged", () => {
    recordLlmUsage("game", "seat 1", 1_000, { inputTokens: 500, outputTokens: 20 });

    expect(llmUsageTotals("game").cacheCreationInputTokens).toBe(0);
    expect(console.info).toHaveBeenCalledWith(expect.stringContaining("500 in / 20 out tokens;"));
  });
});
