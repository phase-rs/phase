import { cleanup, render } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  watchDraft: vi.fn(async () => {}),
  leave: vi.fn(),
}));

// `normalizeSpectatorDraftCode` stays the real one: the page's code
// normalisation is what these assertions are checking, so stubbing it would
// let a broken normalisation pass. Only the store HOOK is replaced.
vi.mock("../../stores/draftSpectatorStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../stores/draftSpectatorStore")>()),
  useDraftSpectatorStore: (
    selector: (state: Record<string, unknown>) => unknown,
  ) =>
    selector({
      draftCode: null,
      status: "idle",
      view: null,
      error: null,
      session: null,
      watchDraft: mocks.watchDraft,
      leave: mocks.leave,
    }),
}));

vi.mock("../../audio/useAudioContext", () => ({ useAudioContext: () => undefined }));
vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/menu/MenuParticles", () => ({ MenuParticles: () => null }));
vi.mock("../../components/draft/DraftSpectatorDashboard", () => ({
  DraftSpectatorDashboard: () => null,
}));

import { DraftSpectatorPage } from "../DraftSpectatorPage";

function renderAt(entry: string) {
  return render(
    <MemoryRouter initialEntries={[entry]}>
      <Routes>
        <Route path="/draft-spectator" element={<DraftSpectatorPage />} />
      </Routes>
    </MemoryRouter>,
  );
}

describe("DraftSpectatorPage", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(cleanup);

  it("passes the route's server to watchDraft", () => {
    renderAt(
      "/draft-spectator?code=abc123&server=wss%3A%2F%2Fplay.example.com%2Fws",
    );

    // The code keeps its existing normalisation; the origin rides beside it.
    expect(mocks.watchDraft).toHaveBeenCalledWith("ABC123", "wss://play.example.com/ws");
  });

  it("passes no origin when the route carried none", () => {
    renderAt("/draft-spectator?code=abc123");

    expect(mocks.watchDraft).toHaveBeenCalledWith("ABC123", undefined);
  });
});
