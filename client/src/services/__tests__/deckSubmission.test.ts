import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DeckPrinting, DeckSection } from "../../adapter/types";
import { STORAGE_KEY_PREFIX } from "../../constants/storage";
import { usePreferencesStore, type CardArtOverride } from "../../stores/preferencesStore";
import { captureSubmissionArtPreferences, submitDeckList, submitParsedDeck, type SubmissionDeck } from "../deckSubmission";
import { detectAndParseDeck, expandParsedDeck, type ParsedDeck } from "../deckParser";
import { pileSeatDeck } from "../pileSource";
import type { PrintingEntry } from "../scryfall";
import type * as Scryfall from "../scryfall";

const io = vi.hoisted(() => ({
  face: vi.fn(), printings: vi.fn(), bounds: vi.fn(), supply: vi.fn(),
}));
vi.mock("../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => ({ getCardFaceData: io.face, checkDeckPrintingsBounds: io.bounds }),
}));
vi.mock("../scryfall", async (importOriginal) => ({
  ...await importOriginal<typeof Scryfall>(),
  getCardPrintings: io.printings,
}));
vi.mock("../engineRuntime", () => ({ deckSupplyForFormat: io.supply, isCardCommanderEligible: vi.fn() }));

const A: PrintingEntry = {
  id: "a", set: "aaa", set_name: "A", collector_number: "001", released_at: "2000-01-01",
  border_color: "black", frame_effects: [], full_art: false, faces: [],
};
const B: PrintingEntry = { ...A, id: "b", set: "bbb", collector_number: "002", released_at: "2001-01-01" };
const C: PrintingEntry = { ...A, id: "c", set: "ccc", collector_number: "003", released_at: "2002-01-01" };
const D: PrintingEntry = { ...A, id: "d", set: "ddd", collector_number: "004", released_at: "2003-01-01" };
const CATALOG = [D, C, B, A];
const SECTIONS = ["main_deck", "sideboard", "commander", "companion", "attraction_deck", "planar_deck", "scheme_deck", "contraption_deck", "signature_spell"] as const;

function pin(printing: PrintingEntry): CardArtOverride {
  return { scryfallId: printing.id, setCode: "not-authoritative", collectorNumber: "not-authoritative" };
}
function row(section: DeckSection, name: string, count: number, printing: PrintingEntry): DeckPrinting {
  return { section, name, count, printing: { set_code: printing.set, collector_number: printing.collector_number } };
}
/** Separate entry contexts are valid ParsedDeck input, unlike normalized imports. */
function separateSources(): ParsedDeck {
  return {
    main: [
      { name: "Forest", count: 2, sourcePrinting: { setCode: "aaa", collectorNumber: "001" } },
      { name: "Forest", count: 3, sourcePrinting: { setCode: "bbb", collectorNumber: "002" } },
    ],
    sideboard: [],
  };
}
function held<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.resetAllMocks();
  usePreferencesStore.setState({ artChain: [], artOverrides: {} });
  io.face.mockImplementation(async (name: string) => ({ scryfall_oracle_id: name }));
  io.printings.mockResolvedValue(CATALOG);
  io.bounds.mockResolvedValue(undefined);
  io.supply.mockResolvedValue("HostPile");
  localStorage.clear();
});
afterEach(() => { vi.useRealTimers(); });

