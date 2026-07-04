import { vi } from "vitest";

import type { EngineAdapter } from "../../adapter/types.ts";
import { buildGameState, buildLegalActionsResult } from "./gameStateFactory.ts";

export const buildEngineAdapterMock = (
  state = buildGameState(),
  overrides: Partial<EngineAdapter> = {},
) => {
  const adapter = {
    initialize: vi.fn().mockResolvedValue(undefined),
    initializeGame: vi.fn().mockResolvedValue({ events: [] }),
    submitAction: vi.fn().mockResolvedValue({ events: [] }),
    getState: vi.fn().mockResolvedValue(state),
    getLegalActions: vi.fn().mockResolvedValue(buildLegalActionsResult()),
    restoreState: vi.fn(),
    getAiAction: vi.fn().mockReturnValue(null),
    dispose: vi.fn(),
    estimateBracket: vi.fn().mockResolvedValue(null),
  } satisfies EngineAdapter;

  return Object.assign(adapter, overrides);
};
