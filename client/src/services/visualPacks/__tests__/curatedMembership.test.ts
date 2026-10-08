import { describe, expect, it } from "vitest";

import type { PrintingEntry } from "../../scryfall.ts";
import type { ArtChainEntry, CardArtOverride } from "../../../stores/preferencesStore.ts";
import { cardCandidateGroups, decodeCandidateKey, semanticCardCandidateGroups } from "../candidateKeys.ts";
import { planCuratedMembership } from "../curatedMembership.ts";
import type { CuratedCardEntry, CuratedMembershipInput } from "../curatedMembership.ts";
import { packId } from "../types.ts";
import type { ScryfallAssetDescriptor } from "../browser/descriptors.ts";

// `curated` is not an admissible PackId until the selector lands; the planner
// takes the pack from its caller, so any existing id exercises it.
const PACK = packId("complete");

// Oracle ids carry hex LETTERS on purpose: an all-digit UUID makes
// `.toUpperCase()` a no-op and silently voids every case-folding assertion.
const BOLT = "11111111-abcd-4111-8111-111111111111";
const GIANT = "22222222-abcd-4222-8222-222222222222";
const SOLO = "33333333-abcd-4333-8333-333333333333";
const SHORT = "55555555-abcd-4555-8555-555555555555";
const TOKEN_ORACLE = "44444444-abcd-4444-8444-444444444444";
const BLANK = "66666666-abcd-4666-8666-666666666666";

function url(token: string, size: string): string {
  return `https://cards.scryfall.io/${size}/front/a/b/${token}.jpg?1`;
}

function imageFace(token: string) {
  return { normal: url(token, "normal"), art_crop: url(token, "art_crop") };
}

function printing(overrides: Partial<PrintingEntry> & Pick<PrintingEntry, "id" | "set">): PrintingEntry {
  return {
    set_name: overrides.set.toUpperCase(),
    collector_number: "1",
    released_at: "2020-01-01",
    border_color: "black",
    frame_effects: [],
    full_art: false,
    faces: [imageFace(overrides.id)],
    ...overrides,
  };
}