describe("submission selection and source contexts (V1)", () => {
  it("selects every copy in its own supplied entry context without changing names", async () => {
    const deck = separateSources();
    const result = await submitParsedDeck(deck);
    expect(result).toEqual({ ...expandParsedDeck(deck), printings: [row("main_deck", "Forest", 2, A), row("main_deck", "Forest", 3, B)] });
    expect(io.face).toHaveBeenCalledTimes(1);
    expect(io.printings).toHaveBeenCalledTimes(1);
    expect(deck.main[0].sourcePrinting).toEqual({ setCode: "aaa", collectorNumber: "001" });
  });

  it("honors a real single-source import and its owner's pin and chain", async () => {
    const deck = detectAndParseDeck("Deck\n2 Forest (AAA) 001");
    expect((await submitParsedDeck(deck)).printings).toEqual([row("main_deck", "Forest", 2, A)]);
    usePreferencesStore.setState({ artOverrides: { Forest: pin(C) } });
    expect((await submitParsedDeck(deck)).printings).toEqual([row("main_deck", "Forest", 2, C)]);
    usePreferencesStore.setState({ artOverrides: {}, artChain: [{ type: "newest" }] });
    expect((await submitParsedDeck(deck)).printings).toEqual([row("main_deck", "Forest", 2, D)]);
  });

  it("characterizes the inherited import limit: the first source owns all merged same-name copies", async () => {
    // d-15-10: this observes importer loss, not preservation of every imported choice.
    const deck = detectAndParseDeck("Deck\n2 Forest (AAA) 001\n3 Forest (BBB) 002");
    expect(deck.main).toEqual([{ name: "Forest", count: 5, sourcePrinting: { setCode: "aaa", collectorNumber: "001" } }]);
    expect((await submitParsedDeck(deck)).printings).toEqual([row("main_deck", "Forest", 5, A)]);
  });

  it("uses valid pins rather than the pin's descriptive set and collector fields", async () => {
    usePreferencesStore.setState({ artOverrides: { Forest: pin(C) } });
    expect((await submitParsedDeck(separateSources())).printings).toEqual([row("main_deck", "Forest", 5, C)]);
  });

  it("obeys chain ordering and only consults a source when the chain asks", async () => {
    usePreferencesStore.setState({ artChain: [{ type: "set", setCode: "ddd", label: "D" }, { type: "source_printing" }] });
    expect((await submitParsedDeck(separateSources())).printings).toEqual([row("main_deck", "Forest", 5, D)]);
    usePreferencesStore.setState({ artChain: [{ type: "source_printing" }, { type: "newest" }] });
    expect((await submitParsedDeck(separateSources())).printings).toEqual([row("main_deck", "Forest", 2, A), row("main_deck", "Forest", 3, B)]);
  });

  it("never substitutes canonical art for a stale pin or an unmatched chain", async () => {
    usePreferencesStore.setState({ artOverrides: { Forest: pin({ ...A, id: "gone" }) }, artChain: [{ type: "newest" }] });
    expect(await submitParsedDeck(separateSources())).not.toHaveProperty("printings");
    usePreferencesStore.setState({ artChain: [] });
    expect((await submitParsedDeck(separateSources())).printings).toEqual([row("main_deck", "Forest", 2, A), row("main_deck", "Forest", 3, B)]);
    usePreferencesStore.setState({ artOverrides: {}, artChain: [{ type: "set", setCode: "missing", label: "Missing" }] });
    expect(await submitParsedDeck(separateSources())).not.toHaveProperty("printings");
    usePreferencesStore.setState({ artChain: [{ type: "oldest" }] });
    expect((await submitParsedDeck(separateSources())).printings).toEqual([row("main_deck", "Forest", 5, A)]);
  });

  it("takes the names-only fast path but resolves any nonempty override map", async () => {
    const deck = { main_deck: ["Forest"], sideboard: [] };
    expect(await submitDeckList(deck)).toEqual(deck);
    expect(io.face).not.toHaveBeenCalled();
    expect(io.printings).not.toHaveBeenCalled();
    expect(io.bounds).not.toHaveBeenCalled();
    usePreferencesStore.setState({ artOverrides: { Forest: pin(B) } });
    expect((await submitDeckList(deck)).printings).toEqual([row("main_deck", "Forest", 1, B)]);
    expect(io.face).toHaveBeenCalledWith("Forest");
    io.face.mockClear();
    usePreferencesStore.setState({ artOverrides: { Unrelated: pin(A) } });
    expect(await submitDeckList(deck)).not.toHaveProperty("printings");
    expect(io.face).toHaveBeenCalledWith("Forest");
  });

  it("emits sparse choices, not ordinal placeholders or defaults", async () => {
    const deck: ParsedDeck = {
      main: [{ name: "Forest", count: 1 }, { name: "Forest", count: 1, sourcePrinting: { setCode: "aaa", collectorNumber: "001" } }],
      sideboard: [],
    };
    expect((await submitParsedDeck(deck)).printings).toEqual([row("main_deck", "Forest", 1, A)]);
    expect(await submitParsedDeck(detectAndParseDeck("Deck\n2 Forest"))).not.toHaveProperty("printings");
    expect((await submitParsedDeck(separateSources())).printings).toEqual([row("main_deck", "Forest", 2, A), row("main_deck", "Forest", 3, B)]);
  });

  it.each([{ printings: [] }, { printings: [row("main_deck", "Forest", 1, A)] }])("passes an existing sidecar through without preference or metadata reads: %j", async ({ printings }) => {
    usePreferencesStore.setState({ artOverrides: { Forest: pin(B) } });
    const deck = { main_deck: ["Forest"], sideboard: [], printings };
    const stateRead = vi.spyOn(usePreferencesStore, "getState");
    expect(await submitDeckList(deck)).toBe(deck);
    expect(stateRead).not.toHaveBeenCalled();
    stateRead.mockRestore();
    expect(io.face).not.toHaveBeenCalled();
    expect(io.bounds).not.toHaveBeenCalled();
    expect((await submitDeckList({ main_deck: ["Forest"], sideboard: [] })).printings).toEqual([row("main_deck", "Forest", 1, B)]);
  });
});

