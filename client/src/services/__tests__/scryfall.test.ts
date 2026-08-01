import { execFileSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const REPO_ROOT = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../../..",
);

function withTempDir(run: (dir: string) => void, prefix = "scryfall-") {
  const dir = mkdtempSync(path.join(tmpdir(), prefix));
  try {
    run(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function makeLocalDataMap(
  cards: Record<string, { name: string; mana_cost?: string; cmc?: number; type_line?: string; oracle_id?: string }>,
): Response {
  const map: Record<string, unknown> = {};
  for (const [key, card] of Object.entries(cards)) {
    map[key.toLowerCase()] = {
      name: card.name,
      oracle_id: card.oracle_id ?? key,
      face_names: [card.name.toLowerCase()],
      mana_cost: card.mana_cost ?? "{1}",
      cmc: card.cmc ?? 1,
      type_line: card.type_line ?? "Instant",
      colors: [],
      color_identity: [],
      keywords: [],
      faces: [
        {
          normal: `https://img.example/${encodeURIComponent(card.name)}.jpg`,
          art_crop: `https://img.example/${encodeURIComponent(card.name)}-art.jpg`,
        },
      ],
    };
  }
  return new Response(JSON.stringify(map), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

function makeEmptyCardDataMap(): Response {
  return new Response(JSON.stringify({}), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

async function loadScryfallModule() {
  vi.resetModules();
  return import("../scryfall.ts");
}

describe("normalizeCardName", () => {
  it("strips set code brackets", async () => {
    const { normalizeCardName } = await loadScryfallModule();
    expect(normalizeCardName("Goblin Lackey [UZ]")).toBe("Goblin Lackey");
  });

  it("strips angle-bracket treatment tags", async () => {
    const { normalizeCardName } = await loadScryfallModule();
    expect(normalizeCardName("Abrade <retro>")).toBe("Abrade");
    expect(normalizeCardName("Kiki-Jiki, Mirror Breaker <timeshifted>")).toBe(
      "Kiki-Jiki, Mirror Breaker",
    );
  });

  it("strips collector numbers in angle brackets", async () => {
    const { normalizeCardName } = await loadScryfallModule();
    expect(normalizeCardName("Mountain <288>")).toBe("Mountain");
  });

  it("strips foil markers", async () => {
    const { normalizeCardName } = await loadScryfallModule();
    expect(normalizeCardName("Goblin Rabblemaster [PRM-BAB] (F)")).toBe(
      "Goblin Rabblemaster",
    );
  });

  it("strips combined decorators", async () => {
    const { normalizeCardName } = await loadScryfallModule();
    expect(
      normalizeCardName("Krenko, Mob Boss <retro> [RVR] (F)"),
    ).toBe("Krenko, Mob Boss");
  });

  it("leaves plain card names unchanged", async () => {
    const { normalizeCardName } = await loadScryfallModule();
    expect(normalizeCardName("Lightning Bolt")).toBe("Lightning Bolt");
  });
});

describe("scryfallLegalityKey", () => {
  it("uses Scryfall legality keys for constructed formats", async () => {
    const { scryfallLegalityKey } = await loadScryfallModule();

    expect(scryfallLegalityKey("Modern")).toBe("modern");
    expect(scryfallLegalityKey("Premodern")).toBe("premodern");
  });

  it("maps commander variants to Scryfall legality keys", async () => {
    const { scryfallLegalityKey } = await loadScryfallModule();

    expect(scryfallLegalityKey("Brawl")).toBe("standardbrawl");
    expect(scryfallLegalityKey("HistoricBrawl")).toBe("brawl");
    expect(scryfallLegalityKey("DuelCommander")).toBe("duel");
    expect(scryfallLegalityKey("PauperCommander")).toBe("paupercommander");
  });

  it("returns undefined for formats without a Scryfall legality key", async () => {
    const { scryfallLegalityKey } = await loadScryfallModule();

    expect(scryfallLegalityKey("TinyLeaders")).toBeUndefined();
    expect(scryfallLegalityKey("FreeForAll")).toBeUndefined();
    expect(scryfallLegalityKey("Archenemy")).toBeUndefined();
  });
});

describe("pickOldestPrinting", () => {
  it("picks the earliest release date and lowest collector number on ties", async () => {
    const { pickOldestPrinting } = await loadScryfallModule();
    const printings = [
      {
        id: "new",
        set: "neo",
        set_name: "Kamigawa: Neon Dynasty",
        collector_number: "10",
        released_at: "2022-02-11",
        border_color: "black",
        frame_effects: [],
        full_art: false,
        faces: [{ normal: "https://img.example/new.jpg", art_crop: "https://img.example/new-art.jpg" }],
      },
      {
        id: "old",
        set: "lea",
        set_name: "Limited Edition Alpha",
        collector_number: "2",
        released_at: "1993-08-05",
        border_color: "black",
        frame_effects: [],
        full_art: false,
        faces: [{ normal: "https://img.example/old.jpg", art_crop: "https://img.example/old-art.jpg" }],
      },
      {
        id: "same-day-later-cn",
        set: "lea",
        set_name: "Limited Edition Alpha",
        collector_number: "10",
        released_at: "1993-08-05",
        border_color: "black",
        frame_effects: [],
        full_art: false,
        faces: [{ normal: "https://img.example/same-day.jpg", art_crop: "https://img.example/same-day-art.jpg" }],
      },
    ];

    expect(pickOldestPrinting(printings).id).toBe("old");
  });
});

describe("fetchCardData", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("returns card data from local JSON", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(
      makeLocalDataMap({
        "lightning bolt": { name: "Lightning Bolt" },
      }),
    );

    const { fetchCardData } = await loadScryfallModule();
    const card = await fetchCardData("Lightning Bolt");

    expect(card.name).toBe("Lightning Bolt");
    // Only the local data fetch — no API calls
    expect(global.fetch).toHaveBeenCalledTimes(1);
  });

  it("throws when card is not in local data (no API fallback)", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(makeEmptyCardDataMap());

    const { fetchCardData } = await loadScryfallModule();
    await expect(fetchCardData("Nonexistent Card")).rejects.toThrow(
      /not in local data/,
    );

    // Only the local data fetch — no API calls
    expect(global.fetch).toHaveBeenCalledTimes(1);
  });

  it("normalizes decorated names before local lookup", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(
      makeLocalDataMap({
        abrade: { name: "Abrade" },
      }),
    );

    const { fetchCardData } = await loadScryfallModule();
    const card = await fetchCardData("Abrade <retro>");

    expect(card.name).toBe("Abrade");
  });

  it("resolves ASCII names to diacritic local data keys (issue #1497)", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(
      makeLocalDataMap({
        "éomer of the riddermark": { name: "Éomer of the Riddermark", oracle_id: "eomer-oracle" },
      }),
    );

    const { resolveOracleIdSync, fetchCardImageUrl, loadScryfallData } = await loadScryfallModule();
    await loadScryfallData();
    expect(resolveOracleIdSync("Eomer of the Riddermark")).toBe("eomer-oracle");
    await expect(fetchCardImageUrl("Eomer of the Riddermark", 0)).resolves.toMatch(/^https?:\/\//);
  });
});

describe("fetchCardData — combined multi-face names", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  // A two-face card keyed the way the export does it: by front-face name and by
  // the spaced display name, but NOT by the glued combined form.
  function makeDfcDataMap(): Response {
    const dfc = {
      oracle_id: "peter-oracle",
      face_names: ["peter parker", "the amazing spider-man"],
      faces: [
        { normal: "https://img.example/peter-front.jpg", art_crop: "https://img.example/peter-front-art.jpg" },
        { normal: "https://img.example/peter-back.jpg", art_crop: "https://img.example/peter-back-art.jpg" },
      ],
      layout: "transform",
      name: "Peter Parker // The Amazing Spider-Man",
      mana_cost: "{1}{W}",
      cmc: 2,
      type_line: "Legendary Creature — Human Hero",
      colors: ["W"],
      color_identity: ["W"],
      keywords: [],
    };
    const map: Record<string, unknown> = {
      "peter parker": dfc,
      "peter parker // the amazing spider-man": dfc,
      // A single-faced card whose own printed name contains "//" (issue #4790).
      "sp//dr, piloted by peni": {
        oracle_id: "spdr-oracle",
        face_names: ["sp//dr, piloted by peni"],
        faces: [{ normal: "https://img.example/spdr.jpg", art_crop: "https://img.example/spdr-art.jpg" }],
        name: "SP//dr, Piloted by Peni",
        mana_cost: "{3}{W}{U}",
        cmc: 5,
        type_line: "Legendary Artifact Creature — Spider Hero",
        colors: ["W", "U"],
        color_identity: ["W", "U"],
        keywords: [],
      },
    };
    return new Response(JSON.stringify(map), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  }

  it("resolves a hand-typed glued double-faced name via the front face", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(makeDfcDataMap());

    const { fetchCardData } = await loadScryfallModule();
    const card = await fetchCardData("Peter Parker//The Amazing Spider-Man");

    expect(card.name).toBe("Peter Parker // The Amazing Spider-Man");
    expect(global.fetch).toHaveBeenCalledTimes(1);
  });

  it("resolves the canonical spaced double-faced name directly", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(makeDfcDataMap());

    const { fetchCardData } = await loadScryfallModule();
    const card = await fetchCardData("Peter Parker // The Amazing Spider-Man");

    expect(card.name).toBe("Peter Parker // The Amazing Spider-Man");
  });

  it("does not mis-split a single-faced card whose name contains \"//\" (issue #4790)", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(makeDfcDataMap());

    const { fetchCardData } = await loadScryfallModule();
    const card = await fetchCardData("SP//dr, Piloted by Peni");

    // Its own name is a primary key, so the exact match wins before any split.
    expect(card.name).toBe("SP//dr, Piloted by Peni");
    expect(card.type_line).toContain("Spider Hero");
  });
});

