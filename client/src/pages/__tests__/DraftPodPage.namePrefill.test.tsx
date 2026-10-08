import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { draftProcedureFixture } from "../../adapter/__tests__/draftProcedureFixture";

const mocks = vi.hoisted(() => ({
  draftProcedure: vi.fn(),
  loadActiveDraftPod: vi.fn(() => null),
  // Both recovery probes the page runs on mount. `absent` terminates them, so
  // the setup form this suite measures is what renders. That no persisted host
  // session reaches the store — which would mask the seed — is asserted rather
  // than assumed: see the `loadDraftHostSession` check in the first test.
  inspectActiveDraftPod: vi.fn(() => ({ type: "absent" })),
  clearActiveDraftPodIfCurrent: vi.fn(),
  persistedDraftHostSessionState: vi.fn(() => "live"),
  loadDraftHostSession: vi.fn(),
  clearActiveDraftPod: vi.fn(),
  multiplayerState: {
    phase: "idle",
    leave: vi.fn(async () => {}),
    role: null as "host" | "guest" | null,
    roomCode: null as string | null,
    hostDraft: vi.fn(async () => {}),
    resumeDraft: vi.fn(async () => "absent" as const),
    view: null,
  },
}));

// `DraftPodPage` selects off this store and the REAL `draftPodStore` calls
// `.getState()` on it, so the mock serves both shapes.
vi.mock("../../stores/multiplayerDraftStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../stores/multiplayerDraftStore")>()),
  useMultiplayerDraftStore: Object.assign(
    (selector: (state: typeof mocks.multiplayerState) => unknown) =>
      selector(mocks.multiplayerState),
    { getState: () => mocks.multiplayerState, subscribe: () => () => {} },
  ),
}));

// Only the adapter CLASS is replaced. The real exports have to survive the
// mock because this suite's own `draftProcedureFixture` calls
// `isSharedStackDistribution` to build `allowed_set_layouts`
// (adapter/__tests__/draftProcedureFixture.ts).
vi.mock("../../adapter/draft-adapter", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../adapter/draft-adapter")>()),
  DraftAdapter: class {
    draftProcedure = mocks.draftProcedure;
  },
}));

vi.mock("../../services/draftPersistence", () => ({
  loadActiveDraftPod: mocks.loadActiveDraftPod,
  inspectActiveDraftPod: mocks.inspectActiveDraftPod,
  clearActiveDraftPodIfCurrent: mocks.clearActiveDraftPodIfCurrent,
  persistedDraftHostSessionState: mocks.persistedDraftHostSessionState,
  loadDraftHostSession: mocks.loadDraftHostSession,
  clearActiveDraftPod: mocks.clearActiveDraftPod,
}));

vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/menu/MenuShell", () => ({
  MenuShell: ({ children }: { children: ReactNode }) => <>{children}</>,
}));
vi.mock("../../components/draft/HostControls", () => {
  const emptyTopActions: readonly [] = [];
  return {
    HostControls: () => null,
    useHostDraftTopActions: (_options: { enabled: boolean }) => emptyTopActions,
  };
});
vi.mock("../../components/draft/SetSelector", () => ({ SetSelector: () => null }));

import { DraftPodPage } from "../DraftPodPage";
import { useDraftPodStore } from "../../stores/draftPodStore";
import { useMultiplayerStore } from "../../stores/multiplayerStore";

/** The one input this suite is about. Both the host and the join form render
 *  exactly one, under the shared `podSetup.namePlaceholder`; the room-code
 *  input beside it carries a different placeholder, so this cannot match it. */
function nameInput(): HTMLInputElement {
  return screen.getByPlaceholderText("Enter your name...") as HTMLInputElement;
}

function renderPage() {
  return render(
    <MemoryRouter initialEntries={["/draft-pod"]}>
      <DraftPodPage />
    </MemoryRouter>,
  );
}

describe("DraftPodPage saved-name prefill", () => {
  afterEach(cleanup);

  beforeEach(() => {
    vi.clearAllMocks();
    mocks.multiplayerState.phase = "idle";
    mocks.multiplayerState.view = null;
    mocks.loadActiveDraftPod.mockReturnValue(null);
    mocks.draftProcedure.mockResolvedValue(draftProcedureFixture());
    mocks.inspectActiveDraftPod.mockReturnValue({ type: "absent" });
    // `useMultiplayerStore` is one module-level instance shared by every test
    // in this file, so the identity has to be put back between them or the
    // first test that sets it leaks forward.
    useMultiplayerStore.getState().setDisplayName("");
    useDraftPodStore.getState().reset();
  });

  it.each([
    ["Host a Pod", /Host a Pod/, "hostDisplayName"],
    ["Join a Pod", /Join a Pod/, "guestDisplayName"],
  ])("pre-fills the %s form with the saved name", async (_label, card, field) => {
    useMultiplayerStore.getState().setDisplayName("Alice");
    const user = userEvent.setup();
    renderPage();

    await user.click(screen.getByRole("button", { name: card }));

    // REVERT-FAILING: BASE seeds both fields from `initialState`'s "", so the
    // input renders empty and this reads "".
    expect(nameInput().value).toBe("Alice");
    expect(useDraftPodStore.getState()[field as "hostDisplayName" | "guestDisplayName"]).toBe("Alice");
    // The seed is what put "Alice" there, not a restored host session — which
    // would overwrite `hostDisplayName`, so this does the discriminating work
    // on the host row. The `absent` probe means it never ran.
    expect(mocks.loadDraftHostSession).not.toHaveBeenCalled();
  });

  it("leaves the pre-filled name editable", async () => {
    useMultiplayerStore.getState().setDisplayName("Alice");
    const user = userEvent.setup();
    renderPage();
    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));

    await user.clear(nameInput());
    await user.type(nameInput(), "Bea");

    // Discriminates a seed that re-applies on every render: measured, dropping
    // the effect's dependency array lands "AliceBea" here instead of "Bea".
    expect(nameInput().value).toBe("Bea");
    expect(useDraftPodStore.getState().hostDisplayName).toBe("Bea");
  });

  it("enables Join Pod on the seeded name alone, once a code is entered", async () => {
    useMultiplayerStore.getState().setDisplayName("Alice");
    const user = userEvent.setup();
    renderPage();
    await user.click(screen.getByRole("button", { name: /Join a Pod/ }));

    await user.type(screen.getByPlaceholderText("Enter room code..."), "ABCDE");

    // The button's `disabled` reads `guestDisplayName.trim()`, so this is the
    // half that proves the seed is real form state and not a rendered
    // placeholder: on BASE the guest still has to type a name to get here.
    expect(screen.getByRole("button", { name: "Join Pod" })).toBeEnabled();
  });

  it("leaves the field empty when no name has been saved", async () => {
    const user = userEvent.setup();
    renderPage();

    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));

    // Reach guard: the form rendered, so an empty value is this assertion's
    // subject rather than a missing input.
    expect(nameInput()).toBeInTheDocument();
    expect(nameInput().value).toBe("");
  });
});
