/**
 * Bell / bell-off glyph shared by the priority-yield surfaces (the per-trigger
 * control on a stack entry and the standing-yields summary chip). `muted` draws
 * the struck-through bell — the "auto-passing / silenced" state. Kept in one
 * place so both surfaces stay visually identical (CR 117.3d yields).
 */
export function YieldMuteIcon({ muted, className = "h-3.5 w-3.5 shrink-0" }: { muted: boolean; className?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      className={className}
      fill="none"
      stroke="currentColor"
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {muted ? (
        <>
          <path d="M8.7 3A6 6 0 0 1 18 8c0 2.6.5 4.4 1.1 5.7" />
          <path d="M17 17H3s3-2 3-9a4.8 4.8 0 0 1 .3-1.7" />
          <path d="M10.3 21a1.94 1.94 0 0 0 3.4 0" />
          <line x1="2" y1="2" x2="22" y2="22" />
        </>
      ) : (
        <>
          <path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" />
          <path d="M10.3 21a1.94 1.94 0 0 0 3.4 0" />
        </>
      )}
    </svg>
  );
}