describe("fetchCardImageUrl", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("returns image URL from local data", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(
      makeLocalDataMap({
        "lightning bolt": { name: "Lightning Bolt" },
      }),
    );

    const { fetchCardImageUrl } = await loadScryfallModule();
    const url = await fetchCardImageUrl("Lightning Bolt", 0, "normal");

    expect(url).toBe("https://img.example/Lightning%20Bolt.jpg");
    expect(global.fetch).toHaveBeenCalledTimes(1);
  });

  it("falls back to a real local printing when canonical image data is Scryfall's soon placeholder", async () => {
    const oracleId = "war-room-oracle";
    global.fetch = vi.fn()
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            [oracleId]: {
              oracle_id: oracleId,
              face_names: ["war room"],
              faces: [
                {
                  normal: "https://errors.scryfall.com/soon.jpg",
                  art_crop: "https://errors.scryfall.com/soon.jpg",
                },
              ],
              layout: "normal",
              name: "War Room",
              mana_cost: "",
              cmc: 0,
              type_line: "Land",
              colors: [],
              color_identity: [],
              keywords: [],
            },
            "war room": {
              oracle_id: oracleId,
              face_names: ["war room"],
              faces: [
                {
                  normal: "https://errors.scryfall.com/soon.jpg",
                  art_crop: "https://errors.scryfall.com/soon.jpg",
                },
              ],
              layout: "normal",
              name: "War Room",
              mana_cost: "",
              cmc: 0,
              type_line: "Land",
              colors: [],
              color_identity: [],
              keywords: [],
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        ),
      )
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            [oracleId]: [
              {
                id: "future-placeholder",
                set: "soc",
                set_name: "Secrets of Strixhaven Commander",
                collector_number: "422",
                released_at: "2026-04-24",
                border_color: "black",
                frame_effects: [],
                full_art: false,
                faces: [
                  {
                    normal: "https://errors.scryfall.com/soon.jpg",
                    art_crop: "https://errors.scryfall.com/soon.jpg",
                  },
                ],
              },
              {
                id: "real-printing",
                set: "cmm",
                set_name: "Commander Masters",
                collector_number: "1054",
                released_at: "2023-08-04",
                border_color: "black",
                frame_effects: [],
                full_art: false,
                faces: [
                  {
                    normal: "https://cards.scryfall.io/normal/front/w/r/war-room.jpg",
                    art_crop: "https://cards.scryfall.io/art_crop/front/w/r/war-room.jpg",
                  },
                ],
              },
            ],
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        ),
      );

    const { fetchCardImageAssetByOracleId, fetchCardImageUrl } = await loadScryfallModule();
    const url = await fetchCardImageUrl("War Room", 0, "normal");
    const oracleAsset = await fetchCardImageAssetByOracleId(oracleId, "War Room", "normal");

    expect(url).toBe("https://cards.scryfall.io/normal/front/w/r/war-room.jpg");
    expect(oracleAsset.src).toBe("https://cards.scryfall.io/normal/front/w/r/war-room.jpg");
    expect(global.fetch).toHaveBeenCalledTimes(2);
  });

  it("throws when card image is not in local data (no API fallback)", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(makeEmptyCardDataMap());

    const { fetchCardImageUrl } = await loadScryfallModule();
    await expect(
      fetchCardImageUrl("Nonexistent Card", 0, "normal"),
    ).rejects.toThrow(/not in local data/);

    expect(global.fetch).toHaveBeenCalledTimes(1);
  });

  it("normalizes decorated names for image lookup", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(
      makeLocalDataMap({
        mountain: { name: "Mountain" },
      }),
    );

    const { fetchCardImageUrl } = await loadScryfallModule();
    const url = await fetchCardImageUrl("Mountain <288>", 0, "art_crop");

    expect(url).toBe("https://img.example/Mountain-art.jpg");
  });
});

