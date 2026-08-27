import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ActiveDraftPodMeta } from "../../services/draftPersistence";

const mocks = vi.hoisted(() => ({
  navigate: vi.fn(),
  loadActiveQuickDraft: vi.fn(() => null),
  loadActiveDraftPod: vi.fn((): ActiveDraftPodMeta | null => null),
  loadActiveDraftGuest: vi.fn(() => null),
  loadGame: vi.fn(async () => null),
}));

vi.mock("react-router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-router")>()),
  useNavigate: () => mocks.navigate,
}));

// `ScreenChrome` reaches `ChromeControls -> AccountControl`, which is unrelated
// to the tile grid under test.
vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));

// The landing page renders a resume card off localStorage; pin both persistence
// reads so no ambient browser state decides what renders.
vi.mock("../../services/quickDraftPersistence", () => ({
  loadActiveQuickDraft: mocks.loadActiveQuickDraft,
}));
vi.mock("../../services/draftPersistence", () => ({
  loadActiveDraftPod: mocks.loadActiveDraftPod,
  loadActiveDraftGuest: mocks.loadActiveDraftGuest,
}));
vi.mock("../../services/gamePersistence", () => ({ loadGame: mocks.loadGame }));

// No store is mocked here, and none needs to be: `COMMANDER_DRAFT_ENTRY` and
// `draftKindLabels` come from the leaf module `components/draft/draftKind`, so this
// page value-imports no store at all. Its `lazy()` chunk therefore does not pull
// `multiplayerDraftStore -> draftPodHostAdapter -> p2p-draft-host -> network/
// connection` or the game loop. The slug and the labels asserted below are real.

import { DraftLandingPage } from "../DraftLandingPage";

function podMeta(overrides: Partial<ActiveDraftPodMeta> = {}): ActiveDraftPodMeta {
  return {
    id: "pod-1",
    roomCode: "ABCDE",
    kind: "CommanderDraft",
    podSize: 4,
    hostDisplayName: "Host",
    tournamentFormat: "Swiss",
    podPolicy: "Competitive",
    phase: "lobby",
    pickCount: 0,
    updatedAt: Date.now(),
    ...overrides,
  };
}

function renderPage() {
  return render(
    <MemoryRouter>
      <DraftLandingPage />
    </MemoryRouter>,
  );
}

describe("DraftLandingPage Commander Draft entry", () => {
  afterEach(cleanup);

  beforeEach(() => {
    mocks.navigate.mockClear();
    mocks.loadActiveQuickDraft.mockReturnValue(null);
    mocks.loadActiveDraftPod.mockReturnValue(null);
  });

  it("deep-links the Commander tile into pod setup", async () => {
    const user = userEvent.setup();
    renderPage();

    // Reach guard: the tile grid rendered, so an absent Commander tile below
    // would be a real absence rather than a failed render.
    expect(screen.getByRole("button", { name: /Pod Draft/ })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Commander Draft/ }));

    // REVERT-FAILING: at BASE there is no fifth tile at all, and the `?kind=`
    // slug is the contract `DraftPodPage`'s entry effect reads.
    expect(mocks.navigate).toHaveBeenCalledWith("/draft-pod?kind=commander");
  });

  it("leaves the existing Pod Draft route alone", async () => {
    const user = userEvent.setup();
    renderPage();

    await user.click(screen.getByRole("button", { name: /Pod Draft/ }));

    expect(mocks.navigate).toHaveBeenCalledWith("/draft-pod");
  });

  it("labels a resumed Commander pod in prose, not as a raw enum", () => {
    mocks.loadActiveDraftPod.mockReturnValue(podMeta({ kind: "CommanderDraft" }));
    renderPage();

    // REVERT-FAILING: `t("landing.podLabel", { kind: meta.kind })` renders
    // "CommanderDraft Pod" — the raw wire enum — for this exact fixture.
    expect(screen.getByText("Commander Pod")).toBeInTheDocument();
    // Paired negative — non-vacuous because the positive above proves the card
    // rendered and this exact string is what BASE produces.
    expect(screen.queryByText("CommanderDraft Pod")).toBeNull();
  });

  it("still labels the pre-existing kinds from the same map", () => {
    mocks.loadActiveDraftPod.mockReturnValue(podMeta({ kind: "Premier" }));
    renderPage();

    // Sibling guard: the label map did not become Commander-specific.
    expect(screen.getByText("Premier Pod")).toBeInTheDocument();
  });
});
