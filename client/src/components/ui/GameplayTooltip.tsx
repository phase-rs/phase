import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";

interface GameplayTooltipProps {
  id?: string;
  children: ReactNode;
  className?: string;
  /**
   * What a touch tap on the trigger does. Touch has no hover, and the focus a
   * tap gives does not open the tooltip, so this is a touch player's only way
   * to read it.
   * - `"ignore"` (the default) is for a trigger whose tap acts, such as
   *   Resolve: the tap does its job and shows nothing.
   * - `"toggle"` is for a trigger that only explains something, such as a
   *   keyword's reminder text: a tap opens the tooltip, and the next tap, on
   *   the trigger or anywhere else, closes it.
   */
  touch?: "ignore" | "toggle";
}

/**
 * Hover/focus tooltip rendered in a portal on `document.body` so it escapes the
 * transformed / overflow-clipped stacking contexts of its trigger — most
 * importantly a battlefield card's rotating `motion.div`, which otherwise traps
 * the tooltip beneath the `z-[100]` hover card preview no matter how high its
 * own z-index. The trigger is the closest `.group` ancestor (the same trigger
 * the previous CSS `group-hover` implementation used, so call sites need no
 * changes). Hover over it and keyboard focus within it open the tooltip, and so
 * does a touch tap where `touch="toggle"` opts in. The fixed position is
 * measured from that trigger and clamped into the viewport with an 8px
 * margin. Non-positional `className` overrides (width, padding, text) still
 * apply; positional ones are superseded by the automatic viewport-aware
 * placement.
 */
export function GameplayTooltip({
  id,
  children,
  className,
  touch = "ignore",
}: GameplayTooltipProps) {
  const anchorRef = useRef<HTMLSpanElement>(null);
  const tipRef = useRef<HTMLSpanElement>(null);
  const triggerRef = useRef<Element | null>(null);
  const [visible, setVisible] = useState(false);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  // Open only when the player is pointing at the trigger or tabbing to it.
  //
  // Hover means a pointer other than touch moving over the trigger.
  // `pointerenter` alone is not hover: the browser also sends it when the
  // trigger appears under a pointer that has not moved, so a button mounting
  // under a resting pointer would open its tooltip with no one pointing at it.
  // A touch move is excluded too: a finger on the screen is a tap, not a hover.
  //
  // Focus opens it only when the browser marks that focus `:focus-visible`,
  // as it does for keyboard focus and not for the focus a tap or a click gives
  // a button. That focus outlives the gesture, so it would hold the tooltip
  // open after the finger lifts.
  //
  // A finger lifting also fires `pointerleave`. That ends a tap rather than
  // moving off the trigger, so only a hovering pointer's leave closes it.
  useEffect(() => {
    const trigger = anchorRef.current?.closest<HTMLElement>(".group") ?? null;
    triggerRef.current = trigger;
    if (!trigger) return;
    const showOnHover = (event: PointerEvent) => {
      if (event.pointerType !== "touch") setVisible(true);
    };
    const showOnKeyboardFocus = (event: FocusEvent) => {
      if (event.target instanceof Element && event.target.matches(":focus-visible")) {
        setVisible(true);
      }
    };
    const hideOnLeave = (event: PointerEvent) => {
      if (event.pointerType !== "touch") setVisible(false);
    };
    const hide = () => setVisible(false);
    trigger.addEventListener("pointermove", showOnHover);
    trigger.addEventListener("focusin", showOnKeyboardFocus);
    trigger.addEventListener("pointerleave", hideOnLeave);
    trigger.addEventListener("focusout", hide);
    return () => {
      trigger.removeEventListener("pointermove", showOnHover);
      trigger.removeEventListener("focusin", showOnKeyboardFocus);
      trigger.removeEventListener("pointerleave", hideOnLeave);
      trigger.removeEventListener("focusout", hide);
    };
  }, []);

  // A trigger that opts in with `touch="toggle"` opens on a touch tap and
  // closes on the next tap, on the trigger or anywhere else.
  useEffect(() => {
    if (touch !== "toggle") return;
    const trigger = anchorRef.current?.closest<HTMLElement>(".group") ?? null;
    if (!trigger) return;
    // A tap's own `click` does not report its pointer type reliably across
    // browsers, so the `pointerdown` that starts the tap records it.
    let tapPointerType: string | null = null;
    const notePointerType = (event: PointerEvent) => {
      tapPointerType = event.pointerType;
    };
    const toggleOnTap = () => {
      if (tapPointerType === "touch") setVisible((isVisible) => !isVisible);
      tapPointerType = null;
    };
    const hideOnOutsidePointer = (event: PointerEvent) => {
      if (event.target instanceof Node && trigger.contains(event.target)) return;
      setVisible(false);
    };
    trigger.addEventListener("pointerdown", notePointerType);
    trigger.addEventListener("click", toggleOnTap);
    // Capture, so a tap on a control that stops propagation still closes it.
    document.addEventListener("pointerdown", hideOnOutsidePointer, true);
    return () => {
      trigger.removeEventListener("pointerdown", notePointerType);
      trigger.removeEventListener("click", toggleOnTap);
      document.removeEventListener("pointerdown", hideOnOutsidePointer, true);
    };
  }, [touch]);

  // Once visible, place the tooltip above the trigger with right edges aligned,
  // flipping below when there's no room above and clamping horizontally so it
  // never leaves the viewport (8px margin). setPos returns the previous object
  // when the numbers are unchanged so a changing `children` identity can't spin
  // a measure→setState→measure loop.
  useLayoutEffect(() => {
    if (!visible) return;
    const trigger = triggerRef.current;
    const tip = tipRef.current;
    if (!trigger || !tip) return;
    const r = trigger.getBoundingClientRect();
    const m = 8;
    const w = tip.offsetWidth;
    const h = tip.offsetHeight;
    let left = r.right - w;
    if (left + w > window.innerWidth - m) left = window.innerWidth - m - w;
    if (left < m) left = m;
    let top = r.top - m - h;
    if (top < m) top = r.bottom + m;
    setPos((prev) =>
      prev && prev.left === left && prev.top === top ? prev : { left, top },
    );
  }, [visible, children]);

  const shown = visible && pos !== null;

  return (
    <>
      <span ref={anchorRef} className="hidden" aria-hidden />
      {createPortal(
        <span
          ref={tipRef}
          id={id}
          role="tooltip"
          style={{
            position: "fixed",
            left: pos?.left ?? 0,
            top: pos?.top ?? 0,
            visibility: shown ? "visible" : "hidden",
          }}
          className={[
            "pointer-events-none z-[130] w-64 rounded-[8px] border border-white/10 bg-slate-950 px-3 py-2 text-left text-[11px] leading-snug font-medium text-slate-100 shadow-xl shadow-black/40",
            className,
          ]
            .filter(Boolean)
            .join(" ")}
        >
          {children}
        </span>,
        document.body,
      )}
    </>
  );
}
