import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { savePreconDeck } from "../preconDecks";
import type { DeckEntry } from "../../hooks/useDecks";
import { getDeckMeta, STORAGE_KEY_PREFIX, writeDraftAutosaveDeck } from "../../constants/storage";
import { installFifoWebLocks, resetSavedDeckLibraryForTests, uninstallWebLocks } from "../../test/helpers/webLocks";

beforeEach(async () => {
  localStorage.clear();
  installFifoWebLocks();
  await resetSavedDeckLibraryForTests();
});
afterEach(() => {
  uninstallWebLocks();
});

describe("savePreconDeck", () => {
  it("clears an autosave marker on the deck it overwrites", async () => {
    const result = await writeDraftAutosaveDeck("Sealed", "[Autosave] Sealed", "autosave-data");
    if (result.status === "skipped") throw new Error(`writeDraftAutosaveDeck skipped: ${result.reason}`);
    const name = result.value;
    const precon: DeckEntry = {
      name: "Precon", code: "SET", type: "Commander Deck", coveragePct: 100,
      mainBoard: [{ name: "Forest", count: 40 }],
      sideBoard: [],
      commander: undefined,
    };

    await savePreconDeck(name, precon, { type: "replace" });

    expect(getDeckMeta(name)?.autosaveSlot).toBeUndefined();
    const persisted = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + name) ?? "{}");
    expect(persisted.main).toEqual([{ name: "Forest", count: 40 }]);
  });
});
