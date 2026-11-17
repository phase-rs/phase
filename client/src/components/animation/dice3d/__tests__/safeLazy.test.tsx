import { Suspense } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { resetWarnOnce, safeLazy } from "../safeLazy";

function Fallback({ label }: { label: string }) {
  return <span>{`fallback:${label}`}</span>;
}
function Real({ label }: { label: string }) {
  return <span>{`real:${label}`}</span>;
}

async function renderLazy(Lazy: React.ComponentType<{ label: string }>) {
  render(
    <Suspense fallback={<span>loading</span>}>
      <Lazy label="x" />
    </Suspense>,
  );
  await screen.findByText(/^(fallback|real):x$/);
}

describe("safeLazy", () => {
  let warn: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    resetWarnOnce();
    warn = vi.spyOn(console, "warn").mockImplementation(() => {});
  });
  afterEach(() => {
    cleanup();
    warn.mockRestore();
  });

  it("renders the real component when the chunk resolves with its export", async () => {
    await renderLazy(safeLazy("T", async () => ({ Real }), (m) => m.Real, Fallback));
    expect(screen.getByText("real:x")).toBeTruthy();
    expect(warn).not.toHaveBeenCalled();
  });

  it("falls back, without throwing, when import() resolves undefined", async () => {
    await renderLazy(
      safeLazy<{ label: string }, { Real: typeof Real }>("T", async () => undefined, (m) => m.Real, Fallback),
    );
    expect(screen.getByText("fallback:x")).toBeTruthy();
    expect(warn).toHaveBeenCalledTimes(1);
  });

  it("falls back when the module resolves without the expected export", async () => {
    await renderLazy(
      safeLazy<{ label: string }, { Real?: typeof Real }>("T", async () => ({}), (m) => m.Real, Fallback),
    );
    expect(screen.getByText("fallback:x")).toBeTruthy();
  });

  it("falls back when the import rejects", async () => {
    await renderLazy(
      safeLazy<{ label: string }, { Real: typeof Real }>(
        "T",
        () => Promise.reject(new Error("Failed to fetch dynamically imported module")),
        (m) => m.Real,
        Fallback,
      ),
    );
    expect(screen.getByText("fallback:x")).toBeTruthy();
  });

  it("logs a degraded chunk only once", async () => {
    const lazyA = safeLazy<{ label: string }, { Real?: typeof Real }>("Once", async () => ({}), (m) => m.Real, Fallback);
    const lazyB = safeLazy<{ label: string }, { Real?: typeof Real }>("Once", async () => ({}), (m) => m.Real, Fallback);
    await renderLazy(lazyA);
    await renderLazy(lazyB);
    expect(warn).toHaveBeenCalledTimes(1);
  });
});
