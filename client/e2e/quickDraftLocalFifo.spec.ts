import { expect, test, type Locator, type Page } from "@playwright/test";

/**
 * A Quick Draft started from the draft menu, in a browser on the built client
 * and the real draft engine, drafts on the local FIFO runtime: the seat picks
 * from its own queue of passed packs, and the pack toolbar shows the seat's
 * own step in the round and that queue, as the engine's view publishes them.
 *
 * Seven engine bots share the pod with the player, and a bot drafts whenever
 * it holds a pack, so by the time the player sees a view every pack still in
 * play has reached the player's queue. The counts below follow from that and
 * from the size of the boosters this file serves.
 *
 * The built client does not ship its generated data files, so each test
 * serves the three a local start reads: the set's booster pool and the set
 * catalog (empty), which a set start reads, and a card database for the
 * cards in play, which both starts read.
 */

const SET_CODE = "TST";
const SET_NAME = "Proof Set";
const SPELLS = ["Alpha", "Beta", "Gamma", "Delta"];
const BASICS = ["Plains", "Island", "Swamp", "Mountain", "Forest"];
/** Each booster of the set: four commons. */
const CARDS_PER_BOOSTER = 4;

/** The set's draft-pools.json entry. */
function draftPools(): Record<string, unknown> {
  return {
    [SET_CODE.toLowerCase()]: {
      code: SET_CODE,
      name: SET_NAME,
      release_date: null,
      pack_variants: [{
        contents: [{ slot: "common", count: CARDS_PER_BOOSTER, choices: [{ sheet: "common", weight: 1 }] }],
        weight: 1,
      }],
      pack_variants_total_weight: 1,
      sheets: {
        common: {
          cards: SPELLS.map((name, index) => ({
            name, set_code: SET_CODE, collector_number: String(index + 1), rarity: "common", weight: 1,
          })),
          total_weight: SPELLS.length,
          foil: false,
          balance_colors: false,
        },
      },
      prints: [],
      basic_lands: [],
    },
  };
}

/** card-data.json for the cards in play: the commons as vanilla 1/1s, and the basic lands. */
function cardDatabase(): Record<string, unknown> {
  return Object.fromEntries([...SPELLS, ...BASICS].map((name) => {
    const basic = BASICS.includes(name);
    return [name.toLowerCase(), {
      name,
      mana_cost: { type: "NoCost" },
      card_type: { supertypes: basic ? ["Basic"] : [], core_types: [basic ? "Land" : "Creature"], subtypes: [] },
      power: basic ? null : "1",
      toughness: basic ? null : "1",
      loyalty: null,
      defense: null,
      oracle_text: null,
      abilities: [],
      triggers: [],
      static_abilities: [],
      replacements: [],
      keywords: [],
      bracket_signals: { game_changer: false, mass_land_denial: false, extra_turn: false, efficient_tutor: false },
    }];
  }));
}

async function serveDraftData(page: Page): Promise<void> {
  await page.route("**/draft-pools.json", (route) => route.fulfill({ json: draftPools() }));
  await page.route("**/scryfall-sets.json", (route) => route.fulfill({ json: {} }));
  await page.route("**/card-data.json", (route) => route.fulfill({ json: cardDatabase() }));
}

/**
 * The toolbar's position label, matched whole. The queue line follows it in
 * the toolbar's text, so a substring match would take "Pack 1 Pick 1" for
 * "Pack 1 Pick 13 packs in your queue".
 */
function position(page: Page, text: string): Locator {
  return page.locator("[data-pack-toolbar]").getByText(text, { exact: true });
}

/** Select the first card of the pack on show and confirm it. */
async function pickFirstCard(page: Page): Promise<void> {
  await page.locator("[data-pack-card-activation]").first().click();
  await page.getByRole("button", { name: "Confirm Pick", exact: true }).click();
}

test("a Quick set draft started from the draft menu drafts from the seat's own pack queue", async ({ page }) => {
  await serveDraftData(page);
  await page.goto("/draft");
  await page.getByRole("button", { name: /^Quick Draft/ }).click();
  await page.getByRole("button", { name: `Add a pack of ${SET_NAME}` }).click();
  await page.getByRole("button", { name: "Start Draft", exact: true }).click();
  await page.getByRole("button", { name: "Start Drafting", exact: true }).click();

  const queue = page.locator("[data-pack-queue]");
  // The player's own booster, then the three bot boosters that still hold a
  // card when they reach the player; the other four were drafted empty.
  await expect(position(page, "Pack 1 Pick 1")).toBeVisible();
  await expect(queue).toHaveText("4 packs in your queue");

  // Each pick passes the player's head on and never brings a pack back.
  for (const [pick, queued] of [[2, "3 packs"], [3, "2 packs"], [4, "1 pack"]] as const) {
    await pickFirstCard(page);
    await expect(position(page, `Pack 1 Pick ${pick}`)).toBeVisible();
    await expect(queue).toHaveText(`${queued} in your queue`);
  }

  // The last pick drains every queue, so the second round opens and its
  // steps count from the start again.
  await pickFirstCard(page);
  await expect(position(page, "Pack 2 Pick 1")).toBeVisible();
  await expect(queue).toHaveText("4 packs in your queue");
});

test("a Quick cube draft started from the draft menu drafts from the seat's own pack queue", async ({ page }) => {
  await serveDraftData(page);
  await page.goto("/draft");
  await page.getByRole("button", { name: /^Cube Draft/ }).click();
  // The default settings deal 8 seats 3 boosters of 15 cards: 360 cards.
  await page.getByPlaceholder("1 Lightning Bolt").fill(SPELLS.map((name) => `90 ${name}`).join("\n"));
  await page.getByRole("button", { name: "Start Cube Draft", exact: true }).click();
  await page.getByRole("button", { name: "Start Drafting", exact: true }).click();

  const queue = page.locator("[data-pack-queue]");
  // No 15-card booster empties in seven bot picks, so all eight reach the player.
  await expect(position(page, "Pack 1 Pick 1")).toBeVisible();
  await expect(queue).toHaveText("8 packs in your queue");

  // The player's booster goes round the seven bots and rejoins the queue.
  await pickFirstCard(page);
  await expect(position(page, "Pack 1 Pick 2")).toBeVisible();
  await expect(queue).toHaveText("8 packs in your queue");
});
