import { afterEach, describe, expect, it, vi } from "vitest";

import { CUSTOM_CUBE_SET_CODE, DRAFT_KINDS } from "../../adapter/draftKinds";
import { listSavedDeckNames, loadSavedDeck, STORAGE_KEY_PREFIX } from "../../constants/storage";
import {
  installFifoWebLocks,
  refusingWebLocks,
  resetSavedDeckLibraryForTests,
  uninstallWebLocks,
} from "../../test/helpers/webLocks";
import { migrateSavedDecks } from "../deckMigrations";
import { autosaveDraftDeck, draftAutosaveSlot, draftSubmissionToParsedDeck } from "../draftDeckAutosave";

describe("draftAutosaveSlot", () => {
  it("distinguishes a solo cube draft (kind Quick, custom-cube set code) from a solo Quick draft", () => {
    expect(draftAutosaveSlot("Quick", "TST")).toBe("Quick");
    expect(draftAutosaveSlot("Quick", CUSTOM_CUBE_SET_CODE)).toBe("Cube");
    expect(draftAutosaveSlot("Quick", null)).toBe("Quick");
  });

  it.each(DRAFT_KINDS.filter((kind) => kind !== "Quick"))(
    "maps every other draft kind (%s) to its own slot, regardless of set code",
    (kind) => {
      expect(draftAutosaveSlot(kind, "TST")).toBe(kind);
      expect(draftAutosaveSlot(kind, CUSTOM_CUBE_SET_CODE)).toBe(kind);
    },
  );
});

describe("draftSubmissionToParsedDeck", () => {
  it("removes one main-deck copy per commander and saves the partition's sideboard", () => {
    const partition = {
      mainDeck: ["Commander Card", "Commander Card", "Bolt", "Plains"],
      sideboard: ["Bear"],
    };
    const commanders = ["Commander Card"];

    const deck = draftSubmissionToParsedDeck(partition, commanders);

    expect(deck.main).toEqual(expect.arrayContaining([
      { name: "Commander Card", count: 1 },
      { name: "Bolt", count: 1 },
      { name: "Plains", count: 1 },
    ]));
    expect(deck.main).toHaveLength(3);
    expect(deck.sideboard).toEqual([{ name: "Bear", count: 1 }]);
    expect(deck.commander).toEqual(["Commander Card"]);
  });

  it("omits the commander field for a non-commander submission", () => {
    const deck = draftSubmissionToParsedDeck({ mainDeck: ["Bolt"], sideboard: [] }, []);
    expect(deck.commander).toBeUndefined();
    expect(deck.sideboard).toEqual([]);
  });
});

