import { useCallback, useSyncExternalStore } from "react";

function mediaQueryMatches(query: string): boolean {
  return typeof window !== "undefined"
    && typeof window.matchMedia === "function"
    && window.matchMedia(query).matches;
}

/** Subscribe to a CSS media query without duplicating viewport state in React. */
export function useMediaQuery(query: string): boolean {
  const subscribe = useCallback((onChange: () => void) => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
      return () => undefined;
    }

    const mediaQuery = window.matchMedia(query);
    mediaQuery.addEventListener("change", onChange);
    return () => mediaQuery.removeEventListener("change", onChange);
  }, [query]);
  const getSnapshot = useCallback(() => mediaQueryMatches(query), [query]);

  return useSyncExternalStore(subscribe, getSnapshot, () => false);
}
