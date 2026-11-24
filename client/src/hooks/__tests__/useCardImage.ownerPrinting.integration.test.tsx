import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { act, cleanup, render, renderHook, within, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import init, { clear_game_state } from "@wasm/engine";
import { getSharedAdapter } from "../../adapter/wasm-adapter";
import type { CardPrinting, GameObject } from "../../adapter/types";
import { CardImage } from "../../components/card/CardImage";
import { submitDeckList, submitParsedDeck, type SubmissionArtSnapshot, type SubmissionDeck } from "../../services/deckSubmission";
import type { PrintingEntry } from "../../services/scryfall";
import type { VisualPackBackend } from "../../services/visualPacks/backend";
import { encodeCandidateKey } from "../../services/visualPacks/candidateKeys";
import { assetKey, catalogRoot, decodeCandidateKey, installedRevision, packId } from "../../services/visualPacks/types";
import type { ResolutionKey, ResolutionResponse, RevisionEvent } from "../../services/visualPacks/types";
import { useConnectivityStore } from "../../stores/connectivityStore";
import { FORMAT_DEFAULTS } from "../../stores/multiplayerStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { useCardImage } from "../useCardImage";

const forestOracle = "b34bb2dc-c1af-4d77-b0b3-a0fb342a5fc6";
const forest = {
  name: "Forest", scryfall_oracle_id: forestOracle, mana_cost: { type: "NoCost" },
  card_type: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Forest"] },
  keywords: [], abilities: [], triggers: [], static_abilities: [], replacements: [],
};
const slotZero = "3279314f-d639-4489-b2ab-3621bb3ca64b";
const slotOne = "a4df3e89-610c-4f42-a93a-e694342307cc";
const alias = "5ac2dbbc-8bf4-45fd-a5c1-7597c885fc6b";
const newest = "b460f5f7-c7c9-400c-8419-23d614f45bf9";
const viewer = "a305e44f-4253-4754-b83f-1e34103d77b0";
const entries: PrintingEntry[] = [
  { id: newest, set: "ecl", collector_number: "273", released_at: "2026-01-23" },
  { id: viewer, set: "tla", collector_number: "286", released_at: "2025-11-21" },
  { id: slotOne, set: "m21", collector_number: "273", released_at: "2020-07-03" },
  { id: slotZero, set: "m21", collector_number: "272", released_at: "2020-07-03" },
  { id: alias, set: "m12", collector_number: "247", released_at: "2011-07-15" },
].map((entry) => ({
  ...entry, set_name: entry.set, border_color: "black", frame_effects: [], full_art: false,
  faces: [{ normal: `https://images.example/${entry.id}.jpg` }],
}));
const oracleEntry = {
  oracle_id: forestOracle, name: "Forest", face_names: ["forest"], layout: "normal",
  mana_cost: "", cmc: 0, type_line: "Basic Land — Forest", colors: [], color_identity: ["G"],
  keywords: [], faces: [{ normal: "/ordinary-forest.jpg", orientation: "portrait" }],
};
const list: SubmissionDeck = { main_deck: ["Forest", "Forest"], sideboard: [], commander: [] };
const plain: SubmissionArtSnapshot = { artChain: [], artOverrides: {} };
const viewerPin = { scryfallId: viewer, setCode: "tla", collectorNumber: "286" };
const installedPack = packId("curated");
const locations = new Map<string, string>();
const revisionListeners = new Set<(event: RevisionEvent) => void>();
let revision = installedRevision("0");
let revisionNumber = 0;
let beforeResolve: ((keys: ResolutionKey[]) => Promise<void>) | undefined;

const resolveAssets = vi.fn(async (keys: ResolutionKey[]): Promise<ResolutionResponse> => {
  await beforeResolve?.(keys);
  return {
    revision,
    entries: keys.map((key, ordinal) => {
      const url = locations.get(key.key);
      return {
        ordinal, key,
        matches: url ? [{
          packId: installedPack, assetKey: assetKey("asset:v1:exact_printing:QQ"),
          catalogRoot: catalogRoot("a".repeat(64)), url, media: "image/jpeg",
        }] : [],
      };
    }),
  };
});
const backend = {
  resolve: resolveAssets,
  async subscribeRevision(listener: (event: RevisionEvent) => void) {
    revisionListeners.add(listener);
    return () => { revisionListeners.delete(listener); };
  },
} as unknown as VisualPackBackend;
vi.mock("../../services/platform", async (original) => ({
  ...await original<typeof import("../../services/platform")>(),
  loadVisualPackBackend: async () => backend,
}));

function install(id: string, url: string) {
  for (const rung of ["small", "normal"] as const) {
    locations.set(encodeCandidateKey("english_printing", [id, 0, "full_card", rung]), url);
  }
}
function requestsExact(keys: ResolutionKey[], id: string) {
  return keys.some((key) => key.kind === "candidate" && decodeCandidateKey(key.key)[1][0] === id);
}
function exactRequested(id: string) {
  return resolveAssets.mock.calls.some(([keys]) => requestsExact(keys, id));
}
function response(input: string) {
  if (input.includes("scryfall-printings")) return Response.json({ [forestOracle]: entries });
  if (input.includes("scryfall-data")) return Response.json({ [forestOracle]: oracleEntry, forest: oracleEntry });
  if (input.includes("card-data.json")) return Response.json({ forest });
  return new Response(null, { status: 404 });
}

beforeAll(async () => {
  const bytes = await readFile(resolve(__dirname, "../../wasm/engine_wasm_bg.wasm"));
  const module = await WebAssembly.compile(bytes);
  expect((await init({ module_or_path: module })).memory).toBeInstanceOf(WebAssembly.Memory);
  vi.stubGlobal("Worker", class {
    constructor() { throw new Error("force the production main-thread fallback"); }
  });
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => response(String(input))));
  vi.stubGlobal("caches", { match: async (input: string) => response(input) });
  await getSharedAdapter().initialize();
}, 60_000);

