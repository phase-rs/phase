import { describe, expect, it, vi } from "vitest";

import type { VisualPackBackend } from "../backend.ts";
import {
  cardBackCandidate,
  cardCandidateGroups,
  decodeCandidateKey,
  encodeCandidateKey,
  manaSymbolCandidate,
  semanticCardCandidateGroups,
  setIconCandidate,
  tokenCandidateGroups,
} from "../candidateKeys.ts";
import { VisualPackRepository } from "../repository.ts";
import {
  assetKey,
  candidateKey,
  catalogRoot,
  installedRevision,
  packId,
} from "../types.ts";
import type { ResolutionKey, ResolutionResponse, RevisionEvent } from "../types.ts";

const ROOT = catalogRoot("a".repeat(64));
const PRINTING = "22222222-2222-4222-8222-222222222222";
const ORACLE = "11111111-1111-4111-8111-111111111111";
const TOKEN = "44444444-4444-4444-8444-444444444444";

function rawCandidateKey(kind: string, tuple: readonly unknown[]): string {
  const bytes = new TextEncoder().encode(`${JSON.stringify([kind, tuple])}\n`);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  const payload = btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  return `candidate:v1:${kind}:${payload}`;
}

const CORPUS_KEYS = {
  oracle: "candidate:v1:oracle:WyJvcmFjbGUiLFsiMTExMTExMTEtMTExMS00MTExLTgxMTEtMTExMTExMTExMTExIiwwLCJmdWxsX2NhcmQiLCJub3JtYWwiXV0K",
  oracle_alias: "candidate:v1:oracle_alias:WyJvcmFjbGVfYWxpYXMiLFsib3JhY2xlIGFsaWFzIiwwLCJmdWxsX2NhcmQiLCJub3JtYWwiXV0K",
  card_back: "candidate:v1:card_back:WyJjYXJkX2JhY2siLFtdXQo",
  english_alias: "candidate:v1:english_alias:WyJlbmdsaXNoX2FsaWFzIixbImVuZ2xpc2ggYWxpYXMiLDAsImZ1bGxfY2FyZCIsIm5vcm1hbCJdXQo",
  english_printing: "candidate:v1:english_printing:WyJlbmdsaXNoX3ByaW50aW5nIixbIjIyMjIyMjIyLTIyMjItNDIyMi04MjIyLTIyMjIyMjIyMjIyMiIsMCwiZnVsbF9jYXJkIiwibm9ybWFsIl1dCg",
  localized_alias: "candidate:v1:localized_alias:WyJsb2NhbGl6ZWRfYWxpYXMiLFsiZGUiLCJsb2thbGVyIGFsaWFzIiwwLCJmdWxsX2NhcmQiLCJub3JtYWwiXV0K",
  localized_printing: "candidate:v1:localized_printing:WyJsb2NhbGl6ZWRfcHJpbnRpbmciLFsiZGUiLCIyMjIyMjIyMi0yMjIyLTQyMjItODIyMi0yMjIyMjIyMjIyMjIiLDAsImZ1bGxfY2FyZCIsIm5vcm1hbCJdXQo",
  mana_symbol: "candidate:v1:mana_symbol:WyJtYW5hX3N5bWJvbCIsWyJ7V30iXV0K",
  set_icon: "candidate:v1:set_icon:WyJzZXRfaWNvbiIsWyJhYmMiXV0K",
  token_alias: "candidate:v1:token_alias:WyJ0b2tlbl9hbGlhcyIsWyJwcmVzZXQ6Zml4dHVyZSIsMCwiZnVsbF9jYXJkIiwibm9ybWFsIl1dCg",
  token_reference: "candidate:v1:token_reference:WyJ0b2tlbl9yZWZlcmVuY2UiLFsicHJpbnRpbmc6NDQ0NDQ0NDQtNDQ0NC00NDQ0LTg0NDQtNDQ0NDQ0NDQ0NDQ0OmZyb250IiwwLCJmdWxsX2NhcmQiLCJub3JtYWwiXV0K",
} as const;

