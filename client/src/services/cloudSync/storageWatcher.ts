import { isUserOwnedStorageKey } from "../../constants/storage";

/**
 * Single chokepoint for "the user's portable profile changed in this tab".
 *
 * Writes to user-owned keys are scattered across ~9 call sites (deck builder,
 * feed service, import modal, game setup, precon loader) plus the Zustand
 * `persist` middleware for preferences — and every one of them ultimately calls
 * `localStorage.setItem`/`removeItem`. The same-tab `storage` event does NOT
 * fire for a tab's own writes, so we wrap those two methods once at boot and
 * notify on any user-owned-key change. This is the DRY, can't-miss-a-site
 * alternative to sprinkling markDirty() through every save path, and it
 * automatically excludes game state, draft blobs, and caches (their keys are
 * not user-owned per `isUserOwnedStorageKey`).
 *
 * IMPORTANT: we patch `Storage.prototype`, NOT the `localStorage` instance.
 * Storage objects have legacy named-property setter semantics: assigning
 * `localStorage.foo = x` is spec-defined to call `setItem("foo", String(x))`,
 * which means the natural-looking `localStorage.setItem = wrapper` silently
 * stores the stringified function under the key "setItem" and the real
 * `Storage.prototype.setItem` is never replaced — writes keep flowing through
 * the unwrapped method. Firefox has always enforced this; modern Chromium
 * does too. Patching the prototype (a regular object, exempt from NamedItem
 * semantics) is the only path that actually intercepts. The
 * `this === localStorage` guard keeps sessionStorage writes uninstrumented.
 *
 * Idempotent: a second install is a no-op. Returns an uninstaller.
 */
let installed = false;
let paused = false;

export function watchUserStorage(onDirty: (key: string) => void): () => void {
  if (installed) return () => {};
  installed = true;

  const origSet = Storage.prototype.setItem;
  const origRemove = Storage.prototype.removeItem;

  Storage.prototype.setItem = function (this: Storage, key: string, value: string) {
    origSet.call(this, key, value);
    if (this === localStorage && !paused && isUserOwnedStorageKey(key)) {
      onDirty(key);
    }
  };
  Storage.prototype.removeItem = function (this: Storage, key: string) {
    origRemove.call(this, key);
    if (this === localStorage && !paused && isUserOwnedStorageKey(key)) {
      onDirty(key);
    }
  };

  return () => {
    Storage.prototype.setItem = origSet;
    Storage.prototype.removeItem = origRemove;
    installed = false;
  };
}

/**
 * Suppress dirty notifications while applying a remote snapshot — `applyBackup`
 * writes the user-owned keys, which would otherwise re-mark the profile dirty
 * and schedule a redundant push of data we just pulled.
 */
export function withStorageWatchSuppressed(fn: () => void): void {
  paused = true;
  try {
    fn();
  } finally {
    paused = false;
  }
}