describe("fetchCardImageAssetByOracleId — reversible cards (issue #2031)", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("resolves front-face art keyed by face oracle_id", async () => {
    const oracleId = "ea9709b6-4c37-4d5a-b04d-cd4c42e4f9dd";
    global.fetch = vi.fn().mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          [oracleId]: {
            oracle_id: oracleId,
            face_names: ["propaganda", "propaganda"],
            faces: [
              {
                normal: "https://img.example/propaganda-front.jpg",
                art_crop: "https://img.example/propaganda-front-art.jpg",
              },
              {
                normal: "https://img.example/propaganda-back.jpg",
                art_crop: "https://img.example/propaganda-back-art.jpg",
              },
            ],
            layout: "reversible_card",
            name: "Propaganda // Propaganda",
            mana_cost: "{2}{U}",
            cmc: 3,
            type_line: "Enchantment",
            colors: ["U"],
            color_identity: ["U"],
            keywords: [],
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
    );

    const { fetchCardImageAssetByOracleId } = await loadScryfallModule();
    const asset = await fetchCardImageAssetByOracleId(oracleId, "Propaganda", "normal");

    expect(asset.src).toBe("https://img.example/propaganda-front.jpg");
    expect(global.fetch).toHaveBeenCalledTimes(1);
  });
});

