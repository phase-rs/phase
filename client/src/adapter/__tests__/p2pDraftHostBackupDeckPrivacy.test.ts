import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../draft-adapter", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../draft-adapter")>()),
  DraftAdapter: vi.fn().mockImplementation(function () {
    return {};
  }),
}));

vi.mock("../../services/draftPersistence", () => ({
  saveDraftHostSession: vi.fn().mockResolvedValue(undefined),
  clearDraftHostSession: vi.fn(),
}));

import { P2PDraftHost } from "../p2p-draft-host";
import { saveDraftHostSession } from "../../services/draftPersistence";
import type { PersistedDraftHostSession } from "../../services/draftPersistence";
import { draftIntergameDigest } from "../../services/intergameCommandLedger";

/**
 * A submitted deck is private to its seat until its match launches it. The
 * backup row is readable by any pod guest who can derive the host peer id, so
 * NO retained copy of another seat's deck may be in the public upload — while
 * the host's own IndexedDB snapshot keeps every copy for recovery.
 *
 * Drives the real submission path (`handleGuestMessage` `draft_submit_deck`,
 * which writes the deck-submission receipt), then the real persist queue until
 * the periodic upload gate fires. The adapter, match launches, Bo3 state,
 * held commands and workspace snapshot are SYNTHETIC fixtures (no printed
 * card's text is involved; the names are distinctive markers). The server half
 * of this contract is `p2p_backup_get_never_returns_submitted_decks` in
 * crates/phase-server/src/admin.rs.
 */
const SECRET = {
  submitted: "Secret Submitted Deck Card",
  sideboard: "Secret Sideboard Card",
  launch: "Secret Launch Card",
  opponent: "Secret Opponent Card",
  guestLaunch: "Secret Guest Launch Card",
  bo3: "Secret Bo3 Card",
  command: "Secret Command Card",
  workspace: "Secret Workspace Basic",
};

describe("P2PDraftHost public backup never carries submitted decks", () => {
  const BACKUP_URL = "https://backup.example";
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    fetchMock = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  it("submission -> periodic upload keeps every deck copy out of the POST body, in IndexedDB only", async () => {
    const host = new P2PDraftHost(
      { id: "host-peer-abc" } as never,
      () => () => {},
      { type: "Set", data: { set_pool_json: "{}" } } as never,
      "Premier",
      2,
      "Host",
      "Swiss",
      "Casual",
      undefined,
      "persist-deck-privacy",
      "RMABC",
      BACKUP_URL,
    );
    const priv = host as unknown as Record<string, unknown> & {
      handleGuestMessage: (seat: number, message: unknown) => Promise<void>;
      persistSessionStrict: () => Promise<void>;
      persistQueue: Promise<void>;
    };
    priv.draftStarted = true;
    priv.paused = false;
    priv.draftCode = "G37RUJ";
    priv.guestSessions = new Map([[1, { send: vi.fn() }]]);
    priv.adapter = {
      submitDeckForSeat: vi.fn(async () => ({ status: "Deckbuilding" })),
      getViewForSeat: vi.fn(async () => ({
        status: "Deckbuilding",
        seats: [{ has_submitted_deck: false, is_bot: false }],
      })),
      exportSession: vi.fn(async () => JSON.stringify({
        status: "Deckbuilding",
        submitted_decks: {
          1: { seat: 1, main_deck: [SECRET.submitted], commanders: [], printings: [] },
        },
      })),
    };
    const launchDeck = { main_deck: [SECRET.launch], sideboard: [SECRET.sideboard], commander: [] };
    priv.matchLaunches = new Map([
      ["m1", new Map<number, unknown>([
        [0, {
          type: "HumanHost", matchId: "m1",
          deckPayload: { player: launchDeck, opponent: { ...launchDeck, main_deck: [SECRET.opponent] }, ai_decks: [] },
        }],
        [1, { type: "HumanGuest", matchId: "m1", localDeck: { ...launchDeck, main_deck: [SECRET.guestLaunch] } }],
      ])],
    ]);
    priv.bo3State = new Map([
      ["m1", {
        seatA: 0, seatB: 1, submittedA: false, submittedB: false, loserSeat: null, gameNumber: 1,
        score: { p0_wins: 0, p1_wins: 0, draws: 0 },
        decks: [{ seat: 1, main: [{ name: SECRET.bo3, count: 4 }], sideboard: [] }],
      }],
    ]);
    // Production-derived digests (unsalted 32-bit checksums): a retained digest
    // would let a pod member test guessed partitions offline.
    const commandPayload = { type: "SubmitSideboard", main: [{ name: SECRET.command, count: 4 }], sideboard: [] };
    const commandLaunch = { deckPayload: { player: launchDeck, opponent: launchDeck, ai_decks: [] } };
    const payloadDigest = draftIntergameDigest(commandPayload);
    const launchDigest = draftIntergameDigest(commandLaunch);
    priv.intergameCommands = {
      snapshot: () => [{
        commandId: "c1", matchId: "m1", gameNumber: 2, seat: 1,
        payload: commandPayload, launchPayload: commandLaunch,
        launchDigest, payloadDigest, status: "Pending",
      }],
    };
    priv.launchDigests = new Map([["m1", new Map([[1, launchDigest]])]]);
    priv.perSeatWorkspaceSnapshots = new Map([
      [1, { schemaVersion: 1, placements: {}, virtualBasics: [{ name: SECRET.workspace }] }],
    ]);

    // Real submission path: writes the deck-submission receipt.
    await priv.handleGuestMessage(1, {
      type: "draft_submit_deck",
      submissionId: "submission-1",
      mainDeck: [SECRET.submitted, "Forest"],
      commanders: [],
    });
    expect(priv.deckSubmissionReceipts).toBeDefined();
    expect((priv.deckSubmissionReceipts as Map<string, unknown>).size).toBe(1);

    // Periodic gate: every fifth persisted snapshot uploads.
    for (let i = 0; i < 5; i += 1) await priv.persistSessionStrict();
    await priv.persistQueue;

    // Reach guard: an upload really happened.
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const uploaded = JSON.parse(fetchMock.mock.calls[0][1].body as string).snapshot_json as string;
    for (const marker of Object.values(SECRET)) expect(uploaded).not.toContain(marker);
    // The receipt fingerprint is the deck JSON itself; its key must be gone too.
    expect(uploaded).not.toContain("payloadFingerprint");
    // ... and so must every digest of a deck-bearing payload.
    expect(uploaded).not.toContain(payloadDigest);
    expect(uploaded).not.toContain(launchDigest);
    expect(uploaded).not.toContain("payloadDigest");
    expect(uploaded).not.toContain("launchDigest");

    // IndexedDB keeps the recovery data.
    const saved = vi.mocked(saveDraftHostSession).mock.calls[vi.mocked(saveDraftHostSession).mock.calls.length - 1]![1] as PersistedDraftHostSession;
    const local = JSON.stringify(saved);
    for (const marker of Object.values(SECRET)) expect(local).toContain(marker);
    expect(saved.deckSubmissionReceipts).toHaveLength(1);
    expect(local).toContain(payloadDigest);
  });
});
