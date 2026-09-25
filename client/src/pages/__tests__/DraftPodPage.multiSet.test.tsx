import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { draftProcedureFixture } from "../../adapter/__tests__/draftProcedureFixture";
import { refuseRealWebSockets } from "../../test/helpers/refusingWebSocket";

/**
 * The pod host's set selection, end to end through the page.
 *
 * The claim under test is the one a multi-set draft lives or dies on for
 * multiplayer: a host can arrange SEVERAL sets, in an order they choose, and
 * that order reaches the host boundary intact. Before multi-set pods, this page
 * pinned the selector to its single-set mode and kept only one code, so a pod
 * could not express a mixed pool at all — every assertion below fails against
 * that shape.
 *
 * The real `SetSelector` and the real `draftPodStore` render here; only the
 * transport (`multiplayerDraftStore`), the wasm adapter class, and persistence
 * are replaced, so the pack list the host builds is measured where it is
 * actually handed off.
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
}));

vi.mock("../../stores/multiplayerDraftStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../stores/multiplayerDraftStore")>()),
  useMultiplayerDraftStore: Object.assign(
    (selector: (state: typeof mocks.multiplayerState) => unknown) =>
      selector(mocks.multiplayerState),
    { getState: () => mocks.multiplayerState },
  ),
}));

// Only the adapter CLASS is replaced. `setPackSequence` and `distinctJoined`
// are the boundary's own shape logic — stubbing them would let a wrong pack
// sequence pass this suite.
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
vi.mock("../../components/draft/CubeSetupPanel", () => ({ CubeSetupPanel: () => null }));

import { DraftPodPage } from "../DraftPodPage";
import { useDraftPodStore } from "../../stores/draftPodStore";
import { useMultiplayerStore } from "../../stores/multiplayerStore";

/** `draft-pools.json` and `scryfall-sets.json`, as the selector fetches them. */
const POOLS: Record<string, unknown> = {
  isd: { code: "ISD", name: "Innistrad" },
  dka: { code: "DKA", name: "Dark Ascension" },
};
const SCRYFALL_SETS = {
  isd: { name: "Innistrad", icon_svg_uri: "", released_at: "2011-09-30" },
  dka: { name: "Dark Ascension", icon_svg_uri: "", released_at: "2012-02-03" },
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

/** The `poolInput` the page handed the host adapter. */
function hostedPoolInput(): { type: string; data: { pools: unknown[]; sequence: string[] } } {
  const [config] = mocks.multiplayerState.hostDraft.mock.calls[0] as unknown as [
    { poolInput: { type: string; data: { pools: unknown[]; sequence: string[] } } },
  ];
  return config.poolInput;
}

function hostedChaosPoolInput(): { type: string; data: { pools: unknown[]; candidate_codes: string[] } } {
  const [config] = mocks.multiplayerState.hostDraft.mock.calls[0] as unknown as [
    { poolInput: { type: string; data: { pools: unknown[]; candidate_codes: string[] } } },
  ];
  return config.poolInput;
}

/** Walk the host setup form as far as the set selector. */
async function openHostSetup(user: ReturnType<typeof userEvent.setup>): Promise<void> {
  render(
    <MemoryRouter initialEntries={["/draft-pod"]}>
      <DraftPodPage />
    </MemoryRouter>,
  );
  await user.click(screen.getByRole("button", { name: /Host a Pod/ }));
  await user.type(screen.getByPlaceholderText(/name/i), "Host");
  // The selector only renders once the engine has published this kind's
  // booster count, so the page never guesses one.
  await screen.findByRole("button", { name: /Add a pack of Innistrad/ });
}

describe("DraftPodPage host set selection", () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    const opened = [...socketUrls];
    expect(opened).toEqual([]);
  });

  beforeEach(() => {
    vi.clearAllMocks();
    socketUrls = refuseRealWebSockets();
    mocks.multiplayerState.phase = "idle";
    mocks.multiplayerState.view = null;
    mocks.multiplayerState.role = null;
    mocks.multiplayerState.roomCode = null;
    mocks.draftProcedure.mockResolvedValue(draftProcedureFixture({
      pod_size: 8,
      human_seats: 1,
      min_pod_size: 3,
      max_pod_size: 8,
      allowed_pod_sizes: [3, 4, 5, 6, 7, 8],
      packs_per_player: 3,
      cards_per_pick: 1,
      distribution: "PickAndPass",
      min_deck_size: 40,
      match_config: { match_type: "Bo1" },
    }));
    stubFetch();
    useMultiplayerStore.setState({ lastPodListingPublic: false });
    useDraftPodStore.getState().reset();
  });

  it("lets a host arrange several sets and ships that order to the pod", async () => {
    const user = userEvent.setup();
    await openHostSetup(user);

    // Three boosters, two sets, the first set drafted twice — an order no
    // single-set pod could express, and one that dedupe or sorting would lose.
    await user.click(screen.getByRole("button", { name: /Add a pack of Innistrad/ }));
    await user.click(screen.getByRole("button", { name: /Add a pack of Dark Ascension/ }));
    await user.click(screen.getByRole("button", { name: /Add a pack of Innistrad/ }));

    await user.click(screen.getByRole("button", { name: "Create Pod" }));

    await waitFor(() => expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce());
    const poolInput = hostedPoolInput();
    expect(poolInput.type).toBe("Set");
    expect(poolInput.data.sequence).toEqual(["ISD", "DKA", "ISD"]);
    // One pool per DISTINCT set — the sequence is what repeats.
    expect(poolInput.data.pools).toEqual([{ code: "ISD", name: "Innistrad" }, { code: "DKA", name: "Dark Ascension" }]);
    // The label dedupes, mirroring the engine's `DraftSource::set_code`.
    expect(useDraftPodStore.getState().config.setCode).toBe("ISD+DKA");
  });

  it("still creates a single-set pod from one chosen set", async () => {
    const user = userEvent.setup();
    await openHostSetup(user);

    await user.click(screen.getByRole("button", { name: /Add a pack of Innistrad/ }));
    await user.click(screen.getByRole("button", { name: "Create Pod" }));

    await waitFor(() => expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce());
    // A one-element sequence, not the code copied once per booster: the engine
    // repeats the last entry to fill all three packs.
    expect(hostedPoolInput().data.sequence).toEqual(["ISD"]);
    expect(useDraftPodStore.getState().config.setCode).toBe("ISD");
  });

  it("caps the pack list at the kind's engine-published booster count", async () => {
    const user = userEvent.setup();
    await openHostSetup(user);

    for (let i = 0; i < 3; i += 1) {
      await user.click(screen.getByRole("button", { name: /Add a pack of Innistrad/ }));
    }

    // The engine refuses a sequence longer than the event opens, so a fourth
    // pack must be unreachable rather than built and then rejected.
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /Add a pack of Innistrad/ })).toBeDisabled(),
    );
    await user.click(screen.getByRole("button", { name: "Create Pod" }));

    await waitFor(() => expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce());
    expect(hostedPoolInput().data.sequence).toEqual(["ISD", "ISD", "ISD"]);
  });

  it("creates a Chaos pod from candidate sets without serializing assignments", async () => {
    const user = userEvent.setup();
    await openHostSetup(user);

    await user.click(screen.getByRole("radio", { name: "Chaos Draft" }));
    await user.click(screen.getByRole("button", { name: /Add Innistrad as a candidate/ }));
    await user.click(screen.getByRole("button", { name: /Add Dark Ascension as a candidate/ }));
    await user.click(screen.getByRole("button", { name: "Create Pod" }));

    await waitFor(() => expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce());
    const poolInput = hostedChaosPoolInput();
    expect(poolInput.type).toBe("Chaos");
    expect(poolInput.data.candidate_codes).toEqual(["ISD", "DKA"]);
    expect(poolInput.data.pools).toEqual([
      { code: "ISD", name: "Innistrad" },
      { code: "DKA", name: "Dark Ascension" },
    ]);
    expect(poolInput.data).not.toHaveProperty("assignments");
  });

  /**
   * AN ABSENT CONTRACT IS NOT PERMISSION EITHER.
   *
   * `allowed_set_layouts` is `null` on the page until a procedure has been
   * published for the current selection. The radio used to render through that
   * window (`?? true`), which offers a control the engine may refuse -- the
   * guessing the published list exists to remove. The paired positive is the
   * suite's own "creates a Chaos pod" row, which clicks this exact radio under
   * a procedure that DOES publish Chaos, so its absence here is the missing
   * contract and not a renamed label or a form that failed to render.
   */
  it("offers no chaos mode until the engine has published a layout contract", async () => {
    // A procedure payload with NO `allowed_set_layouts` at all -- an engine that
    // predates the capability, or any degraded response. The field arrives as
    // `undefined`, which is why the store's guard is nullish rather than
    // `=== null`: `undefined.includes` would be a TypeError, not a refusal.
    const { allowed_set_layouts: _omitted, ...withoutContract } = draftProcedureFixture();
    mocks.draftProcedure.mockResolvedValue(withoutContract as never);
    const user = userEvent.setup();
    await openHostSetup(user);

    expect(screen.queryByRole("radio", { name: "Chaos Draft" })).toBeNull();
  });

  it("offers no chaos mode for a kind whose boosters share one stack", async () => {
    // A shared stack opens every booster unlooked-at and shuffles them
    // together before the first decision, so no seat holds the packs generated
    // for it: the per-(seat, round) assignment a Chaos pod exists to express
    // describes nothing the players can observe, and only hides which sets the
    // pool is made of. `DraftProcedure::validate_source` refuses the pair, so
    // the setup form must not offer it.
    //
    // The row directly above CLICKS this radio under the pick-and-pass
    // procedure this suite's `beforeEach` publishes. That is the paired
    // positive: the control exists, is reachable, and is labelled exactly this
    // — so its absence here is the distribution's doing, not a renamed label
    // or a form that failed to render.
    mocks.draftProcedure.mockResolvedValue(draftProcedureFixture({
      pod_size: 2,
      human_seats: 2,
      min_pod_size: 2,
      max_pod_size: 4,
      allowed_pod_sizes: [2, 3, 4],
      packs_per_player: 3,
      cards_per_pick: 1,
      distribution: { SharedStackPiles: { pile_count: 3 } },
      min_deck_size: 40,
      match_config: { match_type: "Bo1" },
    }));
    const user = userEvent.setup();
    await openHostSetup(user);

    expect(screen.queryByRole("radio", { name: "Chaos Draft" })).toBeNull();
    expect(screen.queryByRole("radio", { name: "Specific lineup" })).toBeNull();

    // And the pod the host can still create is the named sequence, which is
    // how a Winston pool says what it is made of.
    await user.click(screen.getByRole("button", { name: /Add a pack of Innistrad/ }));
    await user.click(screen.getByRole("button", { name: /Add a pack of Dark Ascension/ }));
    await user.click(screen.getByRole("button", { name: "Create Pod" }));

    await waitFor(() => expect(mocks.multiplayerState.hostDraft).toHaveBeenCalledOnce());
    const poolInput = hostedPoolInput();
    expect(poolInput.type).toBe("Set");
    expect(poolInput.data.sequence).toEqual(["ISD", "DKA"]);
    const [config] = mocks.multiplayerState.hostDraft.mock.calls[0] as unknown as [Record<string, unknown>];
    expect(config).not.toHaveProperty("listing");
  });
});
