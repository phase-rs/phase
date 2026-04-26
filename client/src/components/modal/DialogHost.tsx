import { useEffect, useMemo, useState } from "react";
import { motion, useReducedMotion } from "framer-motion";
import type { ReactNode } from "react";

import type { WaitingFor } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { DialogPeekCtx, type DialogPeekContext } from "./dialogPeekContext.ts";

// `WaitingFor` variants that do NOT render a centered dialog/overlay.
// Board-level interactions (Priority, combat declarations) and pre-game
// flows render inline on the board rather than as a centered modal.
const NON_DIALOG_WAITING_FOR_TYPES = new Set<string>([
  "Priority",
  "DeclareAttackers",
  "DeclareBlockers",
  "AssignCombatDamage",
  "MulliganDecision",
  "MulliganBottomCards",
  "ChoosePlayDraw",
  "BetweenGamesSideboard",
  "BetweenGamesChoosePlayDraw",
  "GameOver",
]);

// `WaitingFor` variants whose UI deliberately uses `pointer-events: none` so
// the player can click cards on the battlefield to pick targets. The host
// MUST stay out of the way for these — wrapping them in a viewport-sized
// `fixed inset-0` host would intercept clicks before they reach the board.
// These dialogs also don't surface a peek button (the overlay is already
// translucent and click-through), so peek isn't relevant for them.
const CLICK_THROUGH_WAITING_FOR_TYPES = new Set<string>([
  "TargetSelection",
  "TriggerTargetSelection",
  "CopyTargetChoice",
  "ExploreChoice",
  "TapCreaturesForManaAbility",
  "TapCreaturesForSpellCost",
]);

function isDialogVisibleFor(waitingFor: WaitingFor | null | undefined): boolean {
  if (!waitingFor) return false;
  return !NON_DIALOG_WAITING_FOR_TYPES.has(waitingFor.type);
}

function isClickThroughDialog(waitingFor: WaitingFor | null | undefined): boolean {
  if (!waitingFor) return false;
  return CLICK_THROUGH_WAITING_FOR_TYPES.has(waitingFor.type);
}

export function DialogHost({ children }: { children: ReactNode }) {
  const waitingFor = useGameStore((s) => s.waitingFor);
  const [peeked, setPeeked] = useState(false);
  const shouldReduceMotion = useReducedMotion();

  // CR display contract: every new engine prompt must be visible. When the
  // WaitingFor reference changes (the store emits a new object on every
  // engine update), reset peek so the player isn't shown a hidden dialog.
  useEffect(() => {
    setPeeked(false);
  }, [waitingFor]);

  const dialogVisible = isDialogVisibleFor(waitingFor);
  const clickThrough = isClickThroughDialog(waitingFor);
  // Only wrap when there's a centered dialog that benefits from being
  // anchored to the viewport. Click-through overlays (TargetingOverlay)
  // must NOT be wrapped — the host would intercept board clicks at z-40
  // and break target picking.
  const wrapped = dialogVisible && !clickThrough;
  const showPeekTab = peeked && wrapped;

  const ctxValue = useMemo<DialogPeekContext>(
    () => ({
      peeked,
      togglePeek: () => setPeeked((p) => !p),
      setPeeked,
    }),
    [peeked],
  );

  // Use mobile-aware slide direction. On wide viewports the dialog slides
  // right (mirrors the stack panel — established muscle memory). On narrow
  // viewports it slides down (more reachable on phones).
  const isNarrow = useIsNarrowViewport();
  const slideTransform = peeked
    ? isNarrow
      ? { x: 0, y: "calc(100vh - 64px)" }
      : { x: "calc(100vw - 32px)", y: 0 }
    : { x: 0, y: 0 };

  return (
    <DialogPeekCtx.Provider value={ctxValue}>
      {/* The host's motion.div must (a) NOT establish a transform-CB that
          mis-anchors `fixed inset:0` dialog descendants when at rest, and
          (b) NOT block board clicks/hovers when no dialog is up.
          Both are achieved by gating `fixed inset-0` on `dialogVisible`:
          when a dialog is visible the host fills the viewport (so the
          transform CB IS the viewport, dialogs render correctly, slide
          works); when none is up the host collapses to an in-flow 0-size
          box that intercepts nothing. Pointer-events:none kicks in only
          while peeked so clicks/hovers pass through to the battlefield —
          otherwise the dialog itself handles events normally. */}
      <motion.div
        className={wrapped ? "fixed inset-0 z-40" : ""}
        style={
          wrapped ? { pointerEvents: peeked ? "none" : undefined } : undefined
        }
        animate={wrapped ? slideTransform : undefined}
        transition={
          shouldReduceMotion
            ? { duration: 0 }
            : { type: "spring", stiffness: 320, damping: 32 }
        }
      >
        {children}
      </motion.div>
      {showPeekTab ? (
        <PeekRestoreTab
          direction={isNarrow ? "bottom" : "right"}
          onClick={() => setPeeked(false)}
        />
      ) : null}
    </DialogPeekCtx.Provider>
  );
}

