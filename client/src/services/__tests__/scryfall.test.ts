import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const REPO_ROOT = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../../..",
);

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

  function withTempDir(run: (dir: string) => void) {
    const dir = mkdtempSync(path.join(tmpdir(), "scryfall-gen-"));
    try {
      run(dir);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  }

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
