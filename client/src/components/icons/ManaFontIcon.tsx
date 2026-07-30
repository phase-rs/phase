import { useSyncExternalStore, type CSSProperties } from "react";

/**
 * mana-font (Andrew Gioia) icon renderer. Wraps an `<i class="ms …">` glyph
 * from the "Mana" webfont declared in `mana-font/css/mana.css` (imported once in
 * `main.tsx`). Callers pass the mana-font class(es) — e.g. `"ms-ability-flying"`
 * or `"ms-loyalty-up ms-loyalty-2"` — plus a `fallbackText` that renders as
 * plain text until the webfont is ready (and forever, in a non-browser env).
 */

/** Maps the icon size to a Tailwind text-size class (mana-font glyphs scale
 * with `font-size`). Omit `size` to inherit the parent's font size — the right
 * choice inside already-sized rows like the keyword strip. */
const SIZE_CLASS: Record<NonNullable<ManaFontIconProps["size"]>, string> = {
  xs: "text-xs",
  sm: "text-sm",
  md: "text-base",
  lg: "text-lg",
};

// ── Shared font-readiness gate (the ONLY place FOUC is handled) ──────────────
// A single module-level load of the "Mana" family, shared across every icon
// instance via a useSyncExternalStore-compatible store. Until the font resolves
// we render `fallbackText`, so no glyph flashes as a tofu box on first paint.
let manaFontReady = false;
let loadStarted = false;
const readyListeners = new Set<() => void>();

function markReady(): void {
  manaFontReady = true;
  for (const listener of readyListeners) listener();
}

function startManaFontLoad(): void {
  if (loadStarted) return;
  loadStarted = true;
  // Non-browser env (SSR/tests without a DOM) or a browser lacking the Font
  // Loading API: treat as ready so consumers fall straight through to the glyph
  // path rather than being pinned on fallback text forever.
  if (typeof document === "undefined" || !("fonts" in document)) {
    manaFontReady = true;
    return;
  }
  document.fonts.load("1em Mana").then(markReady, markReady);
}

function subscribe(listener: () => void): () => void {
  readyListeners.add(listener);
  startManaFontLoad();
  return () => {
    readyListeners.delete(listener);
  };
}

/**
 * `true` once the "Mana" webfont has loaded (or immediately in a non-browser
 * env). Kicks off the shared one-time load on first subscription.
 */
export function useManaFontReady(): boolean {
  return useSyncExternalStore(
    subscribe,
    () => manaFontReady,
    () => false,
  );
}

interface ManaFontIconProps {
  /** mana-font class(es), space-separated (e.g. `"ms-loyalty-up ms-loyalty-2"`). */
  iconClass: string;
  /** Icon size; omit to inherit the parent's font size. */
  size?: "xs" | "sm" | "md" | "lg";
  /** When set, the glyph is exposed as an image with this accessible name;
   * otherwise it is decorative (`aria-hidden`). */
  label?: string;
  /** Plain-text shown until the webfont is ready (and in non-browser envs). */
  fallbackText: string;
  className?: string;
  /** Inline style, e.g. a card-relative `fontSize` clamp or a counter-rotation
   * that can't be expressed as a static Tailwind class. Applied to both the
   * glyph and its pre-load fallback so sizing stays stable across the swap. */
  style?: CSSProperties;
}

export function ManaFontIcon({
  iconClass,
  size,
  label,
  fallbackText,
  className,
  style,
}: ManaFontIconProps) {
  const ready = useManaFontReady();
  const sizeClass = size ? SIZE_CLASS[size] : "";
  const a11y = label
    ? ({ role: "img", "aria-label": label } as const)
    : ({ "aria-hidden": true } as const);

  if (!ready) {
    return (
      <span
        className={[sizeClass, className].filter(Boolean).join(" ")}
        style={style}
        {...a11y}
      >
        {fallbackText}
      </span>
    );
  }

  return (
    <i
      className={["ms", iconClass, sizeClass, className].filter(Boolean).join(" ")}
      style={style}
      {...a11y}
    />
  );
}