describe("adjacent coalescing and engine bounds (V1a)", () => {
  it.each(["source", "pin"])("coalesces 2049 identical basics after %s selection", async (selection) => {
    if (selection === "pin") usePreferencesStore.setState({ artOverrides: { Forest: pin(A) } });
    const deck = detectAndParseDeck(`Deck\n2049 Forest${selection === "source" ? " (AAA) 001" : ""}`);
    const result = await submitParsedDeck(deck);
    expect(result.main_deck).toEqual(Array(2049).fill("Forest"));
    expect(result.printings).toEqual([row("main_deck", "Forest", 2049, A)]);
    expect(io.bounds).toHaveBeenCalledWith([row("main_deck", "Forest", 2049, A)]);
  });

  it("preserves A/B/A order and only coalesces equal outcomes after source selection", async () => {
    const deck: ParsedDeck = {
      main: [A, B, A].map((printing) => ({
        name: "Forest", count: 1, sourcePrinting: { setCode: printing.set, collectorNumber: printing.collector_number },
      })),
      sideboard: [],
    };
    expect((await submitParsedDeck(deck)).printings).toEqual([row("main_deck", "Forest", 1, A), row("main_deck", "Forest", 1, B), row("main_deck", "Forest", 1, A)]);
    usePreferencesStore.setState({ artOverrides: { Forest: pin(C) } });
    expect((await submitParsedDeck(deck)).printings).toEqual([row("main_deck", "Forest", 3, C)]);
  });

  it("omits the whole candidate on engine rejection, not a prefix", async () => {
    io.bounds.mockRejectedValueOnce(new Error("engine resource bound"));
    expect(await submitParsedDeck(separateSources())).toEqual(expandParsedDeck(separateSources()));
    expect(io.bounds).toHaveBeenCalledWith([row("main_deck", "Forest", 2, A), row("main_deck", "Forest", 3, B)]);
    expect((await submitParsedDeck(separateSources())).printings).toEqual([row("main_deck", "Forest", 2, A), row("main_deck", "Forest", 3, B)]);
  });
});