beforeEach(() => {
  beforeResolve = undefined;
  locations.clear();
  resolveAssets.mockClear();
  useConnectivityStore.setState({ forcedOffline: true, browserOnline: true });
  usePreferencesStore.setState({
    language: "en", showOwnerPrintings: true, artChain: [], artOverrides: { [forestOracle]: viewerPin },
  });
  install(viewer, "/viewer.jpg");
  install(slotZero, "/slot-zero.jpg");
  install(slotOne, "/slot-one.jpg");
  install(alias, "/alias.jpg");
  install(newest, "/newest.jpg");
  revision = installedRevision(String(++revisionNumber));
  for (const listener of revisionListeners) {
    listener({ cause: "install", revision, operationId: null, catalogRoot: null });
  }
});

afterEach(() => {
  cleanup();
  clear_game_state();
});

async function initializeDeck(deck: SubmissionDeck) {
  const adapter = getSharedAdapter();
  await adapter.initializeGame({
    player: deck, opponent: { ...list, main_deck: Array<string>(12).fill("Forest") },
  }, FORMAT_DEFAULTS.Freeform, 2, undefined, 0);
  const { state, actions } = await adapter.getViewerSnapshot(0);
  expect(actions.some((action) => action.type === "MulliganDecision")).toBe(true);
  const cards = state.players[0].hand.map((id) => state.objects[id]);
  expect(cards).toHaveLength(2);
  for (const card of cards) {
    expect(card.name).toBe("Forest");
    expect(card.printed_ref?.oracle_id).toBe(forestOracle);
    expect(card.printing).toBeDefined();
    expect(card).toHaveProperty("base_printing", card.printing);
  }
  return cards;
}

