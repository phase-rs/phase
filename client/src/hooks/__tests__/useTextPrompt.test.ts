import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { useTextPrompt } from "../useTextPrompt";

describe("useTextPrompt", () => {
  it("resolves with the entered value on confirm", async () => {
    const { result } = renderHook(() => useTextPrompt());

    let pending!: Promise<string | null>;
    act(() => {
      pending = result.current.request();
    });
    expect(result.current.open).toBe(true);

    act(() => {
      result.current.confirm("secret");
    });
    expect(result.current.open).toBe(false);
    await expect(pending).resolves.toBe("secret");
  });

  it("resolves with null on cancel", async () => {
    const { result } = renderHook(() => useTextPrompt());

    let pending!: Promise<string | null>;
    act(() => {
      pending = result.current.request();
    });

    act(() => {
      result.current.cancel();
    });
    expect(result.current.open).toBe(false);
    await expect(pending).resolves.toBeNull();
  });
});