describe("section and engine identity preservation (V2)", () => {
  it.each(SECTIONS)("annotates the submitted %s section without inferring a game zone", async (section) => {
    usePreferencesStore.setState({ artOverrides: { Forest: pin(C) } });
    const deck = { main_deck: [], sideboard: [], [section]: ["Forest", "Forest"], bracket_tier: "cedh", sticker_sheets: ["Forest"] };
    expect(await submitDeckList(deck)).toEqual({ ...deck, printings: [row(section, "Forest", 2, C)] });
    expect(deck).not.toHaveProperty("printings");
  });

  it("preserves all parsed sections and keeps main source out of string-only sections", async () => {
    const deck: ParsedDeck = {
      main: [{ name: "Forest", count: 2, sourcePrinting: { setCode: "aaa", collectorNumber: "001" } }],
      sideboard: [{ name: "Forest", count: 1, sourcePrinting: { setCode: "bbb", collectorNumber: "002" } }],
      commander: ["Forest"], companion: "Forest", planar_deck: ["Forest"], scheme_deck: ["Forest"],
      signature_spell: ["Forest"], sticker_sheets: ["Forest"],
    };
    expect(await submitParsedDeck(deck)).toEqual({ ...expandParsedDeck(deck), printings: [row("main_deck", "Forest", 2, A), row("sideboard", "Forest", 1, B)] });
    usePreferencesStore.setState({ artOverrides: { Forest: pin(C) } });
    expect((await submitParsedDeck(deck)).printings).toEqual([
      row("main_deck", "Forest", 2, C), row("sideboard", "Forest", 1, C),
      ...["commander", "companion", "planar_deck", "scheme_deck", "signature_spell"].map((section) => row(section as DeckSection, "Forest", 1, C)),
    ]);
  });

  it("does not annotate sticker sheets, even when they name a resolvable pinned card", async () => {
    usePreferencesStore.setState({ artOverrides: { Forest: pin(A) } });
    expect(await submitDeckList({ main_deck: [], sideboard: [], sticker_sheets: ["Forest"] })).toEqual({ main_deck: [], sideboard: [], sticker_sheets: ["Forest"] });
    expect(io.face).not.toHaveBeenCalled();
    expect((await submitDeckList({ main_deck: ["Forest"], sideboard: [], sticker_sheets: ["Forest"] })).printings).toEqual([row("main_deck", "Forest", 1, A)]);
  });

  it("resolves qualified homonyms through exact engine face identity", async () => {
    const names = ["Twin [oracle-a]", "Twin [oracle-b]"];
    io.face.mockImplementation(async (name: string) => ({ scryfall_oracle_id: name === names[0] ? "oracle-a" : name === names[1] ? "oracle-b" : "wrong-oracle" }));
    usePreferencesStore.setState({ artOverrides: { "oracle-a": pin(A), "oracle-b": pin(B), "wrong-oracle": pin(C) } });
    const result = await submitDeckList({ main_deck: names, sideboard: [] });
    expect(result.main_deck).toEqual(names);
    expect(result.printings).toEqual([row("main_deck", names[0], 1, A), row("main_deck", names[1], 1, B)]);
    expect(io.face.mock.calls).toEqual(names.map((name) => [name]));
  });

  it.each([null, {}, { scryfall_oracle_id: "" }, { scryfall_oracle_id: 7 }])("omits unavailable engine identity %j alongside a live sourced sibling", async (face) => {
    io.face.mockResolvedValueOnce(face);
    const result = await submitParsedDeck(detectAndParseDeck("Deck\n1 Unknown (AAA) 001\n1 Forest (AAA) 001"));
    expect(result.printings).toEqual([row("main_deck", "Forest", 1, A)]);
    expect(result.main_deck).toEqual(["Unknown", "Forest"]);
  });

  it("omits failed metadata without failing the submission or losing resolved siblings", async () => {
    io.printings.mockRejectedValueOnce(new Error("offline"));
    const result = await submitParsedDeck(detectAndParseDeck("Deck\n1 Island (AAA) 001\n1 Forest (BBB) 002"));
    expect(result.printings).toEqual([row("main_deck", "Forest", 1, B)]);
    expect(result.main_deck).toEqual(["Island", "Forest"]);
  });
});