function fakeBackend(
  resolve: (keys: ResolutionKey[]) => Promise<ResolutionResponse>,
  revision?: RevisionEvent,
): VisualPackBackend {
  const unavailable = vi.fn(async (): Promise<never> => { throw new Error("unused"); });
  return {
    catalogStatus: unavailable,
    curatedSelector: unavailable,
    curatedDrift: unavailable,
    deckLibrarySelector: unavailable,
    deckLibraryDrift: unavailable,
    reconcileDeckLibrary: unavailable,
    refreshCatalog: unavailable,
    catalogSummary: unavailable,
    estimateInstall: unavailable,
    start: unavailable,
    cancel: unavailable,
    operationStatus: unavailable,
    remove: unavailable,
    verify: unavailable,
    resolve,
    subscribeProgress: vi.fn(async () => () => {}),
    subscribeRevision: vi.fn(async (listener) => {
      if (revision) listener(revision);
      return () => {};
    }),
  };
}

function resolved(
  keys: ResolutionKey[],
  matches: Map<string, Array<{ asset: string; pack?: string; url: string }>>,
  revision = "1",
): ResolutionResponse {
  return {
    revision: installedRevision(revision),
    entries: keys.map((key, ordinal) => ({
      ordinal,
      key,
      matches: (matches.get(key.key) ?? []).map((match) => ({
        packId: packId(match.pack ?? "core"),
        assetKey: assetKey(`asset:v1:canonical_card:${match.asset}`),
        catalogRoot: ROOT,
        url: match.url,
        media: "image/jpeg" as const,
      })),
    })),
  };
}

describe("CandidateKey parity", () => {
  it("matches the accepted seven-class corpus for all eleven kinds", () => {
    const cards = cardCandidateGroups({
      language: "de",
      englishPrintingId: PRINTING,
      oracleId: ORACLE,
      localizedAliases: ["lokaler alias"],
      englishAliases: ["english alias"],
      oracleAliases: ["oracle alias"],
      faceIndex: 0,
      variant: "full_card",
      rung: "normal",
    }).flatMap(({ keys }) => keys);
    const tokens = tokenCandidateGroups({
      scryfallId: TOKEN,
      faceName: "front",
      presetId: "fixture",
      faceIndex: 0,
      rung: "normal",
    }).flatMap(({ keys }) => keys);
    const actual = [...cards, ...tokens, cardBackCandidate(), manaSymbolCandidate("{W}"), setIconCandidate("abc")];
    expect(new Set(actual)).toEqual(new Set(Object.values(CORPUS_KEYS)));
    for (const key of Object.values(CORPUS_KEYS)) {
      expect(candidateKey(key)).toBe(key);
      expect(decodeCandidateKey(key)[0]).toBe(key.split(":")[2]);
    }
  });

  it("normalizes large and rejects malformed or semantically illegal keys", () => {
    const large = cardCandidateGroups({
      oracleId: ORACLE,
      faceIndex: 0,
      variant: "full_card",
      rung: "large",
    });
    expect(large[0].keys[0]).toBe(encodeCandidateKey("oracle", [ORACLE, 0, "full_card", "normal"]));
    expect(() => encodeCandidateKey("oracle", ["ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDEF", 0, "full_card", "normal"])).toThrow();
    expect(() => encodeCandidateKey("oracle_alias", ["e\u0301", 0, "full_card", "normal"])).toThrow();
    expect(() => encodeCandidateKey("token_reference", [`printing:${TOKEN}:front`, -1, "full_card", "normal"])).toThrow();
    expect(() => encodeCandidateKey("token_alias", ["preset:x", 0, "art_crop", "art_crop"])).toThrow();
    expect(() => decodeCandidateKey(`${CORPUS_KEYS.oracle}=`)).toThrow();
    expect(() => decodeCandidateKey("candidate:v1:unknown:W1widW5rbm93blwiLFtdXQo")).toThrow();
    expect(() => candidateKey("candidate:v1:oracle:QQ")).toThrow();
  });

  it.each([
    ["invalid UUID", "oracle", ["not-a-uuid", 0, "full_card", "normal"]],
    ["missing axis", "oracle", [ORACLE, 0, "full_card"]],
    ["extra axis", "oracle", [ORACLE, 0, "full_card", "normal", "extra"]],
    ["negative face", "oracle", [ORACLE, -1, "full_card", "normal"]],
    ["invalid variant", "oracle", [ORACLE, 0, "thumbnail", "normal"]],
    ["noncanonical rung", "oracle", [ORACLE, 0, "full_card", "large"]],
    ["non-NFC alias", "oracle_alias", ["e\u0301", 0, "full_card", "normal"]],
    ["token crop", "token_alias", ["preset:fixture", 0, "art_crop", "art_crop"]],
  ])("rejects canonical encoding with %s at the public boundary", (_label, kind, tuple) => {
    expect(() => candidateKey(rawCandidateKey(kind as string, tuple as readonly unknown[]))).toThrow();
  });
});

