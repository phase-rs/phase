import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  LANDSCAPE_GAME_REQUIREMENT_QUERY,
  useLandscapeGameRequirement,
} from "../useLandscapeGameRequirement.ts";

function installMediaQuery(initialMatches: boolean) {
  let matches = initialMatches;
  const listeners = new Set<() => void>();
  const mediaQuery = {
    get matches() {
      return matches;
    },
    media: LANDSCAPE_GAME_REQUIREMENT_QUERY,
    onchange: null,
    addEventListener: vi.fn((_type: string, listener: () => void) => {
      listeners.add(listener);
    }),
    removeEventListener: vi.fn((_type: string, listener: () => void) => {
      listeners.delete(listener);
    }),
    addListener: vi.fn(),
    removeListener: vi.fn(),
    dispatchEvent: vi.fn(),
  } as unknown as MediaQueryList;

  vi.spyOn(window, "matchMedia").mockReturnValue(mediaQuery);

  return {
    setMatches(next: boolean) {
      matches = next;
      for (const listener of listeners) listener();
    },
  };
}

describe("useLandscapeGameRequirement", () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("blocks a touch-first portrait viewport", () => {
    installMediaQuery(true);

    const { result } = renderHook(() => useLandscapeGameRequirement());

    expect(window.matchMedia).toHaveBeenCalledWith(
      LANDSCAPE_GAME_REQUIREMENT_QUERY,
    );
    expect(result.current).toBe(true);
  });

  it("releases the gate when the device rotates to landscape", () => {
    const query = installMediaQuery(true);
    const { result } = renderHook(() => useLandscapeGameRequirement());

    act(() => query.setMatches(false));

    expect(result.current).toBe(false);
  });
});