describe("frozen inputs and bounded preparation (V3)", () => {
  it("deep-copies nested preferences once for a batch before metadata waits", async () => {
    const chain = { type: "set" as const, setCode: "aaa", label: "A" };
    const override = pin(A);
    usePreferencesStore.setState({ artChain: [chain], artOverrides: { Forest: override } });
    const snapshot = captureSubmissionArtPreferences();
    const metadata = held<PrintingEntry[]>();
    io.printings.mockReturnValue(metadata.promise);
    const first = submitDeckList({ main_deck: ["Forest", "Island"], sideboard: [] }, snapshot);
    await vi.waitFor(() => expect(io.printings).toHaveBeenCalledTimes(2));
    chain.setCode = "bbb";
    override.scryfallId = "b";
    usePreferencesStore.setState({ artChain: [{ type: "newest" }], artOverrides: { Forest: pin(C) } });
    const second = submitDeckList({ main_deck: ["Forest", "Island"], sideboard: [] }, snapshot);
    metadata.resolve(CATALOG);
    const expected = [row("main_deck", "Forest", 1, A), row("main_deck", "Island", 1, A)];
    expect((await first).printings).toEqual(expected);
    expect((await second).printings).toEqual(expected);
    expect((await submitDeckList({ main_deck: ["Forest", "Island"], sideboard: [] })).printings).toEqual([row("main_deck", "Forest", 1, C), row("main_deck", "Island", 1, D)]);
  });

  it.each(["mutate source", "replace source"])("captures entry values before await: %s", async (mutation) => {
    const deck = separateSources();
    const metadata = held<PrintingEntry[]>();
    io.printings.mockReturnValue(metadata.promise);
    const submission = submitParsedDeck(deck);
    await vi.waitFor(() => expect(io.printings).toHaveBeenCalled());
    if (mutation === "mutate source") {
      deck.main[0].sourcePrinting!.setCode = "bbb";
      deck.main[0].sourcePrinting!.collectorNumber = "002";
    } else {
      deck.main[0].sourcePrinting = { setCode: "bbb", collectorNumber: "002" };
    }
    metadata.resolve(CATALOG);
    expect((await submission).printings).toEqual([row("main_deck", "Forest", 2, A), row("main_deck", "Forest", 3, B)]);
    expect((await submitParsedDeck(deck)).printings).toEqual([row("main_deck", "Forest", 5, B)]);
  });

  it.each(["parsed", "list"])("copies present optional arrays before await for %s input", async (kind) => {
    usePreferencesStore.setState({ artOverrides: { Forest: pin(A), Island: pin(B) } });
    const commander = ["Forest"];
    const stickers = ["old sheet"];
    const parsed: ParsedDeck = { main: [], sideboard: [], commander, sticker_sheets: stickers };
    const list = { main_deck: [], sideboard: [], commander, sticker_sheets: stickers, bracket_tier: "core" };
    const metadata = held<PrintingEntry[]>();
    io.printings.mockReturnValue(metadata.promise);
    const submission = kind === "parsed" ? submitParsedDeck(parsed) : submitDeckList(list);
    await vi.waitFor(() => expect(io.printings).toHaveBeenCalled());
    commander[0] = "Island";
    stickers[0] = "new sheet";
    metadata.resolve(CATALOG);
    const result = await submission;
    expect(result.commander).toEqual(["Forest"]);
    expect(result.sticker_sheets).toEqual(["old sheet"]);
    expect(result.printings).toEqual([row("commander", "Forest", 1, A)]);
    const later = kind === "parsed" ? await submitParsedDeck(parsed) : await submitDeckList(list);
    expect(later.commander).toEqual(["Island"]);
    expect(later.sticker_sheets).toEqual(["new sheet"]);
    expect(later.printings).toEqual([row("commander", "Island", 1, B)]);
  });

  it.each(["identity", "catalog", "bounds"])("returns by 5000 ms while %s is held and never patches a returned deck", async (stage) => {
    vi.useFakeTimers();
    const pending = held<never>();
    if (stage === "identity") io.face.mockReturnValueOnce(pending.promise);
    if (stage === "catalog") io.printings.mockReturnValueOnce(pending.promise);
    if (stage === "bounds") io.bounds.mockReturnValueOnce(pending.promise);
    let result: SubmissionDeck | undefined;
    const promise = submitParsedDeck(separateSources()).then((deck) => { result = deck; return deck; });
    await vi.advanceTimersByTimeAsync(4999);
    expect(result).toBeUndefined();
    await vi.advanceTimersByTimeAsync(1);
    expect(result).toEqual(expandParsedDeck(separateSources()));
    const returned = await promise;
    const bytes = JSON.stringify(returned);
    // The real value type depends on the deliberately held I/O boundary.
    const release = pending.resolve as (value: unknown) => void;
    release(stage === "identity" ? { scryfall_oracle_id: "Forest" } : stage === "catalog" ? CATALOG : undefined);
    await vi.advanceTimersByTimeAsync(10000);
    expect(JSON.stringify(returned)).toBe(bytes);
    expect(vi.getTimerCount()).toBe(0);
    expect((await submitParsedDeck(separateSources())).printings).toEqual([row("main_deck", "Forest", 2, A), row("main_deck", "Forest", 3, B)]);
  });

  it("omits resolved siblings too when another lookup consumes the whole deadline", async () => {
    vi.useFakeTimers();
    const catalog = held<PrintingEntry[]>();
    io.printings.mockImplementation((oracleId: string) => oracleId === "Island" ? catalog.promise : Promise.resolve(CATALOG));
    const deck = detectAndParseDeck("Deck\n1 Forest (AAA) 001\n1 Island (BBB) 002");
    const submission = submitParsedDeck(deck);
    await vi.advanceTimersByTimeAsync(5000);
    const returned = await submission;
    expect(returned).toEqual(expandParsedDeck(deck));
    expect(io.bounds).not.toHaveBeenCalled();
    catalog.resolve(CATALOG);
    await vi.advanceTimersByTimeAsync(0);
    expect(returned).not.toHaveProperty("printings");
    expect((await submitParsedDeck(deck)).printings).toEqual([row("main_deck", "Forest", 1, A), row("main_deck", "Island", 1, B)]);
  });

  it("shares the deadline with the final bounds check instead of granting another 5000 ms", async () => {
    vi.useFakeTimers();
    const catalog = held<PrintingEntry[]>();
    const bounds = held<void>();
    io.printings.mockReturnValueOnce(catalog.promise);
    io.bounds.mockReturnValueOnce(bounds.promise);
    let result: SubmissionDeck | undefined;
    const submission = submitParsedDeck(separateSources()).then((deck) => { result = deck; return deck; });
    await vi.advanceTimersByTimeAsync(4900);
    catalog.resolve(CATALOG);
    await vi.advanceTimersByTimeAsync(0);
    expect(io.bounds).toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(99);
    expect(result).toBeUndefined();
    await vi.advanceTimersByTimeAsync(1);
    expect(result).toEqual(expandParsedDeck(separateSources()));
    bounds.resolve();
    await vi.advanceTimersByTimeAsync(0);
    expect(await submission).not.toHaveProperty("printings");
    expect((await submitParsedDeck(separateSources())).printings).toEqual([row("main_deck", "Forest", 2, A), row("main_deck", "Forest", 3, B)]);
  });

  it("retains valid annotations when metadata and validation finish just before the deadline", async () => {
    vi.useFakeTimers();
    const catalog = held<PrintingEntry[]>();
    io.printings.mockReturnValueOnce(catalog.promise);
    const submission = submitParsedDeck(separateSources());
    await vi.advanceTimersByTimeAsync(4999);
    catalog.resolve(CATALOG);
    await vi.advanceTimersByTimeAsync(0);
    expect((await submission).printings).toEqual([row("main_deck", "Forest", 2, A), row("main_deck", "Forest", 3, B)]);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("decorates a saved HostPile with the supplied batch snapshot but leaves default and fixed seats empty", async () => {
    localStorage.setItem(`${STORAGE_KEY_PREFIX}Pile`, JSON.stringify(separateSources()));
    usePreferencesStore.setState({ artOverrides: { Forest: pin(A) } });
    const snapshot = captureSubmissionArtPreferences();
    usePreferencesStore.setState({ artOverrides: { Forest: pin(B) } });
    expect((await pileSeatDeck("Dandan", { type: "SavedDeck", name: "Pile" }, snapshot))?.printings).toEqual([row("main_deck", "Forest", 5, A)]);
    io.face.mockClear();
    expect((await pileSeatDeck("Dandan", { type: "Default" }))?.main_deck).toEqual([]);
    io.supply.mockResolvedValueOnce("EngineFixed");
    expect((await pileSeatDeck("Momir", { type: "SavedDeck", name: "Pile" }))?.main_deck).toEqual([]);
    expect(io.face).not.toHaveBeenCalled();
    expect(await pileSeatDeck("Dandan", { type: "SavedDeck", name: "missing" })).toBeNull();
    expect((await pileSeatDeck("Dandan", { type: "SavedDeck", name: "Pile" }))?.printings).toEqual([row("main_deck", "Forest", 5, B)]);
  });
});
