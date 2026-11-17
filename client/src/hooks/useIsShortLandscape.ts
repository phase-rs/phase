import { useEffect, useState } from "react";

/**
 * A phone on its side: the short screens that index.css's compact styles
 * target (`max-height: 500px`), in landscape. A short portrait window, such
 * as a narrow desktop sliver, does not match.
 */
export const SHORT_LANDSCAPE_QUERY = "(max-height: 500px) and (orientation: landscape)";

/**
 * True while the screen matches `SHORT_LANDSCAPE_QUERY`, following the phone
 * as it turns. For layouts that rearrange rather than shrink on a landscape
 * phone; `useIsCompactHeight` covers any short screen.
 */
export function useIsShortLandscape(): boolean {
  const [isShortLandscape, setIsShortLandscape] = useState(() =>
    typeof window !== "undefined"
      ? window.matchMedia(SHORT_LANDSCAPE_QUERY).matches
      : false,
  );

  useEffect(() => {
    const mediaQuery = window.matchMedia(SHORT_LANDSCAPE_QUERY);
    const handleChange = () => setIsShortLandscape(mediaQuery.matches);
    mediaQuery.addEventListener("change", handleChange);
    // Catch a turn between the first render and this subscription.
    handleChange();
    return () => mediaQuery.removeEventListener("change", handleChange);
  }, []);

  return isShortLandscape;
}