function PeekRestoreTab({
  direction,
  onClick,
}: {
  direction: "right" | "bottom";
  onClick: () => void;
}) {
  // Inset by `right-3` / `bottom-3` so all four borders render fully —
  // flush-to-edge positioning clips the outer border on some browsers
  // (especially with non-zero safe-area insets).
  const positionClass =
    direction === "right"
      ? "right-3 top-1/2 -translate-y-1/2 h-24 w-9 rounded-2xl"
      : "bottom-3 left-1/2 -translate-x-1/2 h-9 w-24 rounded-2xl";

  const iconRotate = direction === "right" ? "rotate-180" : "-rotate-90";

  return (
    <motion.button
      type="button"
      onClick={onClick}
      aria-label="Restore dialog"
      title="Restore dialog"
      initial={{ opacity: 0, scale: 0.9 }}
      animate={{
        opacity: 1,
        scale: 1,
        boxShadow: [
          "0 18px 36px rgba(0,0,0,0.45), 0 0 0 1px rgba(34,211,238,0.2)",
          "0 18px 36px rgba(0,0,0,0.45), 0 0 28px rgba(34,211,238,0.55)",
          "0 18px 36px rgba(0,0,0,0.45), 0 0 0 1px rgba(34,211,238,0.2)",
        ],
      }}
      transition={{
        opacity: { delay: 0.1, duration: 0.2 },
        scale: { delay: 0.1, duration: 0.2 },
        boxShadow: { duration: 2.4, repeat: Infinity, ease: "easeInOut" },
      }}
      className={`fixed z-[60] flex items-center justify-center border border-cyan-400/40 bg-[#0b1020]/96 text-cyan-200 backdrop-blur-md transition-colors hover:bg-cyan-500/20 hover:text-white ${positionClass}`}
    >
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 20 20"
        fill="currentColor"
        className={`h-6 w-6 ${iconRotate}`}
      >
        <path
          fillRule="evenodd"
          d="M7.22 4.22a.75.75 0 0 1 1.06 0l5.25 5.25a.75.75 0 0 1 0 1.06l-5.25 5.25a.75.75 0 1 1-1.06-1.06L11.94 10 7.22 5.28a.75.75 0 0 1 0-1.06Z"
          clipRule="evenodd"
        />
      </svg>
    </motion.button>
  );
}

function useIsNarrowViewport(breakpoint = 640): boolean {
  const [isNarrow, setIsNarrow] = useState(() =>
    typeof window === "undefined" ? false : window.innerWidth < breakpoint,
  );
  useEffect(() => {
    if (typeof window === "undefined") return;
    const update = () => setIsNarrow(window.innerWidth < breakpoint);
    window.addEventListener("resize", update);
    return () => window.removeEventListener("resize", update);
  }, [breakpoint]);
  return isNarrow;
}