describe("Scryfall generation scripts — reversible cards (issue #2031)", () => {
  const oracleId = "ea9709b6-4c37-4d5a-b04d-cd4c42e4f9dd";

  it("keys image data by face oracle_id when reversible cards omit root oracle_id", () => {
    withTempDir((dir) => {
      const input = path.join(dir, "oracle-cards.json");
      const output = path.join(dir, "scryfall-data.json");
      writeFileSync(
        input,
        JSON.stringify([
          {
            layout: "reversible_card",
            name: "Propaganda // Propaganda",
            card_faces: [
              {
                oracle_id: oracleId,
                name: "Propaganda",
                mana_cost: "{2}{U}",
                cmc: 3,
                type_line: "Enchantment",
                colors: ["U"],
                color_identity: ["U"],
                keywords: ["Ward"],
                image_uris: {
                  normal: "https://img.example/front.jpg",
                  art_crop: "https://img.example/front-art.jpg",
                },
              },
              {
                oracle_id: oracleId,
                name: "Propaganda",
                image_uris: {
                  normal: "https://img.example/back.jpg",
                  art_crop: "https://img.example/back-art.jpg",
                },
              },
            ],
          },
        ]),
      );

      execFileSync("bash", [path.join(REPO_ROOT, "scripts/gen-scryfall-images.sh")], {
        cwd: REPO_ROOT,
        env: {
          ...process.env,
          SCRYFALL_ORACLE_FILE: input,
          SCRYFALL_IMAGES_OUTPUT: output,
        },
        stdio: "pipe",
      });

      const generated = JSON.parse(readFileSync(output, "utf8"));
      expect(generated[oracleId]).toMatchObject({
        oracle_id: oracleId,
        layout: "reversible_card",
        color_identity: ["U"],
        keywords: ["Ward"],
      });
      expect(generated[oracleId].faces[0].normal).toBe("https://img.example/front.jpg");
    });
  });

  it("groups printings by face oracle_id when reversible cards omit root oracle_id", () => {
    withTempDir((dir) => {
      const input = path.join(dir, "default-cards.json");
      const output = path.join(dir, "scryfall-printings.json");
      writeFileSync(
        input,
        JSON.stringify([
          {
            id: "old-printing",
            layout: "reversible_card",
            name: "Propaganda // Propaganda",
            set: "sld",
            set_name: "Secret Lair Drop",
            collector_number: "1",
            released_at: "2024-01-01",
            border_color: "borderless",
            full_art: false,
            card_faces: [
              {
                oracle_id: oracleId,
                image_uris: {
                  normal: "https://img.example/old-front.jpg",
                  art_crop: "https://img.example/old-front-art.jpg",
                },
              },
            ],
          },
          {
            id: "new-printing",
            layout: "reversible_card",
            name: "Propaganda // Propaganda",
            set: "sld",
            set_name: "Secret Lair Drop",
            collector_number: "2",
            released_at: "2025-01-01",
            border_color: "borderless",
            full_art: true,
            card_faces: [
              {
                oracle_id: oracleId,
                image_uris: {
                  normal: "https://img.example/new-front.jpg",
                  art_crop: "https://img.example/new-front-art.jpg",
                },
              },
            ],
          },
        ]),
      );

      execFileSync("bash", [path.join(REPO_ROOT, "scripts/gen-scryfall-printings.sh")], {
        cwd: REPO_ROOT,
        env: {
          ...process.env,
          SCRYFALL_DEFAULT_CARDS_FILE: input,
          SCRYFALL_PRINTINGS_OUTPUT: output,
        },
        stdio: "pipe",
      });

      const generated = JSON.parse(readFileSync(output, "utf8"));
      expect(generated[oracleId]).toHaveLength(2);
      expect(generated[oracleId][0].id).toBe("new-printing");
      expect(generated[oracleId][1].id).toBe("old-printing");
    });
  });
});

describe("fetchTokenImageUrl — ability-aware printing selection (issue #502)", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  // A Scryfall token-search response whose first hit is a vanilla 1/1 Human.
  function makeTokenSearchResponse(): Response {
    return new Response(
      JSON.stringify({
        data: [{
          name: "Human Token",
          keywords: [],
          image_uris: { normal: "https://img.example/vanilla-human.jpg" },
        }],
        total_cards: 1,
        has_more: false,
      }),
      { status: 200, headers: { "Content-Type": "application/json" } },
    );
  }

  function make404(): Response {
    return new Response("", { status: 404 });
  }

  // Decode every captured search URL's `q=` query string. The first fetch
  // call is always the local Scryfall-data load; search calls follow.
  function capturedQueries(fetchMock: ReturnType<typeof vi.fn>): string[] {
    return fetchMock.mock.calls
      .map((c) => String(c[0]))
      .filter((u) => u.includes("/cards/search?"))
      .map((u) => decodeURIComponent(new URL(u).searchParams.get("q") ?? ""));
  }

  it("Test 1 — a vanilla token query carries is:vanilla", async () => {
    const fetchMock = vi
      .fn()
      // Token-less local data map — forces the API path (no `token:human` key).
      .mockResolvedValueOnce(makeEmptyCardDataMap())
      .mockResolvedValue(makeTokenSearchResponse());
    global.fetch = fetchMock;

    const { fetchTokenImageUrl } = await loadScryfallModule();
    await fetchTokenImageUrl("Human", "normal", {
      power: 1,
      toughness: 1,
      colors: ["White"],
      subtypes: ["Human"],
      hasAbilities: false,
    });

    const queries = capturedQueries(fetchMock);
    expect(queries.length).toBeGreaterThan(0);
    expect(queries[0]).toContain("is:vanilla");
  });

  it("Test 2 — is:vanilla is added only when hasAbilities === false", async () => {
    // Each sub-case re-loads the module so the module-level `loadScryfallData`
    // cache is reset and the leading empty-card-data fetch is consumed afresh.

    // hasAbilities: false → query contains is:vanilla.
    {
      const { fetchTokenImageUrl } = await loadScryfallModule();
      const falseMock = vi
        .fn()
        .mockResolvedValueOnce(makeEmptyCardDataMap())
        .mockResolvedValue(makeTokenSearchResponse());
      global.fetch = falseMock;
      await fetchTokenImageUrl("Human", "normal", {
        power: 1, toughness: 1, colors: ["White"], subtypes: ["Human"],
        hasAbilities: false,
      });
      expect(capturedQueries(falseMock)[0]).toContain("is:vanilla");
    }

    // hasAbilities: true (e.g. a Spirit with flying) → NO is:vanilla.
    {
      const { fetchTokenImageUrl } = await loadScryfallModule();
      const trueMock = vi
        .fn()
        .mockResolvedValueOnce(makeEmptyCardDataMap())
        .mockResolvedValue(makeTokenSearchResponse());
      global.fetch = trueMock;
      await fetchTokenImageUrl("Spirit", "normal", {
        power: 1, toughness: 1, colors: ["White"], subtypes: ["Spirit"],
        hasAbilities: true,
      });
      const queries = capturedQueries(trueMock);
      expect(queries.length).toBeGreaterThan(0);
      for (const q of queries) {
        expect(q).not.toContain("is:vanilla");
      }
    }

    // hasAbilities omitted (preview / no-GameObject path) → NO is:vanilla.
    {
      const { fetchTokenImageUrl } = await loadScryfallModule();
      const undefMock = vi
        .fn()
        .mockResolvedValueOnce(makeEmptyCardDataMap())
        .mockResolvedValue(makeTokenSearchResponse());
      global.fetch = undefMock;
      await fetchTokenImageUrl("Human", "normal", {
        power: 1, toughness: 1, colors: ["White"], subtypes: ["Human"],
      });
      const queries = capturedQueries(undefMock);
      expect(queries.length).toBeGreaterThan(0);
      for (const q of queries) {
        expect(q).not.toContain("is:vanilla");
      }
    }
  });

  it("Test 3 — a vanilla-narrowed query resolves to a vanilla printing", async () => {
    global.fetch = vi
      .fn()
      .mockResolvedValueOnce(makeEmptyCardDataMap())
      .mockResolvedValue(makeTokenSearchResponse());

    const { fetchTokenImageUrl } = await loadScryfallModule();
    const url = await fetchTokenImageUrl("Human", "normal", {
      power: 1,
      toughness: 1,
      colors: ["White"],
      subtypes: ["Human"],
      hasAbilities: false,
    });

    expect(url).toBe("https://img.example/vanilla-human.jpg");
  });

  it("Test 4 — a 404 on the first is:vanilla rung advances to the next rung", async () => {
    global.fetch = vi
      .fn()
      .mockResolvedValueOnce(makeEmptyCardDataMap())
      // First (narrowest) is:vanilla rung 404s — an empty Scryfall search.
      .mockResolvedValueOnce(make404())
      // The next relaxed rung yields the vanilla hit.
      .mockResolvedValue(makeTokenSearchResponse());

    const { fetchTokenImageUrl } = await loadScryfallModule();
    const url = await fetchTokenImageUrl("Human", "normal", {
      power: 1,
      toughness: 1,
      colors: ["White"],
      subtypes: ["Human"],
      hasAbilities: false,
    });

    expect(url).toBe("https://img.example/vanilla-human.jpg");
  });
});

