import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { P2PDraftHost } from "../p2p-draft-host";
import type { DraftProcedure } from "../draft-adapter";
import { draftProcedureFixture } from "./draftProcedureFixture";

/**
 * THE PUBLIC DRAFT CODE MUST NOT ENCODE THE PRIVATE SEED.
 *
 * The code is public in the strongest sense this system has: it is the backup
 * row's key in the URL (`/p2p-draft-backup/<code>`) and it travels inside the
 * backup body, so anyone who can reach the row has it. The host seed is the
 * opposite — it orders the shared stack, and `main_stack`'s order "is the
 * `rng_seed`'s secret" (`draft_core::types`), which is exactly why
 * `redactDraftSessionObject` zeroes `config.rng_seed` out of that same payload.
 *
 * The code used to be built as `draft-${seed.toString(16)}`. That made the
 * redaction ornamental: `parseInt(code.slice(6), 16)` recovered the seed from
 * the row's own key, and the seed regenerates every pile in order. Zeroing a
 * field while publishing a reversible encoding of it protects nothing.
 *
 * Paired with `p2pDraftHostPersistence.test.ts`, which pins the body half
 * (`config.rng_seed` zeroed, IndexedDB copy untouched). This file pins the key
 * half. Neither is sufficient alone.
 */
describe("P2PDraftHost public draft code", () => {
  const originalFetch = globalThis.fetch;
  let draws: number[];
  let drawIndex: number;

  beforeEach(() => {
    globalThis.fetch = vi.fn(async () => new Response("{}", { status: 200 })) as typeof fetch;
    // Distinct, ordered draws so both values are known. Under the defect the
    // two collapse to ONE draw and the code becomes the seed in hex.
    draws = [0xaaaaaaaa, 0xbbbbbbbb];
    drawIndex = 0;
    vi.spyOn(globalThis.crypto, "getRandomValues").mockImplementation(((array: Uint32Array) => {
      array[0] = draws[Math.min(drawIndex, draws.length - 1)]!;
      drawIndex += 1;
      return array;
    }) as typeof crypto.getRandomValues);
  });

  afterEach(() => {
    globalThis.fetch = originalFetch;
    vi.restoreAllMocks();
  });

  async function startAndCapture(): Promise<{ seed: number; draftCode: string }> {
    const procedure: DraftProcedure = draftProcedureFixture({
      pod_size: 2,
      human_seats: 2,
      distribution: { SharedStackPiles: { pile_count: 3 } },
      allowed_pod_sizes: [2, 3, 4],
    });
    const host = new P2PDraftHost(
      { id: "host" } as never,
      () => () => {},
      { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
      "Winston",
      2,
      "Host",
      "Swiss",
      "Competitive",
    );
    const createMultiplayerDraft = vi.fn(
      async (
        _pool: unknown,
        _seats: unknown,
        _kind: unknown,
        _seed: number,
        _draftCode: string,
      ) => {},
    );
    (host as unknown as { adapter: unknown }).adapter = {
      draftProcedure: vi.fn(async () => procedure),
      createMultiplayerDraft,
      getViewForSeat: vi.fn(async () => ({ status: "Lobby" })),
      loadCardDatabase: vi.fn(async () => 0),
    };

    await host.initialize();
    await host.startDraft(true);

    expect(createMultiplayerDraft).toHaveBeenCalledOnce();
    const call = createMultiplayerDraft.mock.calls[0]!;
    return { seed: call[3], draftCode: call[4] };
  }

  it("draws the public code from randomness independent of the reducer seed", async () => {
    const { seed, draftCode } = await startAndCapture();

    // Reach guards: the stub really drove BOTH draws, in order. Without these a
    // defect that stopped calling `crypto.getRandomValues` at all could pass the
    // inequality below on two coincidentally-different constants.
    expect(seed).toBe(0xaaaaaaaa);
    expect(draftCode).toMatch(/^draft-[0-9a-f]{8}$/);
    expect(drawIndex).toBe(2);

    // THE CLAIM. Restore `draft-${seed.toString(16).padStart(8, "0")}` and both
    // of these red: the code becomes "draft-aaaaaaaa", which is the seed.
    expect(draftCode).not.toBe(`draft-${seed.toString(16).padStart(8, "0")}`);
    expect(parseInt(draftCode.slice("draft-".length), 16)).not.toBe(seed);
  });

  it("does not leak the seed through the code under real randomness", async () => {
    // The test above pins the derivation with a stub; this one pins the property
    // the stub is standing in for, using the platform's own entropy. A reverted
    // implementation reds here EVERY run, not probabilistically, because the
    // equality it restores is exact.
    vi.restoreAllMocks();
    for (let run = 0; run < 8; run += 1) {
      const { seed, draftCode } = await startAndCapture();
      expect(parseInt(draftCode.slice("draft-".length), 16)).not.toBe(seed);
    }
  });
});
