import { lazy, type ComponentType, type LazyExoticComponent } from "react";

const warned = new Set<string>();

/** Logs a degradation once per `key` so a stale deploy cannot flood the console. */
export function warnOnce(key: string, message: string, detail?: unknown): void {
  if (warned.has(key)) return;
  warned.add(key);
  console.warn(`[dice3d] ${message}`, detail ?? "");
}

/** Test seam: forgets which degradations were already logged. */
export function resetWarnOnce(): void {
  warned.clear();
}

/**
 * `React.lazy` for an optional enhancement that must never crash the app.
 *
 * After a deploy the old hashed chunk is gone. Vite's preload-error handling
 * (`vite:preloadError` + `preventDefault`) makes `import()` resolve `undefined`
 * rather than reject, and a stale or partial chunk can resolve without the
 * expected export. Plain `lazy(() => import(..).then(m => ({ default: m.X })))`
 * then throws a TypeError during render, which escapes to the app-level
 * ErrorBoundary, unmounts the game and lets a deferred reload fire. Here every
 * such outcome resolves to `Fallback` instead, and is logged once.
 */
export function safeLazy<P extends object, M>(
  name: string,
  load: () => Promise<M | undefined>,
  pick: (module: M) => ComponentType<P> | undefined,
  Fallback: ComponentType<P>,
): LazyExoticComponent<ComponentType<P>> {
  return lazy(async () => {
    try {
      const module = await load();
      const component = module === undefined || module === null ? undefined : pick(module);
      if (typeof component === "function" || (typeof component === "object" && component !== null)) {
        return { default: component };
      }
      warnOnce(name, `${name} chunk resolved without its component; using the static fallback.`);
    } catch (error) {
      warnOnce(name, `${name} chunk failed to load; using the static fallback.`, error);
    }
    return { default: Fallback };
  });
}