describe("rateLimitedFetch (token/search API)", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("retries on network error with backoff", async () => {
    vi.useFakeTimers();

    const tokenResponse = new Response(
      JSON.stringify({
        data: [{
          name: "Goblin Token",
          image_uris: { normal: "https://img.example/goblin.jpg" },
        }],
        total_cards: 1,
        has_more: false,
      }),
      { status: 200, headers: { "Content-Type": "application/json" } },
    );

    global.fetch = vi
      .fn()
      .mockRejectedValueOnce(new TypeError("Failed to fetch"))
      .mockResolvedValueOnce(tokenResponse);

    const { fetchTokenImageUrl } = await loadScryfallModule();
    const pending = fetchTokenImageUrl("Goblin", "normal");

    await vi.advanceTimersByTimeAsync(2000);
    const url = await pending;

    expect(url).toBe("https://img.example/goblin.jpg");
    expect(global.fetch).toHaveBeenCalledTimes(2);
  });
});

describe("image size derivation", () => {
  // The five-path-segment shape every real `cards.scryfall.io` URL has. The
  // mainline `makeLocalDataMap` fixture is deliberately NOT used here: it emits
  // `https://img.example/<Name>.jpg`, a one-segment URL that is not derivable,
  // so every assertion below would pass vacuously against it.
  const SLUG = "front/w/r/war-room.jpg";
  const sized = (size: string, query = "") =>
    `https://cards.scryfall.io/${size}/${SLUG}${query}`;
  const SIZES = ["small", "normal", "large", "art_crop"] as const;

  it("derives every size from every other size", async () => {
    const { deriveImageUrl, imageUrlSize } = await loadScryfallModule();

    for (const from of SIZES) {
      expect(imageUrlSize(sized(from))).toBe(from);
      for (const to of SIZES) {
        const input = sized(from);
        const derived = deriveImageUrl(input, to);
        expect(derived).toBe(sized(to));
        // Non-vacuity: a broken guard that returned its input unchanged would
        // otherwise satisfy every same-size case and look green.
        if (from !== to) expect(derived).not.toBe(input);
      }
    }
  });

  it("preserves the query string", async () => {
    const { deriveImageUrl } = await loadScryfallModule();

    const input = sized("normal", "?1783905318");
    const derived = deriveImageUrl(input, "small");
    expect(derived).toBe(
      "https://cards.scryfall.io/small/front/w/r/war-room.jpg?1783905318",
    );
    expect(derived).not.toBe(input);
  });

  it("derives back faces", async () => {
    const { deriveImageUrl } = await loadScryfallModule();

    const input = "https://cards.scryfall.io/normal/back/w/r/war-room.jpg?1783905318";
    const derived = deriveImageUrl(input, "small");
    expect(derived).toBe(
      "https://cards.scryfall.io/small/back/w/r/war-room.jpg?1783905318",
    );
    expect(derived).not.toBe(input);
  });

  it("returns non-derivable input unchanged, without throwing", async () => {
    const { CARD_BACK_URL, deriveImageUrl, imageUrlSize } = await loadScryfallModule();

    const nonDerivable = [
      // Every face-down card renders through `useCardImage("")`.
      "",
      // `OpponentHand.test.tsx` mocks bare filenames — `new URL()` throws on these.
      "Focused Opponent Card.png",
      // Four path segments, so the card back never gets a ladder.
      CARD_BACK_URL,
      // One segment. Must stay byte-identical or `isPlaceholderImageUrl`'s `===`
      // stops gating the printing-fallback chain.
      "https://errors.scryfall.com/soon.jpg",
      // Six segments.
      "https://cards.scryfall.io/normal/front/w/r/extra/war-room.jpg",
      // Five segments but an unrecognized size.
      "https://cards.scryfall.io/png/front/w/r/war-room.png",
    ];

    for (const input of nonDerivable) {
      expect(deriveImageUrl(input, "small")).toBe(input);
      expect(imageUrlSize(input)).toBeNull();
    }
    expect(imageUrlSize(null)).toBeNull();
    expect(imageUrlSize(undefined)).toBeNull();
  });
});