const BOLT_NEW = printing({ id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", set: "m20", released_at: "2019-07-12" });
const BOLT_OLD = printing({ id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", set: "lea", released_at: "1993-08-05", collector_number: "161" });
// Two-faced, so the planner's pairing of entry-sourced face NAMES with
// printing-sourced face IMAGES is exercised on the exact_printing path.
const GIANT_NEW = printing({
  id: "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
  set: "m21",
  released_at: "2020-07-03",
  faces: [imageFace("giant-p0"), imageFace("giant-p1")],
});

function cardEntry(
  oracleId: string,
  name: string,
  faceNames: string[],
  faces?: CuratedCardEntry["faces"],
): CuratedCardEntry {
  return {
    oracle_id: oracleId,
    name,
    face_names: faceNames,
    faces: faces ?? faceNames.map((_, index) => imageFace(`${oracleId}-${index}`)),
  };
}

const BOLT_ENTRY = cardEntry(BOLT, "Lightning Bolt", ["lightning bolt"]);
const GIANT_ENTRY = cardEntry(GIANT, "Giant Growth", ["giant front", "giant back"]);
// Absent from PRINTINGS, so it takes the canonical path. Its two faces carry
// names distinct from each other and from the card name, so an alias built
// from the wrong one is visible.
const SOLO_ENTRY = cardEntry(SOLO, "Solo Print", ["solo front", "solo back"]);
// face_names is SHORTER than faces — real data for a face Scryfall names only
// on the front — so the fallback to the card name is exercised.
const SHORT_ENTRY: CuratedCardEntry = {
  oracle_id: SHORT,
  name: "Short Names",
  face_names: ["short front"],
  faces: [imageFace(`${SHORT}-0`), imageFace(`${SHORT}-1`)],
};
// A printing Scryfall has no images for. `gen-scryfall-printings.sh` writes an
// explicit null for every URL; the declared face type says `string`, which is
// why the cast is here rather than a defect in the fixture. Measured over
// client/public/scryfall-printings.json: 696 of the 88,846 stored printings
// have this shape, every one of them multi-face, and all 696 are null on
// `art_crop` wherever they are null on `normal`.
const NO_IMAGE_FACE = { normal: null as unknown as string, art_crop: null as unknown as string };
const BLANK_UNIMAGED = printing({
  id: "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
  set: "sld",
  released_at: "2021-01-01",
  faces: [NO_IMAGE_FACE, NO_IMAGE_FACE],
});
const BLANK_IMAGED = printing({
  id: "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
  set: "m19",
  released_at: "2018-07-13",
  faces: [imageFace("blank-p0"), imageFace("blank-p1")],
});
// Its scryfall-data.json entry DOES carry images, as all 211 affected cards do
// (measured, 0 missing), so canonical art is available for exactly this class.
const BLANK_ENTRY = cardEntry(BLANK, "Blank Print", ["blank front", "blank back"]);

/** Mirrors `scryfall-data.json`: cards are keyed by oracle id AND by one or
 *  two lowercased name forms, and tokens are keyed under a `token:` prefix. */
const CARDS: Record<string, CuratedCardEntry> = {
  [BOLT]: BOLT_ENTRY,
  "lightning bolt": BOLT_ENTRY,
  [GIANT]: GIANT_ENTRY,
  "giant growth": GIANT_ENTRY,
  "giant front": GIANT_ENTRY,
  [SOLO]: SOLO_ENTRY,
  [SHORT]: SHORT_ENTRY,
  [`token:${TOKEN_ORACLE}`]: cardEntry(TOKEN_ORACLE, "Saproling", ["saproling"]),
  "token:saproling": cardEntry(TOKEN_ORACLE, "Saproling", ["saproling"]),
};

const PRINTINGS: Record<string, PrintingEntry[]> = {
  [BOLT]: [BOLT_NEW, BOLT_OLD],
  [GIANT]: [GIANT_NEW],
};

const NEWEST: ArtChainEntry[] = [{ type: "newest" }];
const EMPTY_CHAIN: ArtChainEntry[] = [];
const LEA_161 = { setCode: "LEA", collectorNumber: "161" };

function input(overrides: Partial<CuratedMembershipInput> = {}): CuratedMembershipInput {
  return {
    packId: PACK,
    cards: CARDS,
    printings: PRINTINGS,
    artChain: NEWEST,
    artOverrides: {},
    ...overrides,
  };
}

function keysOf(descriptors: readonly ScryfallAssetDescriptor[]): string[] {
  return descriptors.map((value) => value.assetKey);
}

function exactPrintingIds(descriptors: readonly ScryfallAssetDescriptor[]): Set<string> {
  const ids = new Set<string>();
  for (const value of descriptors) {
    const match = /^asset:v1:exact_printing:(.{36})-/.exec(value.assetKey);
    if (match) ids.add(match[1]);
  }
  return ids;
}

function aliasesOf(descriptor: ScryfallAssetDescriptor | undefined): unknown[] {
  return (descriptor?.candidateKeys ?? [])
    .map((key) => decodeCandidateKey(key))
    .filter(([kind]) => kind === "oracle_alias")
    .map(([, tuple]) => tuple[0]);
}

function candidateKinds(descriptor: ScryfallAssetDescriptor): string[] {
  return descriptor.candidateKeys.map((key) => decodeCandidateKey(key)[0]);
}

function descriptorMembership(descriptors: readonly ScryfallAssetDescriptor[]): readonly [string, string][] {
  return descriptors.map(({ assetKey, sourceUrl }) => [assetKey, sourceUrl]);
}

describe("planCuratedMembership", () => {
  it("emits one printing per card across all three rungs", async () => {
    const { descriptors } = await planCuratedMembership(input());

    // Bolt 1 face + Giant 2 + Solo 2 + Short 2 = 7 faces x 3 rungs.
    expect(descriptors).toHaveLength(21);
    expect(exactPrintingIds(descriptors)).toEqual(new Set([BOLT_NEW.id, GIANT_NEW.id]));
    expect(new Set(keysOf(descriptors))).toHaveLength(21);
  });

  it("plans each card once per oracle id, not once per map key", async () => {
    // Deduplicating descriptors by assetKey would hide key-order inflation in
    // the output, so the rule is measured where it bites: the per-card work.
    // CARDS keys Lightning Bolt twice and Giant Growth three times, matching
    // the uneven 1/2/3-keys-per-card shape of the real scryfall-data map.
    const lookups: string[] = [];
    const printings = new Proxy(PRINTINGS, {
      get(target, property) {
        if (typeof property === "string") lookups.push(property);
        return target[property as string];
      },
    });

    await planCuratedMembership(input({ printings }));

    expect(lookups.filter((id) => id === BOLT)).toHaveLength(1);
    expect(lookups.filter((id) => id === GIANT)).toHaveLength(1);
  });

  it("limits descriptors to an optional Oracle-id membership", async () => {
    const allCards = await planCuratedMembership(input());
    const explicitAllCards = await planCuratedMembership(input({
      includedOracleIds: new Set([BOLT, GIANT, SOLO, SHORT]),
    }));
    const filtered = await planCuratedMembership(input({
      includedOracleIds: new Set([BOLT.toUpperCase()]),
    }));

    expect(explicitAllCards).toEqual(allCards);
    expect(exactPrintingIds(filtered.descriptors)).toEqual(new Set([BOLT_NEW.id]));
    expect(filtered.descriptors).toHaveLength(3);
    expect(filtered.membershipDigest).not.toBe(allCards.membershipDigest);
  });

  it("plans an empty supplied membership deterministically", async () => {
    const membership = await planCuratedMembership(input({ includedOracleIds: new Set() }));

    expect(membership.descriptors).toEqual([]);
    expect(membership.membershipDigest).toBe("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
  });

  it("skips token: keys entirely", async () => {
    const { descriptors } = await planCuratedMembership(input());

    expect(keysOf(descriptors).filter((key) => key.includes(TOKEN_ORACLE))).toEqual([]);
  });

  it("derives the small rung from normal and emits all three rungs per face", async () => {
    const { descriptors } = await planCuratedMembership(input());
    const bolt = descriptors.filter((value) => value.assetKey.includes(BOLT_NEW.id));

    expect(keysOf(bolt).sort()).toEqual([
      `asset:v1:exact_printing:${BOLT_NEW.id}-0-art_crop-art_crop`,
      `asset:v1:exact_printing:${BOLT_NEW.id}-0-full_card-normal`,
      `asset:v1:exact_printing:${BOLT_NEW.id}-0-full_card-small`,
    ]);
    expect(bolt.find((value) => value.assetKey.endsWith("small"))?.sourceUrl)
      .toBe(url(BOLT_NEW.id, "small"));
  });

  describe("face names", () => {
    it("aliases each canonical face by its own name, not the card name alone", async () => {
      const { descriptors } = await planCuratedMembership(input());
      const back = descriptors.find((value) =>
        value.assetKey === `asset:v1:canonical_card:${SOLO}-1-full_card-normal`);

      expect(aliasesOf(back)).toEqual(["solo print", "solo back"]);
    });

    it("pairs entry face names with printing face images on the exact path", async () => {
      const { descriptors } = await planCuratedMembership(input());
      const back = descriptors.find((value) =>
        value.assetKey === `asset:v1:exact_printing:${GIANT_NEW.id}-1-full_card-normal`);

      expect(aliasesOf(back)).toEqual(["giant growth", "giant back"]);
      expect(back?.sourceUrl).toBe(url("giant-p1", "normal"));
    });

    it("falls back to the card name for a face beyond the face_names array", async () => {
      const { descriptors } = await planCuratedMembership(input());
      const named = descriptors.find((value) =>
        value.assetKey === `asset:v1:canonical_card:${SHORT}-0-full_card-normal`);
      const unnamed = descriptors.find((value) =>
        value.assetKey === `asset:v1:canonical_card:${SHORT}-1-full_card-normal`);

      expect(aliasesOf(named)).toEqual(["short names", "short front"]);
      expect(aliasesOf(unnamed)).toEqual(["short names"]);
    });
  });

  describe("canonical fallback", () => {
    it("uses the canonical form for a card absent from scryfall-printings.json", async () => {
      const { descriptors } = await planCuratedMembership(input());
      const solo = descriptors.filter((value) => value.assetKey.includes(SOLO));

      expect(keysOf(solo).sort()).toEqual([
        `asset:v1:canonical_card:${SOLO}-0-art_crop-art_crop`,
        `asset:v1:canonical_card:${SOLO}-0-full_card-normal`,
        `asset:v1:canonical_card:${SOLO}-0-full_card-small`,
        `asset:v1:canonical_card:${SOLO}-1-art_crop-art_crop`,
        `asset:v1:canonical_card:${SOLO}-1-full_card-normal`,
        `asset:v1:canonical_card:${SOLO}-1-full_card-small`,
      ]);
      expect(solo.find((value) => value.assetKey === `asset:v1:canonical_card:${SOLO}-0-full_card-small`)?.sourceUrl)
        .toBe(url(`${SOLO}-0`, "small"));
    });

    it("uses the canonical form when the chain yields no winner", async () => {
      const noWinner: ArtChainEntry[] = [{ type: "set", setCode: "zzz", label: "Nope" }];
      const { descriptors } = await planCuratedMembership(input({ artChain: noWinner }));

      expect(keysOf(descriptors).filter((key) => key.startsWith("asset:v1:exact_printing:"))).toEqual([]);
      expect(keysOf(descriptors).sort()).toContain(`asset:v1:canonical_card:${BOLT}-0-full_card-normal`);
    });

    it("preserves the oracle candidate group and appends semantic face identities", async () => {
      const { descriptors } = await planCuratedMembership(input());
      const solo = descriptors.find((value) =>
        value.assetKey === `asset:v1:canonical_card:${SOLO}-1-full_card-normal`);

      // `useCardImage` passes an empty englishPrintingId when no stored
      // preference resolves to a printing, so the legacy oracle group remains
      // first. The appended face identities are a separate caller-owned
      // projection and preserve their canonical order.
      expect(solo?.candidateKeys).toEqual(cardCandidateGroups({
        oracleId: SOLO,
        oracleAliases: ["Solo Print", "solo back"],
        faceIndex: 1,
        variant: "full_card",
        rung: "normal",
      }).flatMap((group) => group.keys).concat(semanticCardCandidateGroups({
        oracleId: SOLO,
        cardName: "Solo Print",
        faceName: "solo back",
        variant: "full_card",
        rung: "normal",
      }).flatMap((group) => group.keys)));
      expect(solo?.candidateKeys.map((key) => decodeCandidateKey(key)[0]))
        .toEqual(["oracle", "oracle_alias", "oracle_alias", "oracle_face", "name_face"]);
    });

    it("never emits both asset forms for one card", async () => {
      const { descriptors } = await planCuratedMembership(input());
      const canonical = new Set(keysOf(descriptors)
        .flatMap((key) => /^asset:v1:canonical_card:(.{36})-/.exec(key)?.[1] ?? []));

      expect(canonical).toEqual(new Set([SOLO, SHORT]));
      expect(exactPrintingIds(descriptors).has(SOLO)).toBe(false);
    });
  });

  describe("a selected printing with no stored images", () => {
    // MEASURED over client/public/scryfall-printings.json and
    // scryfall-data.json: 214 oracle ids own at least one printing whose every
    // face stores null for both `normal` and `art_crop`; 211 of them have NO
    // renderable printing at all, and all 211 carry an image in
    // scryfall-data.json. Gating the canonical fallback on "selected no
    // printings" left those 211 cards contributing ZERO descriptors, while the
    // renderer falls through to canonical art — the network, in a feature whose
    // whole purpose is offline play.
    function blankInput(
      list: PrintingEntry[],
      overrides: Partial<CuratedMembershipInput> = {},
    ): CuratedMembershipInput {
      return input({
        cards: { ...CARDS, [BLANK]: BLANK_ENTRY },
        printings: { ...PRINTINGS, [BLANK]: list },
        ...overrides,
      });
    }

    /** Every key this card contributes, in either asset form. */
    function blankKeys(descriptors: readonly ScryfallAssetDescriptor[]): string[] {
      return keysOf(descriptors)
        .filter((key) => key.includes(BLANK) || key.includes(BLANK_UNIMAGED.id) || key.includes(BLANK_IMAGED.id))
        .sort();
    }

    function exactKeys(id: string): string[] {
      return [
        `asset:v1:exact_printing:${id}-0-art_crop-art_crop`,
        `asset:v1:exact_printing:${id}-0-full_card-normal`,
        `asset:v1:exact_printing:${id}-0-full_card-small`,
        `asset:v1:exact_printing:${id}-1-art_crop-art_crop`,
        `asset:v1:exact_printing:${id}-1-full_card-normal`,
        `asset:v1:exact_printing:${id}-1-full_card-small`,
      ];
    }

    it("selects the image-less printing under this chain, so the fixture is reachable", async () => {
      // The reachability control for the test below. Same printings array, same
      // preferences, and the index-0 printing's face URLs as the ONLY
      // difference: the planner emits that printing's own descriptors, so the
      // chain does select it. A canonical result below is therefore caused by
      // its missing URLs, not by a fixture the chain never reaches.
      const imaged = { ...BLANK_UNIMAGED, faces: [imageFace("blank-n0"), imageFace("blank-n1")] };
      const { descriptors } = await planCuratedMembership(blankInput([imaged, BLANK_IMAGED]));

      expect(blankKeys(descriptors)).toEqual(exactKeys(BLANK_UNIMAGED.id));
    });

    it("emits the canonical form when every face of the selected printing is null", async () => {
      const { descriptors } = await planCuratedMembership(blankInput([BLANK_UNIMAGED, BLANK_IMAGED]));

      expect(blankKeys(descriptors)).toEqual([
        `asset:v1:canonical_card:${BLANK}-0-art_crop-art_crop`,
        `asset:v1:canonical_card:${BLANK}-0-full_card-normal`,
        `asset:v1:canonical_card:${BLANK}-0-full_card-small`,
        `asset:v1:canonical_card:${BLANK}-1-art_crop-art_crop`,
        `asset:v1:canonical_card:${BLANK}-1-full_card-normal`,
        `asset:v1:canonical_card:${BLANK}-1-full_card-small`,
      ]);
      // The bytes come from the scryfall-data entry, which is what the
      // renderer's oracle/name path resolves for this card.
      expect(descriptors.find((value) =>
        value.assetKey === `asset:v1:canonical_card:${BLANK}-0-full_card-normal`)?.sourceUrl)
        .toBe(url(`${BLANK}-0`, "normal"));
    });

    it("keeps the exact form and emits no canonical when the selected printing renders", async () => {
      // The mixed population — a renderable printing beside the image-less one,
      // measured at 3 oracle ids. The exclusivity gate must still fire here:
      // both forms carry this card's oracle-group candidate keys, and
      // `sortMatches` orders `canonical_card:` first, so a card reachable by
      // both would render the WRONG art at `sources[0]`.
      const { descriptors } = await planCuratedMembership(blankInput([BLANK_IMAGED, BLANK_UNIMAGED]));

      expect(blankKeys(descriptors)).toEqual(exactKeys(BLANK_IMAGED.id));
    });

    it("emits no canonical when only one of two selected printings has images", async () => {
      // A chain naming `source_printing` selects TWO printings here: the deck's
      // (image-less) and the chain's fallback (renderable). Descriptors are
      // produced, so the gate does not fire — deliberately. Emitting canonical
      // alongside would break the exclusivity invariant and hand `sources[0]`
      // to canonical for the chain render too. The deck render's fallback to
      // canonical art stays a network fetch: an accepted residual of the
      // one-form rule, not an oversight.
      const chain: ArtChainEntry[] = [{ type: "source_printing" }, { type: "newest" }];
      const { descriptors } = await planCuratedMembership(blankInput([BLANK_IMAGED, BLANK_UNIMAGED], {
        artChain: chain,
        deckPrintings: [{ oracleId: BLANK, source: { setCode: "SLD", collectorNumber: "1" } }],
      }));

      expect(blankKeys(descriptors)).toEqual(exactKeys(BLANK_IMAGED.id));
    });
  });

  it("keys candidates on the lowercased oracle id the renderer stamps", async () => {
    const upper = { ...SOLO_ENTRY, oracle_id: SOLO.toUpperCase() };
    const { descriptors } = await planCuratedMembership(input({
      cards: { [SOLO.toUpperCase()]: upper },
    }));

    expect(keysOf(descriptors)).toContain(`asset:v1:canonical_card:${SOLO}-0-full_card-normal`);
    expect(descriptors[0].candidateKeys.map((key) => decodeCandidateKey(key)[1][0]))
      .toContain(SOLO);
  });

  it("looks printings up by the stored oracle id, which the generator does not fold", async () => {
    // `gen-scryfall-printings.sh` writes `key: .[0].oracle_id` unmodified, and
    // `getCardPrintings` indexes it with the same unmodified value. Folding the
    // lookup key would miss the entry that the renderer would still find.
    const upper = { ...BOLT_ENTRY, oracle_id: BOLT.toUpperCase() };
    const { descriptors } = await planCuratedMembership(input({
      cards: { [BOLT.toUpperCase()]: upper },
      printings: { [BOLT.toUpperCase()]: [BOLT_NEW, BOLT_OLD] },
    }));

    expect(exactPrintingIds(descriptors)).toEqual(new Set([BOLT_NEW.id]));
  });

  it("skips a face whose stored URLs are both null", async () => {
    const nullFaced: CuratedCardEntry = {
      ...SOLO_ENTRY,
      faces: [imageFace(`${SOLO}-0`), { normal: null, art_crop: null }],
    };
    const { descriptors } = await planCuratedMembership(input({
      cards: { ...CARDS, [SOLO]: nullFaced },
    }));

    expect(keysOf(descriptors).filter((key) => key.includes(`${SOLO}-1-`))).toEqual([]);
    expect(keysOf(descriptors).filter((key) => key.includes(`${SOLO}-0-`))).toHaveLength(3);
  });

  it("skips a printing face with a null normal but keeps its art crop", async () => {
    const halfNull = { ...BOLT_NEW, faces: [{ normal: null as unknown as string, art_crop: url(BOLT_NEW.id, "art_crop") }] };
    const { descriptors } = await planCuratedMembership(input({
      printings: { ...PRINTINGS, [BOLT]: [halfNull, BOLT_OLD] },
    }));

    expect(keysOf(descriptors).filter((key) => key.includes(BOLT_NEW.id)))
      .toEqual([`asset:v1:exact_printing:${BOLT_NEW.id}-0-art_crop-art_crop`]);
  });

  describe("deck printings", () => {
    it("carries the deck's printing under the default empty chain", async () => {
      const { descriptors } = await planCuratedMembership(input({
        artChain: EMPTY_CHAIN,
        deckPrintings: [{ oracleId: BOLT, source: LEA_161 }],
      }));

      expect(exactPrintingIds(descriptors)).toEqual(new Set([BOLT_OLD.id]));
    });

    it("emits no canonical form for a deck card — an ACCEPTED divergence", async () => {
      // Under an empty chain a deck card plans exactly one printing, so the
      // exclusivity gate suppresses canonical and this card's battlefield,
      // hand and search renders resolve through the oracle group onto the
      // DECK printing. Offline they show the deck's art where online they
      // would show canonical.
      //
      // This is pinned deliberately. Adding canonical back to close that gap
      // looks like a coverage win and is a regression: both forms would carry
      // this card's oracle-group keys, and `sortMatches` puts
      // `canonical_card:` first, so canonical would take `sources[0]` for the
      // DECK render too and break the deck half of the feature. Do not
      // "fix" this without re-deriving that ordering.
      const { descriptors } = await planCuratedMembership(input({
        artChain: EMPTY_CHAIN,
        deckPrintings: [{ oracleId: BOLT, source: LEA_161 }],
      }));

      expect(keysOf(descriptors).filter((key) => key.startsWith(`asset:v1:canonical_card:${BOLT}-`)))
        .toEqual([]);
      expect(keysOf(descriptors)).toContain(`asset:v1:exact_printing:${BOLT_OLD.id}-0-full_card-normal`);
    });

    it("resolves the deck printing through a chain that names source_printing", async () => {
      const chain: ArtChainEntry[] = [{ type: "source_printing" }, { type: "newest" }];
      const { descriptors } = await planCuratedMembership(input({
        artChain: chain,
        deckPrintings: [{ oracleId: BOLT, source: LEA_161 }],
      }));

      expect(exactPrintingIds(descriptors)).toEqual(new Set([BOLT_NEW.id, BOLT_OLD.id, GIANT_NEW.id]));
    });

    it("omits the deck printing when the chain would not display it", async () => {
      // The renderer reaches its source-printing branch only for an empty
      // chain or a chain naming source_printing. Under any other chain the
      // deck's own art never appears on screen, so the pack must not carry it.
      const { descriptors } = await planCuratedMembership(input({
        deckPrintings: [{ oracleId: BOLT, source: LEA_161 }],
      }));

      expect(exactPrintingIds(descriptors)).toEqual(new Set([BOLT_NEW.id, GIANT_NEW.id]));
    });

    it("matches a deck printing whose oracle id differs in case", async () => {
      const { descriptors } = await planCuratedMembership(input({
        artChain: EMPTY_CHAIN,
        deckPrintings: [{ oracleId: BOLT.toUpperCase(), source: LEA_161 }],
      }));

      expect(exactPrintingIds(descriptors)).toEqual(new Set([BOLT_OLD.id]));
    });

    it("keeps broad semantics on the stored no-source winner and source semantics on every selected printing", async () => {
      const chain: ArtChainEntry[] = [{ type: "source_printing" }, { type: "newest" }];
      const first = await planCuratedMembership(input({
        artChain: chain,
        deckPrintings: [
          { oracleId: BOLT, source: { setCode: "M20", collectorNumber: "1" } },
          { oracleId: BOLT, source: LEA_161 },
          { oracleId: BOLT, source: { setCode: "lea", collectorNumber: "161" } },
        ],
      }));
      const second = await planCuratedMembership(input({
        artChain: chain,
        deckPrintings: [
          { oracleId: BOLT, source: { setCode: "lea", collectorNumber: "161" } },
          { oracleId: BOLT, source: { setCode: "m20", collectorNumber: "1" } },
          { oracleId: BOLT, source: LEA_161 },
        ],
      }));
      const newRows = first.descriptors.filter((value) => value.assetKey.includes(BOLT_NEW.id));
      const oldRows = first.descriptors.filter((value) => value.assetKey.includes(BOLT_OLD.id));

      expect(descriptorMembership(second.descriptors)).toEqual(descriptorMembership(first.descriptors));
      expect(second.membershipDigest).toBe(first.membershipDigest);
      expect(newRows).toHaveLength(3);
      expect(oldRows).toHaveLength(3);
      for (const descriptor of newRows) {
        expect(candidateKinds(descriptor)).toEqual(expect.arrayContaining([
          "source_printing", "oracle_face", "name_face",
        ]));
      }
      for (const descriptor of oldRows) {
        expect(candidateKinds(descriptor)).toContain("source_printing");
        expect(candidateKinds(descriptor)).not.toContain("oracle_face");
        expect(candidateKinds(descriptor)).not.toContain("name_face");
      }
    });

    it("uses the first sorted source printing as primary when no stored selection exists", async () => {
      const { descriptors } = await planCuratedMembership(input({
        artChain: EMPTY_CHAIN,
        deckPrintings: [
          { oracleId: BOLT, source: { setCode: "M20", collectorNumber: "1" } },
          { oracleId: BOLT, source: LEA_161 },
        ],
      }));
      const oldRows = descriptors.filter((value) => value.assetKey.includes(BOLT_OLD.id));
      const newRows = descriptors.filter((value) => value.assetKey.includes(BOLT_NEW.id));

      expect(exactPrintingIds(descriptors)).toEqual(new Set([BOLT_NEW.id, BOLT_OLD.id]));
      for (const descriptor of oldRows) {
        expect(candidateKinds(descriptor)).toEqual(expect.arrayContaining(["oracle_face", "name_face"]));
      }
      for (const descriptor of newRows) {
        expect(candidateKinds(descriptor)).not.toContain("oracle_face");
        expect(candidateKinds(descriptor)).not.toContain("name_face");
      }
    });

    it("keeps mixed-case collector contexts through selection while permutations retain one primary", async () => {
      const upperCollector = printing({
        id: "abababab-abab-4aba-8aba-abababababab",
        set: "abc",
        collector_number: "A63",
      });
      const upperSource = { setCode: "AbC", collectorNumber: "A63" };
      const lowerSource = { setCode: "abc", collectorNumber: "a63" };
      const first = await planCuratedMembership(input({
        printings: { ...PRINTINGS, [BOLT]: [upperCollector, BOLT_OLD] },
        artChain: EMPTY_CHAIN,
        deckPrintings: [
          { oracleId: BOLT, source: LEA_161 },
          { oracleId: BOLT, source: lowerSource },
          { oracleId: BOLT, source: upperSource },
          { oracleId: BOLT, source: upperSource },
        ],
      }));
      const second = await planCuratedMembership(input({
        printings: { ...PRINTINGS, [BOLT]: [upperCollector, BOLT_OLD] },
        artChain: EMPTY_CHAIN,
        deckPrintings: [
          { oracleId: BOLT, source: upperSource },
          { oracleId: BOLT, source: LEA_161 },
          { oracleId: BOLT, source: lowerSource },
        ],
      }));
      const upperRows = first.descriptors.filter((value) => value.assetKey.includes(upperCollector.id));
      const oldRows = first.descriptors.filter((value) => value.assetKey.includes(BOLT_OLD.id));

      expect(exactPrintingIds(first.descriptors)).toEqual(new Set([upperCollector.id, BOLT_OLD.id]));
      expect(descriptorMembership(second.descriptors)).toEqual(descriptorMembership(first.descriptors));
      expect(second.membershipDigest).toBe(first.membershipDigest);
      for (const descriptor of upperRows) {
        expect(candidateKinds(descriptor)).toEqual(expect.arrayContaining(["oracle_face", "name_face"]));
      }
      for (const descriptor of oldRows) {
        expect(candidateKinds(descriptor)).not.toContain("oracle_face");
        expect(candidateKinds(descriptor)).not.toContain("name_face");
      }
    });

    it("skips an image-less nominal winner and uses the first printing that emits descriptors", async () => {
      const chain: ArtChainEntry[] = [{ type: "source_printing" }, { type: "newest" }];
      const { descriptors } = await planCuratedMembership(input({
        cards: { ...CARDS, [BLANK]: BLANK_ENTRY },
        printings: { ...PRINTINGS, [BLANK]: [BLANK_UNIMAGED, BLANK_IMAGED] },
        artChain: chain,
        deckPrintings: [{ oracleId: BLANK, source: { setCode: "M19", collectorNumber: "1" } }],
      }));
      const imaged = descriptors.filter((value) => value.assetKey.includes(BLANK_IMAGED.id));

      expect(keysOf(descriptors).some((key) => key.includes(BLANK_UNIMAGED.id))).toBe(false);
      expect(imaged).toHaveLength(6);
      for (const descriptor of imaged) {
        expect(candidateKinds(descriptor)).toEqual(expect.arrayContaining(["source_printing", "oracle_face", "name_face"]));
      }
    });

    it("keeps a partially imaged primary's broad face/rung identities off later source printings", async () => {
      const partial = printing({
        id: "ffffffff-ffff-4fff-8fff-ffffffffffff",
        set: "m20",
        collector_number: "9",
        faces: [NO_IMAGE_FACE, imageFace("partial-back")],
      });
      const later = printing({
        id: "99999999-9999-4999-8999-999999999999",
        set: "m19",
        collector_number: "8",
        faces: [imageFace("later-front"), imageFace("later-back")],
      });
      const chain: ArtChainEntry[] = [{ type: "source_printing" }, { type: "newest" }];
      const { descriptors } = await planCuratedMembership(input({
        printings: { ...PRINTINGS, [GIANT]: [partial, later] },
        artChain: chain,
        deckPrintings: [{ oracleId: GIANT, source: { setCode: "M19", collectorNumber: "8" } }],
      }));
      const partialRows = descriptors.filter((value) => value.assetKey.includes(partial.id));
      const laterRows = descriptors.filter((value) => value.assetKey.includes(later.id));

      expect(partialRows).toHaveLength(3);
      expect(laterRows).toHaveLength(6);
      for (const descriptor of partialRows) {
        expect(candidateKinds(descriptor)).toEqual(expect.arrayContaining([
          "source_printing", "oracle_face", "name_face",
        ]));
      }
      for (const descriptor of laterRows) {
        expect(candidateKinds(descriptor)).toContain("source_printing");
        expect(candidateKinds(descriptor)).not.toContain("oracle_face");
        expect(candidateKinds(descriptor)).not.toContain("name_face");
      }
    });
  });

  describe("art overrides", () => {
    it("omits the chain winner that a divergent override supersedes", async () => {
      // An override suppresses the chain entirely at render time, so the chain
      // winner is art the user cannot see. Carrying it would cost bytes and
      // put a second exact_printing under this card's oracle-group keys.
      const artOverrides: Record<string, CardArtOverride> = {
        [BOLT]: { scryfallId: BOLT_OLD.id, setCode: "lea", collectorNumber: "161" },
      };
      const { descriptors } = await planCuratedMembership(input({ artOverrides }));

      expect(exactPrintingIds(descriptors)).toEqual(new Set([BOLT_OLD.id, GIANT_NEW.id]));
    });

    it("carries exactly one printing when the override is the chain winner", async () => {
      const artOverrides: Record<string, CardArtOverride> = {
        [BOLT]: { scryfallId: BOLT_NEW.id, setCode: "m20", collectorNumber: "1" },
      };
      const { descriptors } = await planCuratedMembership(input({ artOverrides }));

      expect(exactPrintingIds(descriptors)).toEqual(new Set([BOLT_NEW.id, GIANT_NEW.id]));
    });

    it("falls to canonical when the override names a printing that is gone", async () => {
      // `selectedPrinting` returns null rather than falling through to the
      // chain, and `resolveOverrideUrl` likewise yields no URL, so the render
      // resolves through the oracle group — which is the canonical asset.
      const artOverrides: Record<string, CardArtOverride> = {
        [BOLT]: { scryfallId: GIANT_NEW.id, setCode: "m21", collectorNumber: "1" },
      };
      const { descriptors } = await planCuratedMembership(input({ artOverrides }));

      expect(exactPrintingIds(descriptors)).toEqual(new Set([GIANT_NEW.id]));
      expect(keysOf(descriptors)).toContain(`asset:v1:canonical_card:${BOLT}-0-full_card-normal`);
    });
  });

  describe("membership digest", () => {
    it("is stable across permutations of the input map order", async () => {
      const reversed = Object.fromEntries(Object.entries(CARDS).reverse());
      const first = await planCuratedMembership(input());
      const second = await planCuratedMembership(input({ cards: reversed }));

      expect(second.membershipDigest).toBe(first.membershipDigest);
      expect(keysOf(second.descriptors)).toEqual(keysOf(first.descriptors));
    });

    it("is sorted by assetKey", async () => {
      const { descriptors } = await planCuratedMembership(input());

      expect(keysOf(descriptors)).toEqual([...keysOf(descriptors)].sort());
    });

    it("changes when only a sourceUrl changes and the assetKeys are identical", async () => {
      // A `scryfall-data.json` regeneration can move the bytes behind an
      // unchanged canonical_card key. Digesting keys alone would miss it.
      const regenerated: CuratedCardEntry = {
        ...SOLO_ENTRY,
        faces: [
          { normal: url(`${SOLO}-0`, "normal").replace("?1", "?2"), art_crop: url(`${SOLO}-0`, "art_crop") },
          imageFace(`${SOLO}-1`),
        ],
      };
      const before = await planCuratedMembership(input());
      const after = await planCuratedMembership(input({ cards: { ...CARDS, [SOLO]: regenerated } }));

      expect(keysOf(after.descriptors)).toEqual(keysOf(before.descriptors));
      expect(after.membershipDigest).not.toBe(before.membershipDigest);
    });

    it("changes when the membership changes", async () => {
      const before = await planCuratedMembership(input({ artChain: EMPTY_CHAIN }));
      const after = await planCuratedMembership(input({
        artChain: EMPTY_CHAIN,
        deckPrintings: [{ oracleId: BOLT, source: LEA_161 }],
      }));

      expect(after.membershipDigest).not.toBe(before.membershipDigest);
    });

    it("is a 64-character lowercase hex CatalogRoot", async () => {
      const { membershipDigest } = await planCuratedMembership(input());

      expect(membershipDigest).toMatch(/^[0-9a-f]{64}$/);
    });
  });
});
