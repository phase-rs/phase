import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import i18n from "i18next";
import { draftProcedureFixture } from "../../adapter/__tests__/draftProcedureFixture";
import { resources, SUPPORTED_LNGS } from "../../i18n/resources";
import { refuseRealWebSockets } from "../../test/helpers/refusingWebSocket";

/**
 * The host's public-lobby listing controls, driven through the rendered
 * PodSetup form and the real `draftPodStore` / `multiplayerStore` — the
 * listing choice, its seat-ceiling gate, its remembered preference, and the
 * request it builds are all measured where production actually builds them.
 */

const mocks = vi.hoisted(() => ({
  draftProcedure: vi.fn(),
  loadActiveDraftPod: vi.fn(() => null),
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
    hostDraft: vi.fn(async () => true),
    resumeDraft: vi.fn(async () => "absent" as const),
    view: null as { kind: string; seats: { seat_index: number }[] } | null,
  },
  openBrokerClient: vi.fn<(url: string) => Promise<{ close: () => void }>>(),
}));

vi.mock("../../stores/multiplayerDraftStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../stores/multiplayerDraftStore")>()),
  useMultiplayerDraftStore: Object.assign(
    (selector: (state: typeof mocks.multiplayerState) => unknown) =>
      selector(mocks.multiplayerState),
    { getState: () => mocks.multiplayerState },
  ),
}));

// Only the adapter CLASS is replaced; the module's own shape helpers stay real.
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

vi.mock("../../services/brokerClient", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/brokerClient")>()),
  openBrokerClient: mocks.openBrokerClient,
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
vi.mock("../../components/draft/CubeSetupPanel", () => ({ CubeSetupPanel: () => null }));

import { DraftPodPage } from "../DraftPodPage";
import { useDraftPodStore } from "../../stores/draftPodStore";
import { useMultiplayerStore } from "../../stores/multiplayerStore";

/** `draft-pools.json` and `scryfall-sets.json`, as the selector fetches them. */
const POOLS: Record<string, unknown> = {
  isd: { code: "ISD", name: "Innistrad" },
};
const SCRYFALL_SETS = {
  isd: { name: "Innistrad", icon_svg_uri: "", released_at: "2011-09-30" },
};

function stubFetch(): void {
  vi.stubGlobal("__DRAFT_POOLS_URL__", "/draft-pools.json");
  vi.stubGlobal("__SCRYFALL_SETS_URL__", "/scryfall-sets.json");
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => ({
      ok: true,
      status: 200,
      json: async () => (url === "/scryfall-sets.json" ? SCRYFALL_SETS : POOLS),
    })),
  );
}

let socketUrls: string[] = [];

const ensureSubscriptionSocketMock = vi.fn();

/** Walk the host setup form as far as the set selector. */
async function openHostSetup(user: ReturnType<typeof userEvent.setup>) {
  const rendered = render(
    <MemoryRouter initialEntries={["/draft-pod"]}>
      <DraftPodPage />
    </MemoryRouter>,
  );
  await user.click(screen.getByRole("button", { name: /Host a Pod/ }));
  await user.type(screen.getByPlaceholderText(/name/i), "Host");
  await screen.findByRole("button", { name: /Add a pack of Innistrad/ });
  return rendered;
}

async function setPodSize(user: ReturnType<typeof userEvent.setup>, size: number): Promise<void> {
  await user.click(screen.getByRole("button", { name: "Pod Size" }));
  await user.click(screen.getByRole("option", { name: `${size} players` }));
}

async function addPack(user: ReturnType<typeof userEvent.setup>): Promise<void> {
  await user.click(screen.getByRole("button", { name: /Add a pack of Innistrad/ }));
}

function dispatchedHostConfig(): {
  podSize: number;
  listing?: { broker: unknown; request: Record<string, unknown> };
} {
  const [config] = mocks.multiplayerState.hostDraft.mock.calls[0] as unknown as [{
    podSize: number;
    listing?: { broker: unknown; request: Record<string, unknown> };
  }];
  return config;
}

