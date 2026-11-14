import { describe, expect, it } from "vitest";

import { CARD_BACK_URL, MANA_SYMBOL_SHARDS, manaSymbolSourceUrl } from "../../../scryfall.ts";
import { manaSymbolCandidate } from "../../candidateKeys.ts";
import { catalogRoot, packId } from "../../types.ts";
import { countScryfallAssets, forEachScryfallAsset, type ScryfallAssetDescriptor, type ScryfallBulkSource } from "../scryfallBulk.ts";

// The `core` arm of both functions is planned entirely from local constants —
// it never opens the bulk stream — so this source is only there to satisfy the
// signature, and a fetch that reached the network would fail the run.
const UNUSED_SOURCE: ScryfallBulkSource = Object.freeze({
  root: catalogRoot("a".repeat(64)),
  downloadUrl: "https://data.scryfall.io/all-cards.jsonl.gz",
  updatedAt: "2026-08-01T00:00:00.000Z",
  compressedBytes: 1,
});

const unreachableFetch = (async () => {
  throw new Error("the core selector must not fetch");
}) as unknown as typeof fetch;

async function coreDescriptors(): Promise<ScryfallAssetDescriptor[]> {
  const descriptors: ScryfallAssetDescriptor[] = [];
  await forEachScryfallAsset(
    UNUSED_SOURCE,
    { kind: "core" },
    new AbortController().signal,
    (descriptor) => { descriptors.push(descriptor); },
    unreachableFetch,
  );
  return descriptors;
}

describe("the core visual pack", () => {
  it("installs the card back and every finite mana symbol", async () => {
    const descriptors = await coreDescriptors();
    const sources = descriptors.map((descriptor) => descriptor.sourceUrl);

    expect(sources).toContain(CARD_BACK_URL);
    expect(sources).toEqual(expect.arrayContaining(MANA_SYMBOL_SHARDS.map(manaSymbolSourceUrl)));
    expect(descriptors).toHaveLength(1 + MANA_SYMBOL_SHARDS.length);
    expect(descriptors.every((descriptor) => descriptor.packId === packId("core"))).toBe(true);
  });

  /** The contract that actually makes an installed symbol reachable: the key
   *  the installer writes must be the key `useManaSymbolImage` looks up. A
   *  mismatch caches every pip and renders none of them. */
  it("keys each mana symbol under the candidate the renderer resolves", async () => {
    const byCandidate = new Map(
      (await coreDescriptors()).flatMap((descriptor) =>
        descriptor.candidateKeys.map((key) => [key, descriptor] as const)),
    );

    for (const shard of MANA_SYMBOL_SHARDS) {
      const descriptor = byCandidate.get(manaSymbolCandidate(shard));
      expect(descriptor, `no core descriptor for {${shard}}`).toBeDefined();
      expect(descriptor?.sourceUrl).toBe(manaSymbolSourceUrl(shard));
      // Scryfall serves these as SVG, and `fetchImage` rejects any response
      // whose Content-Type disagrees with the descriptor's declared media.
      expect(descriptor?.media).toBe("image/svg+xml");
    }
  });

  /** `manaSymbolCode` strips the hybrid/Phyrexian slashes, so two distinct
   *  shards could collide into one asset key and silently install 75 of 76. */
  it("gives every asset a distinct key", async () => {
    const keys = (await coreDescriptors()).map((descriptor) => descriptor.assetKey);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it("counts exactly what it installs", async () => {
    const counted = await countScryfallAssets(
      UNUSED_SOURCE,
      { kind: "core" },
      new AbortController().signal,
      undefined,
      unreachableFetch,
    );
    expect(counted).toBe((await coreDescriptors()).length);
  });
});