function imageProps(card: GameObject) {
  return {
    oracleId: card.printed_ref?.oracle_id,
    faceName: card.printed_ref?.face_name,
    ownerPrintedRef: card.printed_ref,
    ownerPrinting: card.printing,
  };
}
function renderCard(card: GameObject) {
  const tile = render(<CardImage cardName={card.name} oracleText="({T}: Add {G}.)" {...imageProps(card)} />);
  return {
    img: () => tile.container.querySelector<HTMLImageElement>('img[alt="Forest"]'),
    fallback: () => within(tile.container).queryByTestId("card-art-fallback"),
    unmount: tile.unmount,
  };
}
async function expectArt(card: GameObject, url: string, asset: string) {
  const tile = renderCard(card);
  await waitFor(() => expect(tile.img()?.getAttribute("src")).toBe(url));
  expect(exactRequested(asset)).toBe(true);
  tile.unmount();
}
function printingRow(set_code: string, collector_number: string, count: number) {
  return { section: "main_deck" as const, name: "Forest", count, printing: { set_code, collector_number } };
}

it("submits plain and unresolved lists as vacancies and renders both catalog choices", async () => {
  for (const snapshot of [plain, {
    ...plain, artChain: [{ type: "set", setCode: "unresolved", label: "Unresolved" }],
    artOverrides: { [forestOracle]: { ...viewerPin, scryfallId: "stale" } },
  } satisfies SubmissionArtSnapshot]) {
    const submitted = await submitDeckList(list, snapshot);
    expect(submitted.printings).toBeUndefined();
    const cards = await initializeDeck(submitted);
    expect(cards.map((card) => card.printing?.collector_number).sort()).toEqual(["272", "273"]);
    for (const card of cards) {
      expect(card.printing?.automatic_art_catalog_version).toBe(1);
      const first = card.printing?.collector_number === "272";
      await expectArt(card, first ? "/slot-zero.jpg" : "/slot-one.jpg", first ? slotZero : slotOne);
    }
  }
});

it.each([
  { chain: { type: "newest" }, set: "ecl", collector: "273", id: newest, url: "/newest.jpg" },
  { chain: { type: "oldest" }, set: "m12", collector: "247", id: alias, url: "/alias.jpg" },
  { chain: { type: "set", setCode: "m21", label: "Core Set 2021" }, set: "m21", collector: "273", id: slotOne, url: "/slot-one.jpg" },
] satisfies { chain: SubmissionArtSnapshot["artChain"][number]; set: string; collector: string; id: string; url: string }[])(
  "renders the submitted $chain.type choice for every copy",
  async ({ chain, set, collector, id, url }) => {
    const row = printingRow(set, collector, 2);
    const submitted = await submitDeckList(list, { artOverrides: {}, artChain: [chain] });
    expect(submitted.printings).toEqual([row]);
    for (const card of await initializeDeck(submitted)) {
      expect(card.printing).toEqual(row.printing);
      await expectArt(card, url, id);
    }
  },
);

it("preserves pin, import and preference precedence through rendering", async () => {
  const preference: SubmissionArtSnapshot = { artOverrides: {}, artChain: [{ type: "newest" }] };
  const pin = { scryfallId: slotOne, setCode: "m21", collectorNumber: "273" };
  const stale = { ...pin, scryfallId: "stale" };
  for (const [override, imported, set, collector, id, url] of [
    [pin, "272", "m21", "273", slotOne, "/slot-one.jpg"],
    [stale, "272", "m21", "272", slotZero, "/slot-zero.jpg"],
    [stale, "999", "ecl", "273", newest, "/newest.jpg"],
  ] as const) {
    const parsed = {
      main: [{ name: "Forest", count: 2, sourcePrinting: { setCode: "m21", collectorNumber: imported } }],
      sideboard: [],
    };
    const deck = await submitParsedDeck(parsed, { ...preference, artOverrides: { [forestOracle]: override } });
    const row = printingRow(set, collector, 2);
    expect(deck.printings).toEqual([row]);
    for (const card of await initializeDeck(deck)) {
      expect(card.printing).toEqual(row.printing);
      await expectArt(card, url, id);
    }
  }
});

