import type { ReactNode } from "react";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { NativeEngineProgress } from "../../../services/nativeEngine";

const mocks = vi.hoisted(() => {
  let listener: ((progress: NativeEngineProgress) => void) | undefined;
  const unlisten = vi.fn();

  return {
    emitProgress(progress: NativeEngineProgress) {
      listener?.(progress);
    },
    subscribeNativeEngineProgress: vi.fn(async (next: (progress: NativeEngineProgress) => void) => {
      listener = next;
      return unlisten;
    }),
    // Annotated to the real signature: inferring from the `null` default would
    // pin the mock to `Promise<null>` and reject every replay value below.
    getNativeEngineProgress: vi.fn(async (): Promise<NativeEngineProgress | null> => null),
    invoke: vi.fn(),
    isDesktopTauri: vi.fn(() => true),
    unlisten,
  };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("../../../services/platform", () => ({ isDesktopTauri: mocks.isDesktopTauri }));

// Only the shell's event bridge is faked. `ensureNativeEngine` and
// `useNativeEngineProvisioning` stay real so these tests exercise the actual
// coupling between an in-flight provisioning call and the overlay's lifetime.
vi.mock("../../../services/nativeEngine", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../services/nativeEngine")>()),
  getNativeEngineProgress: mocks.getNativeEngineProgress,
  subscribeNativeEngineProgress: mocks.subscribeNativeEngineProgress,
}));

// Exit animations would keep the dismissed overlay mounted past the assertion;
// these tests are about when the overlay decides to leave, not how it leaves.
vi.mock("framer-motion", () => ({
  AnimatePresence: ({ children }: { children: ReactNode }) => <>{children}</>,
  motion: {
    div: ({
      children,
      initial: _initial,
      animate: _animate,
      exit: _exit,
      ...props
    }: {
      children: ReactNode;
      initial?: unknown;
      animate?: unknown;
      exit?: unknown;
    } & Record<string, unknown>) => <div {...props}>{children}</div>,
  },
}));

import { ensureNativeEngine, type NativeEngineKey } from "../../../services/nativeEngine";
import { NativeEngineProgressOverlay } from "../NativeEngineProgressOverlay";

const KEY: NativeEngineKey = { release: { version: "1.2.3" } };

// The provisioning store is module state shared by every test in this file, so
// a call left unsettled by a failure would keep the next test's overlay open.
const unsettledCalls = new Set<(ok: boolean) => Promise<void>>();

/** Starts a provisioning call whose shell response this test controls. */
function startProvisioning() {
  let settle!: (ok: boolean) => void;
  // `ensureNativeEngine` reaches `invoke` only after its dynamic import
  // resolves, so a test that settles immediately has to wait for that first.
  const reachedShell = new Promise<void>((resolveReached) => {
    mocks.invoke.mockImplementationOnce(
      () =>
        new Promise((resolve, reject) => {
          settle = (ok) => (ok ? resolve({ port: 4321 }) : reject(new Error("spawn failed")));
          resolveReached();
        }),
    );
  });
  // Starting the call flips the provisioning store the overlay subscribes to,
  // so React has to commit that before a test can advance timers against it.
  let call!: Promise<unknown>;
  act(() => {
    call = ensureNativeEngine(KEY).catch(() => undefined);
  });

  const finish = async (ok: boolean) => {
    unsettledCalls.delete(finish);
    await act(async () => {
      await reachedShell;
    });
    settle(ok);
    await act(async () => {
      await call;
    });
  };
  unsettledCalls.add(finish);
  return { ready: () => finish(true), fail: () => finish(false) };
}

describe("NativeEngineProgressOverlay", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(async () => {
    for (const finish of [...unsettledCalls]) await finish(true);
    cleanup();
    vi.useRealTimers();
  });

  it("clearly shows native server downloads and their artifact key", async () => {
    const provisioning = startProvisioning();
    render(<NativeEngineProgressOverlay />);

    act(() => {
      mocks.emitProgress({
        phase: "downloading_binary",
        detail: "preview-0123456789abcdef",
      });
    });

    const status = await screen.findByRole("status");
    expect(status).toHaveTextContent("Updating native engine");
    expect(status).toHaveTextContent("Downloading updated server…");
    expect(status).toHaveTextContent("preview-0123456789abcdef");
    await provisioning.ready();
  });

  it("shows native-engine completion as a non-busy status", async () => {
    const provisioning = startProvisioning();
    render(<NativeEngineProgressOverlay />);

    act(() => {
      mocks.emitProgress({ phase: "ready" });
    });
    expect(await screen.findByRole("status")).toHaveAttribute("aria-busy", "true");

    await provisioning.ready();

    const status = screen.getByRole("status");
    expect(status).toHaveTextContent("Native engine ready");
    expect(status).toHaveAttribute("aria-busy", "false");
  });

  it("dismisses a failed attempt that the shell never reports as terminal", async () => {
    // Shells older than the `failed` phase stop emitting when provisioning
    // fails. The overlay must still come down — it used to sit over the WASM
    // fallback game for the rest of the session.
    const provisioning = startProvisioning();
    render(<NativeEngineProgressOverlay />);

    act(() => {
      mocks.emitProgress({ phase: "spawning" });
    });
    expect(await screen.findByRole("status")).toHaveTextContent("Starting local server…");

    vi.useFakeTimers();
    await provisioning.fail();
    act(() => {
      vi.advanceTimersByTime(1_500);
    });

    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("does not flash over an engine that is already running", async () => {
    // The overwhelmingly common call reuses a live engine and answers in
    // milliseconds; a full-screen blink on every game start reads as a bug. The
    // overlay must still be hidden that early even while the call is in flight.
    vi.useFakeTimers();
    render(<NativeEngineProgressOverlay />);
    const provisioning = startProvisioning();

    act(() => {
      vi.advanceTimersByTime(200);
    });
    expect(screen.queryByRole("status")).not.toBeInTheDocument();

    await provisioning.ready();
    act(() => {
      vi.advanceTimersByTime(2_000);
    });
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("stays hidden when nothing is being provisioned", async () => {
    mocks.getNativeEngineProgress.mockResolvedValueOnce({ phase: "ready" });

    render(<NativeEngineProgressOverlay />);
    await vi.waitFor(() => expect(mocks.getNativeEngineProgress).toHaveBeenCalledOnce());

    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("replays progress emitted before the overlay mounted", async () => {
    mocks.getNativeEngineProgress.mockResolvedValueOnce({ phase: "resolving" });
    const provisioning = startProvisioning();

    render(<NativeEngineProgressOverlay />);

    expect(await screen.findByRole("status")).toHaveTextContent("Finding the correct local server…");
    await provisioning.ready();
  });

  it("keeps live progress when the replay snapshot is stale", async () => {
    mocks.subscribeNativeEngineProgress.mockImplementationOnce(async (next) => {
      next({ phase: "downloading_data" });
      return mocks.unlisten;
    });
    mocks.getNativeEngineProgress.mockResolvedValueOnce({ phase: "resolving" });
    const provisioning = startProvisioning();

    render(<NativeEngineProgressOverlay />);

    expect(await screen.findByRole("status")).toHaveTextContent("Downloading game data…");
    await provisioning.ready();
  });
});
