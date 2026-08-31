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
 * Subscribers share one prototype wrapper, but each registration has its own
 * lifetime. Returns an idempotent uninstaller for this registration.
 */
interface StorageWatcherRegistration {
  onDirty: (key: string) => void;
}

const registrations = new Set<StorageWatcherRegistration>();
let originalSetItem: typeof Storage.prototype.setItem | null = null;
let originalRemoveItem: typeof Storage.prototype.removeItem | null = null;
let suppressionDepth = 0;

export function watchUserStorage(onDirty: (key: string) => void): () => void {
  const registration = { onDirty };
  registrations.add(registration);

  if (registrations.size === 1) {
    const nativeSetItem = Storage.prototype.setItem;
    const nativeRemoveItem = Storage.prototype.removeItem;
    originalSetItem = nativeSetItem;
    originalRemoveItem = nativeRemoveItem;

    Storage.prototype.setItem = function (this: Storage, key: string, value: string) {
      nativeSetItem.call(this, key, value);
      notifyUserStorageWrite(this, key);
    };
    Storage.prototype.removeItem = function (this: Storage, key: string) {
      nativeRemoveItem.call(this, key);
      notifyUserStorageWrite(this, key);
    };
  }

  let unsubscribed = false;
  return () => {
    if (unsubscribed) return;
    unsubscribed = true;
    registrations.delete(registration);
    if (registrations.size > 0) return;

    Storage.prototype.setItem = originalSetItem!;
    Storage.prototype.removeItem = originalRemoveItem!;
    originalSetItem = null;
    originalRemoveItem = null;
  };
}

function notifyUserStorageWrite(storage: Storage, key: string): void {
  if (
    storage !== localStorage ||
    suppressionDepth > 0 ||
    !isUserOwnedStorageKey(key)
  ) {
    return;
  }

  registrations.forEach(({ onDirty }) => {
    onDirty(key);
  });
}

/**
 * Suppress dirty notifications while applying a remote snapshot — `applyBackup`
 * writes the user-owned keys, which would otherwise re-mark the profile dirty
 * and schedule a redundant push of data we just pulled.
 */
export function withStorageWatchSuppressed(fn: () => void): void {
  suppressionDepth += 1;
  try {
    fn();
  } finally {
    suppressionDepth -= 1;
  }
}