it.each([
  { set: "m21", collector: "272", id: slotZero, url: "/slot-zero.jpg" },
  { set: "m12", collector: "247", id: alias, url: "/alias.jpg" },
])("preserves an authoritative $set/$collector row and uses slot one for the vacancy", async ({ set, collector, id, url }) => {
  const row = printingRow(set, collector, 1);
  const input = { ...list, printings: [row] };
  const submitted = await submitDeckList(input, { ...plain, artChain: [{ type: "newest" }] });
  expect(submitted).toBe(input);
  const cards = await initializeDeck(submitted);
  const owner = cards.find((card) => card.printing?.automatic_art_catalog_version === undefined)!;
  const automatic = cards.find((card) => card.printing?.automatic_art_catalog_version === 1)!;
  expect(owner.printing).toEqual(row.printing);
  expect(automatic.printing).toEqual({ set_code: "m21", collector_number: "273", automatic_art_catalog_version: 1 });
  await expectArt(owner, url, id);
  await expectArt(automatic, "/slot-one.jpg", slotOne);
});

async function slotZeroCard(automatic: boolean) {
  const input = automatic ? list : { ...list, printings: [printingRow("m21", "272", 1)] };
  const submitted = await submitDeckList(input, plain);
  const expected: CardPrinting = {
    set_code: "m21", collector_number: "272", ...(automatic ? { automatic_art_catalog_version: 1 } : {}),
  };
  const card = (await initializeDeck(submitted)).find((card) => card.printing?.collector_number === "272")!;
  expect(card.printing).toEqual(expected);
  return card;
}

it.each([true, false])("handles missing admitted offline art, automatic=%s", async (automatic) => {
  const card = await slotZeroCard(automatic);
  locations.delete(encodeCandidateKey("english_printing", [slotZero, 0, "full_card", "small"]));
  locations.delete(encodeCandidateKey("english_printing", [slotZero, 0, "full_card", "normal"]));
  let release!: () => void;
  const pending = new Promise<void>((done) => { release = done; });
  beforeResolve = async (keys) => {
    if (requestsExact(keys, slotZero)) await pending;
  };
  usePreferencesStore.setState({ showOwnerPrintings: false });
  const tile = renderCard(card);
  const hook = renderHook(() => useCardImage(card.name, imageProps(card)));
  await waitFor(() => expect(tile.img()?.getAttribute("src")).toBe("/viewer.jpg"));
  act(() => usePreferencesStore.setState({ showOwnerPrintings: true }));
  try {
    await waitFor(() => expect(exactRequested(slotZero)).toBe(true));
    if (automatic) expect(tile.img()?.getAttribute("src")).toBe("/viewer.jpg");
  } finally {
    await act(async () => {
      release();
      const exactLookups = resolveAssets.mock.calls.flatMap(([keys], index) =>
        requestsExact(keys, slotZero) ? [resolveAssets.mock.results[index].value] : []);
      await Promise.all(exactLookups);
    });
  }
  await waitFor(() => {
    expect(hook.result.current).toMatchObject({ src: automatic ? "/viewer.jpg" : null, isLoading: false });
    if (automatic) expect(tile.img()?.getAttribute("src")).toBe("/viewer.jpg");
    else {
      expect(tile.img()).toBeNull();
      expect(tile.fallback()).toBeInTheDocument();
    }
  });
});

it.each([true, false])("uses the viewer toggle for delivered art, automatic=%s", async (automatic) => {
  const card = await slotZeroCard(automatic);
  usePreferencesStore.setState({ showOwnerPrintings: false });
  const tile = renderCard(card);
  await waitFor(() => expect(tile.img()?.getAttribute("src")).toBe("/viewer.jpg"));
  act(() => usePreferencesStore.setState({ showOwnerPrintings: true }));
  await waitFor(() => expect(tile.img()?.getAttribute("src")).toBe("/slot-zero.jpg"));
  expect(exactRequested(slotZero)).toBe(true);
  act(() => usePreferencesStore.setState({ showOwnerPrintings: false }));
  await waitFor(() => expect(tile.img()?.getAttribute("src")).toBe("/viewer.jpg"));
});
