import { useState, useEffect } from "react";

const MOBILE_BREAKPOINT = 1024;

/**
 * True while the viewport is narrower than `breakpoint` (default 1024px = `lg`).
 * Pass a custom breakpoint to track a different layout boundary — e.g. the deck
 * builder passes 820px to match the shell rail's appearance, so its filter drawer
 * switches between overlay-sheet and inline-rail at the same point the rail does.
 */
export function useIsMobile(breakpoint: number = MOBILE_BREAKPOINT): boolean {
  const [isMobile, setIsMobile] = useState(
    typeof window !== "undefined" && window.innerWidth < breakpoint,
  );

  useEffect(() => {
    function handleResize() {
      setIsMobile(window.innerWidth < breakpoint);
    }
    handleResize();
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, [breakpoint]);

  return isMobile;
}
