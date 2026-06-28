import { useEffect, useRef, useState, type ReactNode } from "react";

interface GameplayTooltipProps {
  id?: string;
  children: ReactNode;
  className?: string;
}

export function GameplayTooltip({
  id,
  children,
  className,
}: GameplayTooltipProps) {
  const ref = useRef<HTMLSpanElement>(null);
  const [shift, setShift] = useState({ x: 0, y: 0 });
  // Tracks the shift currently applied so each measurement can recover the
  // tooltip's NATURAL position (rect − shift) and avoid a measure→shift→measure
  // feedback loop.
  const shiftRef = useRef({ x: 0, y: 0 });

  // The tooltip is shown purely via CSS `group-hover`; mirror that trigger in
  // JS so we can measure it only while visible and nudge it back inside the
  // viewport. Same 8px-margin clamp idiom as `BoardContextMenu`. Listening on
  // the closest `.group` ancestor — the element the CSS `group-hover` responds
  // to — keeps this self-contained with no call-site changes.
  useEffect(() => {
    const el = ref.current;
    const trigger = el?.closest(".group");
    if (!el || !trigger) return;

    const clamp = () => {
      const r = el.getBoundingClientRect();
      if (r.width === 0 && r.height === 0) return; // not actually visible yet
      const m = 8;
      const { x: sx, y: sy } = shiftRef.current;
      // Natural (un-shifted) edges.
      const left = r.left - sx;
      const right = r.right - sx;
      const top = r.top - sy;
      const bottom = r.bottom - sy;
      let x = 0;
      let y = 0;
      if (left < m) x = m - left;
      else if (right > window.innerWidth - m) x = window.innerWidth - m - right;
      if (top < m) y = m - top;
      else if (bottom > window.innerHeight - m) y = window.innerHeight - m - bottom;
      shiftRef.current = { x, y };
      setShift({ x, y });
    };
    const reset = () => {
      shiftRef.current = { x: 0, y: 0 };
      setShift({ x: 0, y: 0 });
    };

    trigger.addEventListener("pointerenter", clamp);
    trigger.addEventListener("focusin", clamp);
    trigger.addEventListener("pointerleave", reset);
    trigger.addEventListener("focusout", reset);
    return () => {
      trigger.removeEventListener("pointerenter", clamp);
      trigger.removeEventListener("focusin", clamp);
      trigger.removeEventListener("pointerleave", reset);
      trigger.removeEventListener("focusout", reset);
    };
  }, []);

  return (
    <span
      ref={ref}
      id={id}
      role="tooltip"
      className={[
        "pointer-events-none absolute right-0 bottom-full z-50 mb-2 hidden w-64 rounded-md border border-white/10 bg-slate-950/95 px-3 py-2 text-left text-[11px] leading-snug font-medium text-slate-100 shadow-2xl shadow-black/40 backdrop-blur-xl group-hover:block group-focus-visible:block",
        className,
      ]
        .filter(Boolean)
        .join(" ")}
      style={
        shift.x || shift.y
          ? { transform: `translate(${shift.x}px, ${shift.y}px)` }
          : undefined
      }
    >
      {children}
    </span>
  );
}
