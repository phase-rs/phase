import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useMediaQuery } from "../useMediaQuery.ts";

const LANDSCAPE = "(orientation: landscape)";
const PORTRAIT = "(orientation: portrait)";

/**
 * A stand-in for the browser's media queries. Each query's answer and its
 * `change` listeners are held here, so a test can turn the screen, or change
 * an answer without telling anyone, and count who is still listening.
 */
function createFakeScreen() {
  const answers = new Map<string, boolean>();
  const listeners = new Map<string, Set<() => void>>();
  const listenersFor = (query: string) => {
    const existing = listeners.get(query);
    if (existing) return existing;
    const created = new Set<() => void>();
    listeners.set(query, created);
    return created;
  };

  vi.stubGlobal("matchMedia", (query: string) => ({
    media: query,
    get matches() {
      return answers.get(query) ?? false;
    },
    addEventListener: (_type: "change", listener: () => void) => listenersFor(query).add(listener),
    removeEventListener: (_type: "change", listener: () => void) => listenersFor(query).delete(listener),
  }));

  return {
    /** Answers a query from now on, without firing `change`. */
    answer(query: string, matches: boolean) {
      answers.set(query, matches);
    },
    /** Answers a query and fires its `change` event, as a turning screen does. */
    change(query: string, matches: boolean) {
      answers.set(query, matches);
      act(() => {
        for (const listener of listenersFor(query)) listener();
      });
    },
    listenerCount(query: string) {
      return listenersFor(query).size;
    },
  };
}

let fakeScreen: ReturnType<typeof createFakeScreen>;

beforeEach(() => {
  fakeScreen = createFakeScreen();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("useMediaQuery", () => {
  it("follows the screen as it changes", () => {
    const { result } = renderHook(() => useMediaQuery(LANDSCAPE));
    expect(result.current).toBe(false);

    fakeScreen.change(LANDSCAPE, true);
    expect(result.current).toBe(true);

    fakeScreen.change(LANDSCAPE, false);
    expect(result.current).toBe(false);
  });

  it("catches a change between the first render and its subscription", () => {
    let turned = false;
    const { result } = renderHook(() => {
      const matches = useMediaQuery(LANDSCAPE);
      if (!turned) {
        // The screen turns after the first render has read the query, before
        // anything has subscribed, so no `change` event reaches the hook.
        turned = true;
        fakeScreen.answer(LANDSCAPE, true);
      }
      return matches;
    });

    // Reach guard: the turn happened during the first render.
    expect(turned).toBe(true);
    expect(result.current).toBe(true);
  });

  it("answers a new query on its first render, and moves its listener to it", () => {
    fakeScreen.answer(LANDSCAPE, true);
    const answersPerRender: boolean[] = [];
    const { rerender } = renderHook(
      ({ query }: { query: string }) => {
        const matches = useMediaQuery(query);
        answersPerRender.push(matches);
        return matches;
      },
      { initialProps: { query: PORTRAIT } },
    );
    expect(answersPerRender[answersPerRender.length - 1]).toBe(false);
    expect(fakeScreen.listenerCount(PORTRAIT)).toBe(1);

    answersPerRender.length = 0;
    rerender({ query: LANDSCAPE });

    // Every render after the switch answers for the new query, the first one
    // included: none paints the old query's answer.
    expect(answersPerRender.length).toBeGreaterThan(0);
    expect(answersPerRender.every((matches) => matches)).toBe(true);
    expect(fakeScreen.listenerCount(PORTRAIT)).toBe(0);
    expect(fakeScreen.listenerCount(LANDSCAPE)).toBe(1);
  });

  it("stops listening when unmounted", () => {
    const { unmount } = renderHook(() => useMediaQuery(LANDSCAPE));
    // Reach guard: it was listening.
    expect(fakeScreen.listenerCount(LANDSCAPE)).toBe(1);

    unmount();

    expect(fakeScreen.listenerCount(LANDSCAPE)).toBe(0);
  });
});
