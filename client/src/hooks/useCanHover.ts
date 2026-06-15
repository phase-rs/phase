import { useEffect, useState } from "react";

// `(any-hover: hover)` — true when ANY available input can hover, not just the
// primary one. `(hover: hover)` reports only the primary pointer, so a tablet
// whose primary pointer is touch returns false even with a mouse attached; that
// suppressed mouse-hover card previews on battlefield/zone cards (which gate
// their onMouseEnter handlers on this) while a mouse was plugged in. `any-hover`
// keeps pure-touch devices at false (long-press preview only) while enabling
// real mouse hover on touch-primary devices that also have a pointer.
const HOVER_QUERY = "(any-hover: hover)";

export function useCanHover(): boolean {
  const [canHover, setCanHover] = useState(() =>
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia(HOVER_QUERY).matches,
  );

  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return undefined;
    const media = window.matchMedia(HOVER_QUERY);
    const update = () => setCanHover(media.matches);
    update();
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, []);

  return canHover;
}
