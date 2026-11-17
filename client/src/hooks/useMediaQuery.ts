import { useCallback, useSyncExternalStore } from "react";

/**
 * True while the screen matches the CSS media `query`, following it as the
 * screen changes: a phone turning, a window resizing.
 *
 * `useSyncExternalStore` reads the query during render, so a new `query`
 * answers for itself on its first render, and React re-reads it after
 * subscribing, so a change before the subscription is not missed.
 */
export function useMediaQuery(query: string): boolean {
  const subscribe = useCallback(
    (onChange: () => void) => {
      const mediaQuery = window.matchMedia(query);
      mediaQuery.addEventListener("change", onChange);
      return () => mediaQuery.removeEventListener("change", onChange);
    },
    [query],
  );

  return useSyncExternalStore(
    subscribe,
    () => window.matchMedia(query).matches,
    () => false,
  );
}