describe("local face size resolution", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  const NORMAL = "https://cards.scryfall.io/normal/front/w/r/war-room.jpg?1783905318";
  const SMALL = "https://cards.scryfall.io/small/front/w/r/war-room.jpg?1783905318";
  const ART_CROP = "https://cards.scryfall.io/art_crop/front/w/r/war-room.jpg?1783905318";

  function makeSizedDataMap(key: string, name: string): Response {
    return new Response(
      JSON.stringify({
        [key]: {
          oracle_id: key,
          face_names: [name.toLowerCase()],
          faces: [{ normal: NORMAL, art_crop: ART_CROP }],
          layout: "normal",
          name,
          mana_cost: "",
          cmc: 0,
          type_line: "Land",
          colors: [],
          color_identity: [],
          keywords: [],
        },
      }),
      { status: 200, headers: { "Content-Type": "application/json" } },
    );
  }

  it("serves a real small asset from the stored normal URL", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(makeSizedDataMap("war room", "War Room"));

    const { fetchCardImageUrl } = await loadScryfallModule();
    const url = await fetchCardImageUrl("War Room", 0, "small");

    expect(url).toBe(SMALL);
    expect(url).not.toBe(NORMAL);
  });

  it("collapses large to normal", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(makeSizedDataMap("war room", "War Room"));

    const { fetchCardImageUrl } = await loadScryfallModule();

    expect(await fetchCardImageUrl("War Room", 0, "large")).toBe(NORMAL);
  });

  it("serves art_crop untouched", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(makeSizedDataMap("war room", "War Room"));

    const { fetchCardImageUrl } = await loadScryfallModule();

    expect(await fetchCardImageUrl("War Room", 0, "art_crop")).toBe(ART_CROP);
  });

  it("serves a real small asset for local token images", async () => {
    global.fetch = vi.fn().mockResolvedValueOnce(makeSizedDataMap("token:goblin", "Goblin"));

    const { fetchTokenImageUrl } = await loadScryfallModule();
    const url = await fetchTokenImageUrl("Goblin", "small");

    expect(url).toBe(SMALL);
    expect(url).not.toBe(NORMAL);
    // The local hit must short-circuit the Scryfall search API entirely.
    expect(global.fetch).toHaveBeenCalledTimes(1);
  });

  it("serves a real small asset from printings, and still rejects placeholders", async () => {
    const { resolvePrintingImageUrl } = await loadScryfallModule();
    const printing = {
      id: "real-printing",
      set: "cmm",
      set_name: "Commander Masters",
      collector_number: "1054",
      released_at: "2023-08-04",
      border_color: "black",
      frame_effects: [],
      full_art: false,
      faces: [{ normal: NORMAL, art_crop: ART_CROP }],
    };

    const small = resolvePrintingImageUrl(printing, 0, "small");
    expect(small).toBe(SMALL);
    expect(small).not.toBe(NORMAL);
    expect(resolvePrintingImageUrl(printing, 0, "large")).toBe(NORMAL);

    const placeholder = {
      ...printing,
      faces: [{
        normal: "https://errors.scryfall.com/soon.jpg",
        art_crop: "https://errors.scryfall.com/soon.jpg",
      }],
    };
    expect(resolvePrintingImageUrl(placeholder, 0, "small")).toBeNull();
  });
});

