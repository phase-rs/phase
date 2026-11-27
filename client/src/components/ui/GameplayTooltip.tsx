import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";

import { OWNS_TAP_ATTRIBUTE } from "../../hooks/useLongPress.ts";

/** What a touch tap on a tooltip's trigger does; see `GameplayTooltipProps.touch`. */
export type TooltipTouch = "ignore" | "toggle";

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
   *
   * Only a finger's tap toggles. A pen counts as a hovering pointer, as in
   * `useCardHover` and `useInspectHoverProps`, so a pen that cannot hover shows
   * the tooltip from its first move on the trigger until it lifts.
   */
  touch?: TooltipTouch;
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
  // Hover and keyboard focus drive `visible`; a touch tap drives `tapOpen`. A
  // pointer leaving cannot close a tap's tooltip and focus cannot flip it;
  // focus leaving the trigger closes both.
  const [visible, setVisible] = useState(false);
  const [tapOpen, setTapOpen] = useState(false);
  const open = visible || tapOpen;
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
    const hide = () => setVisible(false);
    trigger.addEventListener("pointermove", showOnHover);
    trigger.addEventListener("focusin", showOnKeyboardFocus);
    trigger.addEventListener("pointerleave", hide);
    trigger.addEventListener("focusout", hide);
    return () => {
      trigger.removeEventListener("pointermove", showOnHover);
      trigger.removeEventListener("focusin", showOnKeyboardFocus);
      trigger.removeEventListener("pointerleave", hide);
      trigger.removeEventListener("focusout", hide);
    };
  }, []);

  // A trigger that opts in with `touch="toggle"` opens on a touch tap and
  // closes on the next tap on it. Focus leaving it closes it too.
  //
  // Many of these triggers sit inside a card whose long press opens its
  // preview (`useLongPress`), and that hook captures the pointer, which would
  // send the tap's `click` to the card instead of here. Marking the trigger as
  // owning its taps keeps the pointer uncaptured for a press that starts on
  // it. The long press still works, and those hosts swallow the click that
  // ends a long press before it reaches the trigger (`useInspectHoverProps`,
  // `DialogAttachmentCard`), so that press only opens the preview.
  useEffect(() => {
    if (touch !== "toggle") return;
    const trigger = anchorRef.current?.closest<HTMLElement>(".group") ?? null;
    if (!trigger) return;
    trigger.setAttribute(OWNS_TAP_ATTRIBUTE, "");
    // A tap's own `click` does not report its pointer type reliably across
    // browsers, so the `pointerdown` that starts the tap records it. A touch
    // that turns into a scroll ends in `pointercancel` with no `click`.
    let tapPointerType: string | null = null;
    const notePointerType = (event: PointerEvent) => {
      tapPointerType = event.pointerType;
    };
    const forgetPointerType = () => {
      tapPointerType = null;
    };
    const toggleOnTap = () => {
      if (tapPointerType === "touch") setTapOpen((isOpen) => !isOpen);
      tapPointerType = null;
    };
    const closeTap = () => setTapOpen(false);
    trigger.addEventListener("pointerdown", notePointerType);
    trigger.addEventListener("pointercancel", forgetPointerType);
    trigger.addEventListener("click", toggleOnTap);
    trigger.addEventListener("focusout", closeTap);
    return () => {
      trigger.removeAttribute(OWNS_TAP_ATTRIBUTE);
      trigger.removeEventListener("pointerdown", notePointerType);
      trigger.removeEventListener("pointercancel", forgetPointerType);
      trigger.removeEventListener("click", toggleOnTap);
      trigger.removeEventListener("focusout", closeTap);
      // A trigger that stops opting in (an emblem chip turning activatable)
      // must not keep a tap's tooltip open.
      setTapOpen(false);
    };
  }, [touch]);

  // While a tap holds it open, a tap anywhere else closes it. The listener is
  // on `document` in the capture phase, so a tap on a control that stops
  // propagation still closes it.
  useEffect(() => {
    if (!tapOpen) return;
    const closeOnOutsidePointer = (event: PointerEvent) => {
      const trigger = triggerRef.current;
      if (event.target instanceof Node && trigger?.contains(event.target)) return;
      setTapOpen(false);
    };
    document.addEventListener("pointerdown", closeOnOutsidePointer, true);
    return () => document.removeEventListener("pointerdown", closeOnOutsidePointer, true);
  }, [tapOpen]);

  // Once visible, place the tooltip above the trigger with right edges aligned,
  // flipping below when there's no room above and clamping horizontally so it
  // never leaves the viewport (8px margin). setPos returns the previous object
  // when the numbers are unchanged so a changing `children` identity can't spin
  // a measure→setState→measure loop.
  useLayoutEffect(() => {
    if (!open) return;
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
  }, [open, children]);

  const shown = open && pos !== null;

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
