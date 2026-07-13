import { vi } from "vitest";

import type { EngineAdapter, EngineSnapshot } from "../../adapter/types.ts";
import { nextSnapshotSeq } from "../../adapter/types.ts";
import { buildGameState, buildLegalActionsResult } from "./gameStateFactory.ts";

export const buildEngineAdapterMock = (
  state = buildGameState(),
  overrides: Partial<EngineAdapter> = {},
) => {
  // Holder for the FINAL (post-override) adapter, so the default `getSnapshot`
  // below can delegate to whatever `getState` / `getLegalActions` the caller
  // ended up with. A holder (rather than a direct self-reference) keeps TS out
  // of circular inference.
  const ref: { current?: EngineAdapter } = {};

  const adapter = {
    initialize: vi.fn().mockResolvedValue(undefined),
    initializeGame: vi.fn().mockResolvedValue({ events: [] }),
    submitAction: vi.fn().mockResolvedValue({ events: [] }),
    getState: vi.fn().mockResolvedValue(state),
    getLegalActions: vi.fn().mockResolvedValue(buildLegalActionsResult()),
    /**
     * Default `getSnapshot` composes the adapter's OWN `getState` +
     * `getLegalActions` at call time, so a test that scripts either of those
     * (via `overrides`, or by reassigning them afterwards) gets a snapshot
     * consistent with its script for free — the same pair, read together.
     *
     * The seq comes from the real global counter, so a snapshot taken later in a
     * test always outranks one taken earlier and the store's revision gate
     * behaves exactly as it does in production. A test that needs a *stale*
     * snapshot overrides `getSnapshot` and supplies its own lower seq.
     */
    getSnapshot: vi.fn(async (): Promise<EngineSnapshot> => ({
      state: await ref.current!.getState(),
      legalResult: await ref.current!.getLegalActions(),
      seq: nextSnapshotSeq(),
    })),
    restoreState: vi.fn(),
    getAiAction: vi.fn().mockReturnValue(null),
    dispose: vi.fn(),
    estimateBracket: vi.fn().mockResolvedValue(null),
  } satisfies EngineAdapter;

  const merged = Object.assign(adapter, overrides);
  ref.current = merged;
  return merged;
};
