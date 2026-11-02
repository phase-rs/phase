/**
 * A working `localStorage` for tests that exercise a persisted zustand store.
 *
 * Node 22 ships an experimental `localStorage` global that is present but
 * non-functional unless the process was started with a valid
 * `--localstorage-file`. Under the default happy-dom environment it shadows
 * everything else, so `zustand/persist`'s `setItem` throws
 * "storage.setItem is not a function" and any test that calls `setState` on a
 * persisted store fails before it asserts anything.
 *
 * Scoped to the tests that need it rather than installed globally: several
 * suites opt into `@vitest-environment jsdom` and patch `Storage.prototype` to
 * observe writes, and a global stand-in would make those patches blind.
 */
function installTestLocalStorage(): void {
  const ambient = (globalThis as { localStorage?: Partial<Storage> }).localStorage;
  if (typeof ambient?.setItem === "function") return;

  const entries = new Map<string, string>();
  const storage: Storage = {
    get length() {
      return entries.size;
    },
    clear: () => entries.clear(),
    getItem: (key: string) => entries.get(String(key)) ?? null,
    key: (index: number) => [...entries.keys()][index] ?? null,
    removeItem: (key: string) => {
      entries.delete(String(key));
    },
    setItem: (key: string, value: string) => {
      entries.set(String(key), String(value));
    },
  };

  Object.defineProperty(globalThis, "localStorage", {
    value: storage,
    configurable: true,
    writable: true,
  });
}

// Installed on import, not on call: `zustand/persist` resolves its storage once,
// when the store module is first evaluated. ES module imports are evaluated in
// source order, so a test that needs this must list its side-effect import
// BEFORE the store it exercises — by the time a test body runs, the store has
// already captured whatever storage existed at import time.
installTestLocalStorage();
