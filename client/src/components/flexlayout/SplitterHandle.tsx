/** Shared visual for the row-band resize grabbers ({@link ZoneSplitter}).
 *  Renders a faint full-length boundary line plus a centered grip knob carrying
 *  a double-headed resize arrow, so the bar reads unmistakably as "drag me to
 *  resize". Orientation is parameterized for future reuse. */
export function SplitterHandle({ orientation }: { orientation: "horizontal" | "vertical" }) {
  // "horizontal" = a horizontal bar that resizes vertically (row bands) → ↕.
  // "vertical"   = a vertical bar that resizes horizontally (columns)    → ↔.
  const isRow = orientation === "horizontal";
  return (
    <>
      {/* Faint full-length boundary line so the whole edge reads as draggable. */}
      <span
        aria-hidden
        className={`absolute bg-sky-400/40 ${isRow ? "inset-x-0 h-0.5" : "inset-y-0 w-0.5"}`}
      />
      {/* Center grip knob with a double-headed resize arrow. */}
      <span
        aria-hidden
        className={`relative z-10 flex items-center justify-center rounded-full bg-sky-400 font-bold leading-none text-slate-950 shadow-[0_0_10px_2px_rgba(56,189,248,0.7)] ring-2 ring-sky-200/60 transition-transform group-hover:scale-110 ${
          isRow ? "h-5 w-11 text-sm" : "h-11 w-5 text-sm"
        }`}
      >
        {isRow ? "↕" : "↔"}
      </span>
    </>
  );
}
