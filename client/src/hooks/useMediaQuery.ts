import { useEffect, useState } from "react";

/**
 * True while the screen matches the CSS media `query`, following it as the
 * screen changes: a phone turning, a window resizing.
 */
export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() =>
    typeof window !== "undefined" ? window.matchMedia(query).matches : false,
  );

  useEffect(() => {
    const mediaQuery = window.matchMedia(query);
    const handleChange = () => setMatches(mediaQuery.matches);
    mediaQuery.addEventListener("change", handleChange);
    // Catch a change between the first render and this subscription, and pick
    // up a new `query`.
    handleChange();
    return () => mediaQuery.removeEventListener("change", handleChange);
  }, [query]);

  return matches;
}
