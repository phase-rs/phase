import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { useGameViewportLock } from "../useGameViewportLock.ts";

const LOCK_CLASS = "game-viewport-lock";

describe("useGameViewportLock", () => {
  afterEach(() => {
    cleanup();
    document.documentElement.classList.remove(LOCK_CLASS);
    document.body.classList.remove(LOCK_CLASS);
  });

  it("locks the document only while the game page is mounted", () => {
    const { unmount } = renderHook(() => useGameViewportLock());

    expect(document.documentElement).toHaveClass(LOCK_CLASS);
    expect(document.body).toHaveClass(LOCK_CLASS);

    unmount();

    expect(document.documentElement).not.toHaveClass(LOCK_CLASS);
    expect(document.body).not.toHaveClass(LOCK_CLASS);
  });

  it("does not remove a lock owned by an outer game viewport", () => {
    document.documentElement.classList.add(LOCK_CLASS);
    document.body.classList.add(LOCK_CLASS);
    const { unmount } = renderHook(() => useGameViewportLock());

    unmount();

    expect(document.documentElement).toHaveClass(LOCK_CLASS);
    expect(document.body).toHaveClass(LOCK_CLASS);
  });

  it("keeps the document locked until the last overlapping hook unmounts", () => {
    const first = renderHook(() => useGameViewportLock());
    const second = renderHook(() => useGameViewportLock());

    first.unmount();

    expect(document.documentElement).toHaveClass(LOCK_CLASS);
    expect(document.body).toHaveClass(LOCK_CLASS);

    second.unmount();

    expect(document.documentElement).not.toHaveClass(LOCK_CLASS);
    expect(document.body).not.toHaveClass(LOCK_CLASS);
  });
});