describe("autosaveDraftDeck", () => {
  afterEach(() => {
    uninstallWebLocks();
    localStorage.clear();
  });

  const submission = {
    view: { kind: "Sealed" as const, commanders_required: 0 },
    setCode: null,
    partition: { mainDeck: ["Bolt"], sideboard: [] },
    commanders: [],
  };

  it("resolves and warns without writing, when the lock is refused", async () => {
    Object.defineProperty(globalThis.navigator, "locks", {
      configurable: true,
      value: refusingWebLocks(new DOMException("nope", "InvalidStateError")),
    });
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    await expect(autosaveDraftDeck(submission)).resolves.toBeUndefined();
    expect(warnSpy).toHaveBeenCalledWith("[draftDeckAutosave] autosave skipped:", "lock-refused");
    expect(listSavedDeckNames()).toEqual([]);
    warnSpy.mockRestore();
  });

  it("paired positive: with the lock available, the same submission writes the deck", async () => {
    installFifoWebLocks();
    await resetSavedDeckLibraryForTests();
    await expect(autosaveDraftDeck(submission)).resolves.toBeUndefined();
    expect(listSavedDeckNames().length).toBeGreaterThan(0);
  });

  // CR 100.4b + CR 702.139a: a Limited companion is a pool card outside the main deck, so it is
  // saved in the sideboard and named by the designation. Reading the deck back and the boot
  // migration must both keep the designation and that single sideboard copy.
  it("keeps a Limited companion designation through autosave, load and the boot migration", async () => {
    installFifoWebLocks();
    await resetSavedDeckLibraryForTests();
    await autosaveDraftDeck({
      ...submission,
      partition: { mainDeck: ["Bolt"], sideboard: ["Lurrus of the Dream-Den", "Bear"] },
      companion: "Lurrus of the Dream-Den",
    });
    const [name] = listSavedDeckNames();
    expect(name).toBeDefined();
    const expectSurvives = () => {
      const loaded = loadSavedDeck(name);
      expect(loaded?.companion).toBe("Lurrus of the Dream-Den");
      expect(loaded?.sideboard).toEqual(expect.arrayContaining([
        { name: "Lurrus of the Dream-Den", count: 1 },
        { name: "Bear", count: 1 },
      ]));
      expect(loaded?.sideboard).toHaveLength(2);
    };

    expectSurvives();
    const stored = localStorage.getItem(STORAGE_KEY_PREFIX + name);
    migrateSavedDecks();
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + name)).toBe(stored);
    expectSurvives();
  });

  it("normalizes a padded Limited companion without duplicating its sideboard copy across boots", () => {
    const key = STORAGE_KEY_PREFIX + "Padded Limited Companion";
    localStorage.setItem(key, JSON.stringify({
      main: [{ count: 1, name: "Bolt" }],
      sideboard: [
        { count: 1, name: "Lurrus of the Dream-Den" },
        { count: 1, name: "Bear" },
      ],
      companion: " Lurrus of the Dream-Den ",
      format: "Limited",
    }));
    const expectSurvives = () => {
      const loaded = loadSavedDeck("Padded Limited Companion");
      expect(loaded?.companion).toBe("Lurrus of the Dream-Den");
      expect(loaded?.sideboard).toEqual([
        { count: 1, name: "Lurrus of the Dream-Den" },
        { count: 1, name: "Bear" },
      ]);
    };

    expectSurvives();
    migrateSavedDecks();
    const afterFirstBoot = localStorage.getItem(key);
    expect(JSON.parse(afterFirstBoot ?? "{}").companion).toBe("Lurrus of the Dream-Den");
    expectSurvives();
    migrateSavedDecks();
    migrateSavedDecks();
    expect(localStorage.getItem(key)).toBe(afterFirstBoot);
    expectSurvives();
  });

  // The commander-family netting removes the stale double-listed copy once. A record the
  // migration already netted must keep a second, legitimate copy across later boots.
  it("nets a legacy Commander Draft companion copy once, idempotently across boots", () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Legacy Commander Draft",
      JSON.stringify({
        main: [{ count: 1, name: "Bolt" }],
        sideboard: [{ count: 2, name: "Lurrus of the Dream-Den" }],
        commander: ["Kenrith, the Returned King"],
        companion: "Lurrus of the Dream-Den",
        format: "CommanderDraft",
      }),
    );
    const lurrusCopies = () =>
      loadSavedDeck("Legacy Commander Draft")?.sideboard
        .filter((entry) => entry.name === "Lurrus of the Dream-Den")
        .reduce((total, entry) => total + entry.count, 0);

    expect(lurrusCopies()).toBe(1);
    migrateSavedDecks();
    expect(lurrusCopies()).toBe(1);
    const afterFirstBoot = localStorage.getItem(STORAGE_KEY_PREFIX + "Legacy Commander Draft");
    migrateSavedDecks();
    migrateSavedDecks();
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Legacy Commander Draft")).toBe(afterFirstBoot);
    expect(lurrusCopies()).toBe(1);
    expect(loadSavedDeck("Legacy Commander Draft")?.companion).toBe("Lurrus of the Dream-Den");
  });

  it("autosaves a Commander Draft companion without double-listing it, so a reload keeps both pool copies", async () => {
    installFifoWebLocks();
    await resetSavedDeckLibraryForTests();
    await autosaveDraftDeck({
      view: { kind: "CommanderDraft" as const, commanders_required: 1 },
      setCode: null,
      partition: {
        mainDeck: ["Kenrith, the Returned King", "Bolt"],
        sideboard: ["Lurrus of the Dream-Den", "Lurrus of the Dream-Den"],
      },
      commanders: ["Kenrith, the Returned King"],
      companion: "Lurrus of the Dream-Den",
    });
    const [name] = listSavedDeckNames();
    migrateSavedDecks();
    migrateSavedDecks();
    const loaded = loadSavedDeck(name);
    expect(loaded?.companion).toBe("Lurrus of the Dream-Den");
    expect(loaded?.sideboard).toEqual([{ name: "Lurrus of the Dream-Den", count: 1 }]);
  });

  it("paired control: a non-Limited traditional format still clears the dedicated slot", () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Modern Companion",
      JSON.stringify({
        main: [{ count: 1, name: "Bolt" }],
        sideboard: [{ count: 1, name: "Lurrus of the Dream-Den" }],
        companion: "Lurrus of the Dream-Den",
        format: "Modern",
      }),
    );
    migrateSavedDecks();
    const migrated = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Modern Companion") ?? "{}");
    expect(migrated.companion).toBeUndefined();
    expect(loadSavedDeck("Modern Companion")?.companion).toBeUndefined();
  });
});