describe("VisualPackRepository", () => {
  it("owns tier order, deterministic match order, deduplication, and same-group rungs", async () => {
    const localized = candidateKey(CORPUS_KEYS.localized_printing);
    const localizedAlias = candidateKey(CORPUS_KEYS.localized_alias);
    const english = candidateKey(CORPUS_KEYS.english_printing);
    const localizedSmall = encodeCandidateKey("localized_printing", ["de", PRINTING, 0, "full_card", "small"]);
    const localizedNormal = encodeCandidateKey("localized_printing", ["de", PRINTING, 0, "full_card", "normal"]);
    const englishNormal = encodeCandidateKey("english_printing", [PRINTING, 0, "full_card", "normal"]);
    const backend = fakeBackend(async (keys) => resolved(keys, new Map([
      [localized, [
        { asset: "Qg", pack: "printing:zzz", url: "installed-z" },
        { asset: "QQ", pack: "core", url: "installed-a" },
        { asset: "QQ", pack: "core", url: "duplicate-a" },
      ]],
      [localizedAlias, [{ asset: "Rw", url: "alias-must-not-be-promoted" }]],
      [english, [{ asset: "Qw", url: "english" }]],
      [localizedSmall, [{ asset: "RA", url: "local-small" }]],
      [englishNormal, [{ asset: "Rg", url: "english" }]],
    ])));
    const result = await new VisualPackRepository(async () => backend).resolve({
      groups: [
        { requested: [localized, localizedAlias], small: [localizedSmall], normal: [localizedNormal] },
        { requested: [english], normal: [englishNormal] },
      ],
      rung: "normal",
      allowRemote: true,
      remote: { src: "remote", rungs: { small: "remote-small", normal: "remote-normal" } },
    });
    expect(result.sources.map((source) => source.src)).toEqual(["installed-a", "installed-z", "remote", null]);
    expect(result.sources[0]).toEqual(expect.objectContaining({ rungs: { small: "local-small", normal: "installed-a" } }));
    expect(JSON.stringify(result.sources)).not.toContain("english-normal");
    expect(JSON.stringify(result.sources)).not.toContain("alias-must-not-be-promoted");
  });

  it("keeps art crops strict and resolves every fixed candidate kind", async () => {
    const fixed = [cardBackCandidate(), manaSymbolCandidate("{W}"), setIconCandidate("abc")];
    for (const key of fixed) {
      const matches = new Map([[key, [{ asset: "QQ", url: `installed-${key.split(":")[2]}` }]]]);
      const backend = fakeBackend(async (keys) => resolved(keys, matches));
      const result = await new VisualPackRepository(async () => backend).resolve({ groups: [{ requested: [key] }], rung: "art_crop", allowRemote: false });
      expect(result.sources[0]).toMatchObject({ kind: "installed", rungs: undefined });
    }
  });

  it("retries one stale revision then falls back online without regressing", async () => {
    const key = candidateKey(CORPUS_KEYS.oracle);
    const matches = new Map([[key, [{ asset: "QQ", url: "stale" }]]]);
    const resolve = vi.fn(async (keys: ResolutionKey[]) => resolved(keys, matches, "1"));
    const backend = fakeBackend(resolve, {
      cause: "remove",
      operationId: null,
      catalogRoot: null,
      revision: installedRevision("2"),
    });
    const repository = new VisualPackRepository(async () => backend);
    const result = await repository.resolve({ groups: [{ requested: [key] }], rung: "normal", allowRemote: true, remote: { src: "remote" } });
    expect(resolve).toHaveBeenCalledTimes(2);
    expect(result.revision).toBe("2");
    expect(result.sources.map((source) => source.src)).toEqual(["remote", null]);
  });

  it("synchronously latches one revision subscription across concurrent listeners", async () => {
    let releaseBackend: ((backend: VisualPackBackend) => void) | undefined;
    const backend = fakeBackend(async (keys) => resolved(keys, new Map()));
    const repository = new VisualPackRepository(() => new Promise((resolve) => {
      releaseBackend = resolve;
    }));

    const unsubscribeFirst = repository.subscribe(vi.fn());
    const unsubscribeSecond = repository.subscribe(vi.fn());
    releaseBackend?.(backend);
    await vi.waitFor(() => expect(backend.subscribeRevision).toHaveBeenCalledTimes(1));
    unsubscribeFirst();
    unsubscribeSecond();
  });

  it("publishes an accepted retry revision monotonically", async () => {
    const key = candidateKey(CORPUS_KEYS.oracle);
    const matches = new Map([[key, [{ asset: "QQ", url: "installed" }]]]);
    const resolve = vi.fn()
      .mockImplementationOnce(async (keys: ResolutionKey[]) => resolved(keys, matches, "1"))
      .mockImplementationOnce(async (keys: ResolutionKey[]) => resolved(keys, matches, "3"));
    const backend = fakeBackend(resolve, {
      cause: "remove",
      operationId: null,
      catalogRoot: null,
      revision: installedRevision("2"),
    });
    const repository = new VisualPackRepository(async () => backend);
    const listener = vi.fn();
    repository.subscribe(listener);

    const result = await repository.resolve({ groups: [{ requested: [key] }], rung: "normal", allowRemote: false });

    expect(result.revision).toBe("3");
    expect(repository.currentRevision()).toBe("3");
    expect(listener).toHaveBeenCalledTimes(2);
  });

  it("omits ambiguous companion rungs for a shared alias within one pack", async () => {
    const alias = candidateKey(CORPUS_KEYS.oracle_alias);
    const small = encodeCandidateKey("oracle_alias", ["oracle alias", 0, "full_card", "small"]);
    const normal = encodeCandidateKey("oracle_alias", ["oracle alias", 0, "full_card", "normal"]);
    const backend = fakeBackend(async (keys) => resolved(keys, new Map([
      [alias, [{ asset: "QQ", url: "requested-a" }, { asset: "Qg", url: "requested-b" }]],
      [small, [{ asset: "Qw", url: "small-a" }, { asset: "RA", url: "small-b" }]],
    ])));

    const result = await new VisualPackRepository(async () => backend).resolve({
      groups: [{ requested: [alias], small: [small], normal: [normal] }],
      rung: "normal",
      allowRemote: false,
    });

    expect(result.sources.slice(0, 2)).toEqual([
      expect.objectContaining({ kind: "installed", src: "requested-a", rungs: { normal: "requested-a" } }),
      expect.objectContaining({ kind: "installed", src: "requested-b", rungs: { normal: "requested-b" } }),
    ]);
  });

  it("preserves online behavior when the backend is absent", async () => {
    const result = await new VisualPackRepository(async () => null).resolve({
      groups: [{ requested: [candidateKey(CORPUS_KEYS.oracle)] }],
      rung: "normal",
      allowRemote: true,
      remote: { src: "remote" },
    });
    expect(result.sources).toEqual([{ kind: "remote", src: "remote", rungs: undefined }, { kind: "fallback", src: null }]);
  });

  it.each([true, false])("applies remote policy on every local-miss return path (%s)", async (allowRemote) => {
    const key = candidateKey(CORPUS_KEYS.oracle);
    const expected = allowRemote
      ? [{ kind: "remote", src: "remote", rungs: undefined }, { kind: "fallback", src: null }]
      : [{ kind: "fallback", src: null }];
    const request = { groups: [{ requested: [key] }], rung: "normal" as const, allowRemote, remote: { src: "remote" } };
    const backend = fakeBackend(async (keys) => resolved(keys, new Map()));
    const errored = fakeBackend(async () => { throw new Error("unavailable"); });
    const staleResolve = vi.fn(async (keys: ResolutionKey[]) => resolved(keys, new Map([
      [key, [{ asset: "QQ", url: "stale-installed" }]],
    ]), "1"));
    const stale = fakeBackend(staleResolve, {
      cause: "remove",
      operationId: null,
      catalogRoot: null,
      revision: installedRevision("2"),
    });

    await expect(new VisualPackRepository(async () => null).resolve(request)).resolves.toEqual(
      expect.objectContaining({ sources: expected }),
    );
    await expect(new VisualPackRepository(async () => backend).resolve({ ...request, groups: [] })).resolves.toEqual(
      expect.objectContaining({ sources: expected }),
    );
    await expect(new VisualPackRepository(async () => errored).resolve(request)).resolves.toEqual(
      expect.objectContaining({ sources: expected }),
    );
    await expect(new VisualPackRepository(async () => backend).resolve(request)).resolves.toEqual(
      expect.objectContaining({ sources: expected }),
    );
    const staleResult = await new VisualPackRepository(async () => stale).resolve(request);
    expect(staleResolve).toHaveBeenCalledTimes(2);
    expect(staleResult.sources).toEqual(expected);
  });

  it("applies pack and unambiguous constraints before rung matching", async () => {
    const [requested] = semanticCardCandidateGroups({
      oracleId: ORACLE,
      cardName: "Card",
      faceName: "Card",
      variant: "full_card",
      rung: "normal",
    });
    const [small] = semanticCardCandidateGroups({
      oracleId: ORACLE,
      cardName: "Card",
      faceName: "Card",
      variant: "full_card",
      rung: "small",
    });
    const [normal] = semanticCardCandidateGroups({
      oracleId: ORACLE,
      cardName: "Card",
      faceName: "Card",
      variant: "full_card",
      rung: "normal",
    });
    const backend = fakeBackend(async (keys) => resolved(keys, new Map([
      [requested.keys[0], [
        { asset: "QQ", pack: "core", url: "core-requested" },
        { asset: "Qg", pack: "deck_library", url: "deck-requested" },
      ]],
      [small.keys[0], [
        { asset: "Qw", pack: "core", url: "core-small" },
        { asset: "RA", pack: "deck_library", url: "deck-small" },
      ]],
    ])));
    const constrained = await new VisualPackRepository(async () => backend).resolve({
      groups: [{ requested: requested.keys, small: small.keys, normal: normal.keys, packId: packId("deck_library") }],
      rung: "normal",
      allowRemote: false,
    });
    expect(constrained.sources).toEqual([
      expect.objectContaining({ kind: "installed", src: "deck-requested", rungs: { small: "deck-small", normal: "deck-requested" } }),
      { kind: "fallback", src: null },
    ]);

    const unique = encodeCandidateKey("oracle", [ORACLE, 0, "full_card", "normal"]);
    const unambiguousBackend = fakeBackend(async (keys) => resolved(keys, new Map([
      [unique, [
        { asset: "QQ", pack: "core", url: "core-copy" },
        { asset: "QQ", pack: "printing:abc", url: "printing-copy" },
      ]],
    ])));
    const oneAsset = await new VisualPackRepository(async () => unambiguousBackend).resolve({
      groups: [{ requested: [unique], requireUnambiguousAsset: true }],
      rung: "normal",
      allowRemote: false,
    });
    expect(oneAsset.sources).toEqual([
      expect.objectContaining({ kind: "installed", src: "core-copy" }),
      { kind: "fallback", src: null },
    ]);

    const ambiguousBackend = fakeBackend(async (keys) => resolved(keys, new Map([
      [unique, [
        { asset: "QQ", pack: "core", url: "first" },
        { asset: "Qg", pack: "printing:abc", url: "second" },
      ]],
    ])));
    const multipleAssets = await new VisualPackRepository(async () => ambiguousBackend).resolve({
      groups: [{ requested: [unique], requireUnambiguousAsset: true }],
      rung: "normal",
      allowRemote: false,
    });
    expect(multipleAssets.sources).toEqual([{ kind: "fallback", src: null }]);
  });

  it("treats disallowed requests and ambiguous companions as constrained misses", async () => {
    const requested = encodeCandidateKey("oracle", [ORACLE, 0, "full_card", "normal"]);
    const small = encodeCandidateKey("oracle_alias", ["small", 0, "full_card", "small"]);
    const normal = encodeCandidateKey("oracle_alias", ["normal", 0, "full_card", "normal"]);
    const backend = fakeBackend(async (keys) => resolved(keys, new Map([
      [requested, [{ asset: "QQ", pack: "core", url: "core-only" }]],
      [small, [
        { asset: "Qg", pack: "deck_library", url: "small-a" },
        { asset: "Qw", pack: "deck_library", url: "small-b" },
      ]],
      [normal, [
        { asset: "RA", pack: "deck_library", url: "normal-a" },
        { asset: "RQ", pack: "deck_library", url: "normal-b" },
      ]],
    ])));
    const repository = new VisualPackRepository(async () => backend);

    await expect(repository.resolve({
      groups: [{ requested: [requested], packId: packId("deck_library") }],
      rung: "normal",
      allowRemote: false,
    })).resolves.toEqual(expect.objectContaining({
      sources: [{ kind: "fallback", src: null }],
    }));

    const requestedDeck = encodeCandidateKey("oracle_alias", ["requested", 0, "full_card", "normal"]);
    const companionBackend = fakeBackend(async (keys) => resolved(keys, new Map([
      [requestedDeck, [{ asset: "QQ", pack: "deck_library", url: "requested" }]],
      [small, [
        { asset: "Qg", pack: "deck_library", url: "small-a" },
        { asset: "Qw", pack: "deck_library", url: "small-b" },
      ]],
      [normal, [
        { asset: "RA", pack: "deck_library", url: "normal-a" },
        { asset: "RQ", pack: "deck_library", url: "normal-b" },
      ]],
    ])));
    const companions = await new VisualPackRepository(async () => companionBackend).resolve({
      groups: [{
        requested: [requestedDeck],
        small: [small],
        normal: [normal],
        requireUnambiguousAsset: true,
      }],
      rung: "normal",
      allowRemote: false,
    });
    expect(companions.sources).toEqual([
      expect.objectContaining({ kind: "installed", src: "requested", rungs: undefined }),
      { kind: "fallback", src: null },
    ]);
  });

  it("leaves exact and token-reference identity unrestricted across packs", async () => {
    const [exact] = cardCandidateGroups({
      englishPrintingId: PRINTING,
      faceIndex: 0,
      variant: "full_card",
      rung: "normal",
    });
    const [token] = tokenCandidateGroups({
      scryfallId: TOKEN,
      faceName: "Token",
      faceIndex: 0,
      rung: "normal",
    });
    const backend = fakeBackend(async (keys) => resolved(keys, new Map([
      [exact.keys[0], [
        { asset: "QQ", pack: "core", url: "exact-core" },
        { asset: "Qg", pack: "printing:abc", url: "exact-printing" },
      ]],
      [token.keys[0], [
        { asset: "Qw", pack: "core", url: "token-core" },
        { asset: "RA", pack: "printing:abc", url: "token-printing" },
      ]],
    ])));
    const repository = new VisualPackRepository(async () => backend);
    for (const group of [exact, token]) {
      const result = await repository.resolve({ groups: [{ requested: group.keys }], rung: "normal", allowRemote: false });
      expect(result.sources.map((source) => source.src)).toEqual(
        group === exact
          ? ["exact-core", "exact-printing", null]
          : ["token-core", "token-printing", null],
      );
    }
  });
});
