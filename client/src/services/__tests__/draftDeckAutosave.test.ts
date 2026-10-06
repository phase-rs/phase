import { afterEach, describe, expect, it, vi } from "vitest";

import { CUSTOM_CUBE_SET_CODE, DRAFT_KINDS } from "../../adapter/draftKinds";
import { listSavedDeckNames } from "../../constants/storage";
import {
  installFifoWebLocks,
  refusingWebLocks,
  resetSavedDeckLibraryForTests,
  uninstallWebLocks,
} from "../../test/helpers/webLocks";
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
});