describe("scryfall-fetch mv-failure recovery (PR #6775 review)", () => {
  const LIB_PATH = path
    .join(REPO_ROOT, "scripts/lib/scryfall-fetch.sh")
    .replace(/\\/g, "/");

  // Runs `scryfall_download` in a sourced shell where `curl` and `mv` are
  // shadowed by shell functions. Function-shadowing (not a `cp` reassignment
  // of SCRYFALL_CURL) is required: SCRYFALL_CURL's real curl invocation is
  // array-expanded ("${SCRYFALL_CURL[@]}" -o "$tmp" "$url"), so a stub must be
  // a same-named function to intercept it, not a value substituted in for the
  // array's first word. The shared finalizer captures mv stderr while it
  // decides whether another writer produced a valid destination, so the
  // anti-vacuity marker for mv is recorded through a shell variable rather
  // than stderr text. The validator is different: the real source
  // deliberately runs it inside a subshell (`( "$validator" "$file" )`, see
  // scryfall_download's header comment) to keep a caller-supplied
  // validator's shell-variable writes from leaking back into this library's
  // scope. That isolation is intentional and stays in place — but it means
  // a shell-variable marker for the validator would read back as "never
  // called" even when it was. A subshell isolates variable writes, not file
  // writes, so the validator stub instead records its own invocation as a
  // marker *file* under `dir`.
  function runMvFailureScript(
    dir: string,
    opts: {
      validator?: string;
      destContent?: string; // undefined => destination absent
      curlPayload?: string;
      mvSucceeds?: boolean;
    },
  ): {
    out: string;
    dest: string;
    stderr: string;
    validatorCalls: number;
  } {
    const dest = path.join(dir, "dest.json").replace(/\\/g, "/");
    const payload = path.join(dir, "fixture-payload.json").replace(/\\/g, "/");
    const marker = path.join(dir, "validator-called").replace(/\\/g, "/");
    const stderr = path.join(dir, "scryfall.stderr").replace(/\\/g, "/");
    writeFileSync(payload, opts.curlPayload ?? JSON.stringify({ ok: true }));
    if (opts.destContent !== undefined) {
      writeFileSync(dest, opts.destContent);
    }

    const validatorArg = opts.validator ? ` "${opts.validator}"` : "";
    // The marker file (not a shell variable — see the class comment above)
    // proves the caller-supplied validator was actually invoked, not just
    // that a validator arg was passed: a stub that hardcodes "validator arg
    // present -> fail" without ever calling through would leave no marker
    // behind, and the invocation count below would remain zero.
    const validatorDef = opts.validator
      ? `${opts.validator}() { echo 1 >> "${marker}"; jq -e '.data' "$1" >/dev/null 2>&1; }\n`
      : "";
    const mvDef = opts.mvSucceeds
      ? "" // no override: the real `mv` binary runs, and succeeds.
      : 'mv() { MV_CALLED=1; return 1; }\n';

    const finalScript = `
set -uo pipefail
source "${LIB_PATH}"
SCRYFALL_CURL=(curl)
curl() {
  local out=""
  while [ $# -gt 0 ]; do
    case "$1" in
      -o) out="$2"; shift 2 ;;
      *) shift ;;
    esac
  done
  cp "${payload}" "$out"
}
${mvDef}${validatorDef}scryfall_download "https://example.invalid/data.json" "${dest}"${validatorArg} 2> "${stderr}"
echo "RC=$?"
echo "MV_CALLED=\${MV_CALLED:-0}"
echo "TMP_COUNT=$(ls -1 "${dest}".* 2>/dev/null | wc -l)"
`;

    const out = execFileSync("bash", ["-c", finalScript], {
      cwd: REPO_ROOT,
      stdio: "pipe",
      timeout: 20_000,
    }).toString();
    const validatorCalls = existsSync(marker)
      ? readFileSync(marker, "utf8").trim().split("\n").length
      : 0;
    return {
      out,
      dest,
      stderr: readFileSync(stderr, "utf8"),
      validatorCalls,
    };
  }

  it("case 1: mv fails, destination exists and passes the default validator -> rc 0, tmp cleaned, destination byte-identical to before", () => {
    withTempDir((dir) => {
      const before = JSON.stringify({ ok: true, marker: "pre-existing" });
      const { out, dest } = runMvFailureScript(dir, { destContent: before });

      expect(out).toContain("MV_CALLED=1");
      expect(out).toContain("RC=0");
      expect(out).toMatch(/TMP_COUNT=0/);
      expect(readFileSync(dest, "utf8")).toBe(before);
    });
  });

  it("case 2: mv fails, destination absent -> rc 1, tmp cleaned", () => {
    withTempDir((dir) => {
      const { out, dest, stderr } = runMvFailureScript(dir, {});

      expect(out).toContain("MV_CALLED=1");
      expect(out).toContain("RC=1");
      expect(out).toMatch(/TMP_COUNT=0/);
      expect(stderr).toContain("scryfall: could not rename");
      expect(() => readFileSync(dest, "utf8")).toThrow();
    });
  });

  it("case 3: mv fails, destination is invalid JSON -> rc 1, tmp cleaned", () => {
    withTempDir((dir) => {
      const before = "not-json{";
      const { out, dest, stderr } = runMvFailureScript(dir, {
        destContent: before,
      });

      expect(out).toContain("MV_CALLED=1");
      expect(out).toContain("RC=1");
      expect(out).toMatch(/TMP_COUNT=0/);
      expect(stderr).toContain("scryfall: could not rename");
      // Untouched: the recovery path must never overwrite a bad destination
      // with the freshly-downloaded (but un-mv'd) tmp content either.
      expect(readFileSync(dest, "utf8")).toBe(before);
    });
  });

  it("case 4: mv fails, destination is valid JSON but fails the caller-supplied validator -> rc 1, validator actually invoked", () => {
    withTempDir((dir) => {
      // Syntactically valid JSON, but missing the `.data` field the
      // caller-supplied validator requires — must NOT be accepted merely
      // because it parses.
      const before = JSON.stringify({ foo: "bar" });
      const { out, validatorCalls } = runMvFailureScript(dir, {
        destContent: before,
        // The download itself must PASS the caller validator (it is checked
        // on the tmp file before the mv) so this case reaches the mv-failure
        // recovery branch and fails there, on the destination's shape.
        curlPayload: JSON.stringify({ data: [{ fresh: true }] }),
        validator: "scryfall_test_validate_has_data",
      });

      expect(out).toContain("MV_CALLED=1");
      expect(out).toContain("RC=1");
      // The tmp gate and the failed-rename recovery check both run the
      // caller-supplied validator. Pinning two calls proves this failed on
      // the destination's shape rather than merely at the pre-mv gate.
      expect(validatorCalls).toBe(2);
    });
  });

  it("case 4b (positive control for case 4): mv fails, destination is valid JSON and PASSES the caller-supplied validator -> rc 0, destination unchanged", () => {
    withTempDir((dir) => {
      const before = JSON.stringify({ data: [{ foo: "bar" }] });
      const { out, dest, validatorCalls } = runMvFailureScript(dir, {
        destContent: before,
        // Same pre-mv gate consideration as case 4: the download must pass
        // the caller validator for the recovery branch to be reached at all.
        curlPayload: JSON.stringify({ data: [{ fresh: true }] }),
        validator: "scryfall_test_validate_has_data",
      });

      expect(out).toContain("MV_CALLED=1");
      expect(out).toContain("RC=0");
      expect(readFileSync(dest, "utf8")).toBe(before);
      // Both acceptance points must use the caller-supplied validator.
      expect(validatorCalls).toBe(2);
    });
  });

  it("case 5: mv succeeds -> normal path, destination written from the download, no recovery marker", () => {
    withTempDir((dir) => {
      const payload = JSON.stringify({ ok: true, fresh: true });
      const { out, dest } = runMvFailureScript(dir, {
        mvSucceeds: true,
        curlPayload: payload,
      });

      expect(out).not.toContain("MV_CALLED=1");
      expect(out).toContain("RC=0");
      expect(readFileSync(dest, "utf8")).toBe(payload);
    });
  });

  it("case 5b: freshly-downloaded content fails the caller-supplied validator -> rc 1, destination never written", () => {
    withTempDir((dir) => {
      // Valid JSON (passes the transport-level scryfall_validate_json
      // default gate that always runs on the fresh tmp file) but missing the
      // `.data` field the caller-supplied validator requires. The caller
      // validator gates the tmp file BEFORE the mv, so the bad body must
      // never land at the destination where a later non-validating reader
      // (scryfall_fetch_bulk's other callers) would trust it.
      const payload = JSON.stringify({ ok: true, fresh: true });
      const { out, dest, validatorCalls } = runMvFailureScript(dir, {
        mvSucceeds: true,
        curlPayload: payload,
        validator: "scryfall_test_validate_has_data",
      });

      expect(out).not.toContain("MV_CALLED=1");
      expect(validatorCalls).toBe(1);
      expect(out).toContain("RC=1");
      expect(existsSync(dest)).toBe(false);
      // No orphaned tmp file either.
      expect(out).toMatch(/TMP_COUNT=0/);
    });
  });

  // Runs gen-scryfall-sets.sh hermetically: a stub `curl` on PATH only ever
  // serves file:// URLs (our fixture) and fails fast — no live-network
  // fallback — for anything else, so a regression that stops reading the
  // SCRYFALL_SETS_FILE/URL/OUTPUT seams can never hit the live
  // https://api.scryfall.com/sets endpoint from these tests.
  function runSetsScript(dir: string, cachedContent: string): Record<string, unknown> {
    const binDir = path.join(dir, "bin");
    mkdirSync(binDir);
    const stubPath = path.join(binDir, "curl");
    writeFileSync(
      stubPath,
      `#!/usr/bin/env bash
out=""
url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    http*|file://*) url="$1"; shift ;;
    *) shift ;;
  esac
done
case "$url" in
  file://*)
    src="\${url#file://}"
    case "$src" in
      /?:*) src="\${src#/}" ;;
    esac
    cp "$src" "$out"
    exit 0
    ;;
  *)
    echo "stub-curl: refusing non-file:// URL: $url" >&2
    exit 1
    ;;
esac
`,
    );
    chmodSync(stubPath, 0o755);

    const badCache = path.join(dir, "sets.json");
    writeFileSync(badCache, cachedContent);
    const validFixture = path.join(dir, "fixture-sets.json");
    writeFileSync(
      validFixture,
      JSON.stringify({
        data: [
          {
            code: "abc",
            name: "A Boring Cardboard",
            icon_svg_uri: "https://img.example/abc.svg",
            released_at: "2024-01-01",
          },
        ],
      }),
    );
    const output = path.join(dir, "scryfall-sets.json");

    let threw: unknown;
    try {
      execFileSync("bash", [path.join(REPO_ROOT, "scripts/gen-scryfall-sets.sh")], {
        // A temp cwd keeps a misbehaving run (seams unread, real relative
        // "data/scryfall" + "client/public/scryfall-sets.json" paths used
        // instead) from writing into the actual repo checkout.
        cwd: dir,
        env: {
          ...process.env,
          PATH: `${binDir}${path.delimiter}${process.env.PATH ?? ""}`,
          SCRYFALL_SETS_FILE: badCache,
          SCRYFALL_SETS_URL: pathToFileURL(validFixture).href,
          SCRYFALL_SETS_OUTPUT: output,
        },
        stdio: "pipe",
        timeout: 20_000,
      });
    } catch (err) {
      threw = err;
    }

    if (threw !== undefined) {
      // Assert *why* it failed, not just *that* it failed — a broken stub,
      // missing bash, or a PATH that doesn't propagate through MSYS would
      // also throw. The only regression this branch should ever report is
      // "seams unread, so the script reached for its hardcoded
      // https://api.scryfall.com URL and our stub curl refused it"; any
      // other failure cause fails loudly on the assertion below instead.
      const err = threw as { stderr?: Buffer | string; stdout?: Buffer | string };
      const stderrText = err.stderr?.toString() ?? "";
      expect(stderrText).toContain("stub-curl: refusing non-file:// URL");
    }

    expect(threw).toBeUndefined();
    return JSON.parse(readFileSync(output, "utf8"));
  }

  it("case 6: gen-scryfall-sets.sh discards an invalid cached sets file and refetches from SCRYFALL_SETS_URL", () => {
    withTempDir((dir) => {
      const generated = runSetsScript(dir, "not-json{");
      expect(generated.abc).toMatchObject({ name: "A Boring Cardboard" });
    });
  });

  it.each([
    ["empty data array", JSON.stringify({ data: [] })],
    ["non-array data", JSON.stringify({ data: "oops" })],
  ])(
    "case 6b (%s): a cached sets file that is valid JSON but fails the .data array-shape check is discarded and refetched",
    (_label, cachedContent) => {
      withTempDir((dir) => {
        // jq -e '.data' alone is truthy for any non-null value: {"data":"oops"}
        // would crash the map() transform, and {"data":[]} would be cached as
        // a permanently-empty scryfall-sets.json behind the OUTPUT early-exit.
        // Both must be treated exactly like a corrupt cache: discard, refetch.
        const generated = runSetsScript(dir, cachedContent);
        expect(generated.abc).toMatchObject({ name: "A Boring Cardboard" });
      });
    },
  );
});