describe("DraftPodPage lobby listing controls", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    socketUrls = refuseRealWebSockets();
    mocks.multiplayerState.phase = "idle";
    mocks.multiplayerState.view = null;
    mocks.multiplayerState.role = null;
    mocks.multiplayerState.roomCode = null;
    mocks.multiplayerState.hostDraft = vi.fn(async () => true);
    mocks.draftProcedure.mockResolvedValue(draftProcedureFixture({
      pod_size: 8,
      human_seats: 1,
      min_pod_size: 2,
      max_pod_size: 8,
      allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
      packs_per_player: 3,
      cards_per_pick: 1,
      distribution: "PickAndPass",
      min_deck_size: 40,
      match_config: { match_type: "Bo1" },
    }));
    mocks.openBrokerClient.mockReset().mockResolvedValue({ close: vi.fn() });
    stubFetch();
    localStorage.clear();
    ensureSubscriptionSocketMock.mockReset().mockImplementation(async () => ({
      serverInfo: { mode: "LobbyOnly" },
    }));
    useMultiplayerStore.setState({
      lastPodListingPublic: null,
      ensureSubscriptionSocket: ensureSubscriptionSocketMock,
    });
    useDraftPodStore.getState().reset();
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    const opened = [...socketUrls];
    expect(opened).toEqual([]);
  });

  it("starts listed for a host who has never chosen, directly under Pod Size", async () => {
    const user = userEvent.setup();
    await openHostSetup(user);

    const checkbox = screen.getByRole("checkbox", { name: "List in lobby" });
    expect(checkbox).toBeChecked();
    const podSizeButton = screen.getByRole("button", { name: "Pod Size" });
    const setTab = screen.getByRole("button", { name: "Set" });
    expect(
      podSizeButton.compareDocumentPosition(checkbox) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(
      checkbox.compareDocumentPosition(setTab) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("restores a host's remembered choice not to list", async () => {
    useMultiplayerStore.setState({ lastPodListingPublic: false });
    const user = userEvent.setup();
    await openHostSetup(user);

    expect(screen.getByRole("checkbox", { name: "List in lobby" })).not.toBeChecked();
  });

  it.each(["Set", "Cube"] as const)(
    "keeps the host's choice but disables listing above the lobby's seat ceiling in the %s pool",
    async (pool) => {
      const user = userEvent.setup();
      await openHostSetup(user);
      if (pool === "Cube") {
        await user.click(screen.getByRole("button", { name: "Cube" }));
      }

      const checkbox = screen.getByRole("checkbox", { name: "List in lobby" });
      expect(checkbox).toBeChecked();
      expect(checkbox).toBeDisabled();
      expect(
        screen.getByText("Lobby listing supports pods of up to 6 players."),
      ).toBeInTheDocument();
      expect(screen.queryByRole("checkbox", { name: "Set password" })).toBeNull();
      expect(screen.queryByLabelText(/Room Name/)).toBeNull();

      await setPodSize(user, 6);

      expect(checkbox).toBeChecked();
      expect(checkbox).toBeEnabled();
      expect(
        screen.queryByText("Lobby listing supports pods of up to 6 players."),
      ).toBeNull();
      expect(screen.getByLabelText(/Room Name/)).toBeInTheDocument();
    },
  );

  it("hides the password and room name when the pod is not listed", async () => {
    const user = userEvent.setup();
    await openHostSetup(user);
    await setPodSize(user, 6);

    await user.click(screen.getByRole("checkbox", { name: "List in lobby" }));

    expect(screen.queryByRole("checkbox", { name: "Set password" })).toBeNull();
    expect(screen.queryByLabelText(/Room Name/)).toBeNull();
  });

  it("lists with the password and room name entered in the form", async () => {
    const user = userEvent.setup();
    await openHostSetup(user);
    await setPodSize(user, 6);

    await user.click(screen.getByRole("checkbox", { name: "Set password" }));
    await user.type(screen.getByPlaceholderText("Pod password"), "pw");
    await user.type(screen.getByLabelText(/Room Name/), "Friday");
    await addPack(user);
    await user.click(screen.getByRole("button", { name: "Create Pod" }));

    await waitFor(() => expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce());
    const dispatched = dispatchedHostConfig();
    expect(dispatched.listing?.request).toMatchObject({ password: "pw", roomName: "Friday" });
    expect(mocks.openBrokerClient).toHaveBeenCalledOnce();
  });

  it("gives the password field an accessible name", async () => {
    const user = userEvent.setup();
    await openHostSetup(user);
    await setPodSize(user, 6);

    await user.click(screen.getByRole("checkbox", { name: "Set password" }));

    const passwordInput = screen.getByLabelText("Pod password");
    expect(passwordInput).toHaveAttribute("type", "password");
  });

  it("lists a blank room name under the placeholder the host was shown", async () => {
    const user = userEvent.setup();
    await openHostSetup(user);
    await setPodSize(user, 6);

    const roomNameInput = screen.getByLabelText(/Room Name/) as HTMLInputElement;
    const placeholder = roomNameInput.placeholder;
    await addPack(user);
    await user.click(screen.getByRole("button", { name: "Create Pod" }));

    await waitFor(() => expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce());
    expect(placeholder).toBe("Host's table");
    expect(dispatchedHostConfig().listing?.request.roomName).toBe(placeholder);
  });

  it("opens no lobby connection when the host does not list the pod", async () => {
    const user = userEvent.setup();
    await openHostSetup(user);
    await setPodSize(user, 6);
    await user.click(screen.getByRole("checkbox", { name: "List in lobby" }));
    await addPack(user);
    await user.click(screen.getByRole("button", { name: "Create Pod" }));

    await waitFor(() => expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce());
    expect(dispatchedHostConfig()).not.toHaveProperty("listing");
    expect(mocks.openBrokerClient).not.toHaveBeenCalled();
  });

  it("remembers the host's choice when a pod is created, even while the seat ceiling holds it off", async () => {
    const user = userEvent.setup();
    const first = await openHostSetup(user);
    expect(screen.getByRole("checkbox", { name: "List in lobby" })).toBeChecked();
    await addPack(user);
    await user.click(screen.getByRole("button", { name: "Create Pod" }));
    await waitFor(() => expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce());
    expect(useMultiplayerStore.getState().lastPodListingPublic).toBe(true);
    first.unmount();

    mocks.multiplayerState.hostDraft.mockClear();
    useDraftPodStore.getState().reset();
    const second = await openHostSetup(user);
    await setPodSize(user, 6);
    await user.click(screen.getByRole("checkbox", { name: "List in lobby" }));
    await addPack(user);
    await user.click(screen.getByRole("button", { name: "Create Pod" }));
    await waitFor(() => expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce());
    expect(useMultiplayerStore.getState().lastPodListingPublic).toBe(false);
    second.unmount();

    useDraftPodStore.getState().reset();
    await openHostSetup(user);
    expect(screen.getByRole("checkbox", { name: "List in lobby" })).not.toBeChecked();
  });

  it("does not accept a display name, room name or password past the lobby's bounds", async () => {
    const user = userEvent.setup();
    await openHostSetup(user);
    await setPodSize(user, 6);

    const nameInput = screen.getByPlaceholderText(/name/i) as HTMLInputElement;
    await user.clear(nameInput);
    await user.type(nameInput, "A".repeat(25));
    expect(nameInput.value).toHaveLength(20);

    const roomNameInput = screen.getByLabelText(/Room Name/) as HTMLInputElement;
    await user.type(roomNameInput, "B".repeat(45));
    expect(roomNameInput.value).toHaveLength(40);

    await user.click(screen.getByRole("checkbox", { name: "Set password" }));
    const passwordInput = screen.getByPlaceholderText("Pod password") as HTMLInputElement;
    await user.type(passwordInput, "C".repeat(40));
    expect(passwordInput.value).toHaveLength(32);
  });

  it("clears the password when Set password is turned off", async () => {
    const user = userEvent.setup();
    await openHostSetup(user);
    await setPodSize(user, 6);

    await user.click(screen.getByRole("checkbox", { name: "Set password" }));
    await user.type(screen.getByPlaceholderText("Pod password"), "pw");
    await user.click(screen.getByRole("checkbox", { name: "Set password" }));

    expect(useDraftPodStore.getState().listing.password).toBe("");

    await user.click(screen.getByRole("checkbox", { name: "Set password" }));
    expect((screen.getByPlaceholderText("Pod password") as HTMLInputElement).value).toBe("");
  });

  it("keeps a set password visible after PodSetup remounts without a reset", async () => {
    const user = userEvent.setup();
    const first = await openHostSetup(user);
    await setPodSize(user, 6);
    await user.click(screen.getByRole("checkbox", { name: "Set password" }));
    await user.type(screen.getByPlaceholderText("Pod password"), "pw");
    first.unmount();

    await openHostSetup(user);

    expect(screen.getByRole("checkbox", { name: "Set password" })).toBeChecked();
    expect((screen.getByPlaceholderText("Pod password") as HTMLInputElement).value).toBe("pw");
  });

  it.each(SUPPORTED_LNGS)(
    "labels the optional room name without doubled brackets in %s",
    async (lng) => {
      const bundle = resources[lng] as {
        draft: { podSetup: { roomName: string; optional: string; hostCardTitle: string } };
        multiplayer: Record<string, unknown>;
      };
      i18n.addResourceBundle(lng, "draft", bundle.draft, true, true);
      i18n.addResourceBundle(lng, "multiplayer", bundle.multiplayer, true, true);
      await i18n.changeLanguage(lng);
      try {
        const user = userEvent.setup();
        render(
          <MemoryRouter initialEntries={["/draft-pod"]}>
            <DraftPodPage />
          </MemoryRouter>,
        );
        const hostTitlePattern = new RegExp(
          bundle.draft.podSetup.hostCardTitle.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"),
        );
        await user.click(screen.getByRole("button", { name: hostTitlePattern }));
        await screen.findByRole("checkbox");
        await user.click(
          screen.getByRole("button", { name: i18n.t("draft:podSetup.podSize", { lng }) }),
        );
        await user.click(
          screen.getByRole("option", {
            name: i18n.t("draft:podSetup.playerCount", { count: 6, lng }),
          }),
        );

        const label = document.querySelector('label[for="pod-setup-room-name"]');
        expect(label?.textContent?.replace(/\s+/g, " ").trim()).toBe(
          `${bundle.draft.podSetup.roomName} ${bundle.draft.podSetup.optional}`,
        );
        for (const doubled of ["((", "))", "（（", "））"]) {
          expect(label?.textContent ?? "").not.toContain(doubled);
        }
      } finally {
        await i18n.changeLanguage("en");
      }
    },
  );

  it("tells the host how to create an unlisted pod when the lobby cannot be reached", async () => {
    const user = userEvent.setup();
    await openHostSetup(user);
    await setPodSize(user, 6);
    mocks.openBrokerClient.mockReset().mockRejectedValueOnce(new Error("refused"));
    await addPack(user);
    await user.click(screen.getByRole("button", { name: "Create Pod" }));

    await screen.findByText(
      "Couldn't reach the lobby to list this pod. Turn off “List in lobby” to host by room code.",
    );
    expect(mocks.multiplayerState.hostDraft).not.toHaveBeenCalled();

    await user.click(screen.getByRole("checkbox", { name: "List in lobby" }));
    await user.click(screen.getByRole("button", { name: "Create Pod" }));

    await waitFor(() => expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce());
    expect(dispatchedHostConfig()).not.toHaveProperty("listing");
  });
});
