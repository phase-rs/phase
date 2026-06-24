import { useEffect, useMemo, useState } from "react";
import { motion, useReducedMotion } from "framer-motion";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";

import type { GameObject, ObjectId, WaitingFor } from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { getBoardChoiceView } from "../../viewmodel/gameStateView.ts";
import { DialogPeekCtx, type DialogPeekContext } from "./dialogPeekContext.ts";

// `WaitingFor` variants that do NOT render a centered dialog/overlay.
// Board-level interactions (Priority, combat declarations) and pre-game
// flows render inline on the board rather than as a centered modal.
//
// NOTE: combat damage *assignment* (`AssignCombatDamage` / `AssignBlockerDamage`)
// is deliberately ABSENT — unlike attacker/blocker declaration, it renders a
// centered `ChoiceOverlay` modal (via `CardChoiceModal` → `DamageAssignmentModal`).
// Listing it here would leave the host un-anchored (`className=""`), so the
// modal's `fixed inset-0 z-50` would be trapped inside framer-motion's
// transform stacking context and paint BELOW the board/HUD/hand (see the
// anchoring contract on lines 114-123). Centered modals must stay out of this set.
const NON_DIALOG_WAITING_FOR_TYPES: ReadonlySet<WaitingFor["type"]> = new Set<WaitingFor["type"]>([
  "Priority",
  "DeclareAttackers",
  "DeclareBlockers",
  "MulliganDecision",
  "MulliganBottomCards",
  "OpeningHandBottomCards",
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
//
// Exported so `GamePage` can use the same predicate to gate `<TargetingOverlay />`
// (single source of truth — adding a new click-through WaitingFor only needs
// editing one place, and the typed set forces compile-time validity).
export const CLICK_THROUGH_WAITING_FOR_TYPES: ReadonlySet<WaitingFor["type"]> = new Set<WaitingFor["type"]>([
  "TargetSelection",
  "TriggerTargetSelection",
  "CopyTargetChoice",
  "CopyRetarget",
  "RetargetChoice",
  "ExploreChoice",
  "PopulateChoice",
  "ReturnAsAuraTarget",
]);

// CR 118.3 + CR 605.3b: a `PayCost` prompt is click-through only for the
// TapCreatures kind (the player taps creatures on the battlefield). All other
// cost kinds surface a modal in `CardChoiceModal` and must stay host-wrapped.
export function isClickThroughWaitingFor(
  waitingFor: WaitingFor | null | undefined,
  objects?: Record<ObjectId, GameObject | undefined>,
): boolean {
  if (!waitingFor) return false;
  if (CLICK_THROUGH_WAITING_FOR_TYPES.has(waitingFor.type)) return true;
  return getBoardChoiceView(waitingFor, objects) != null;
}

function isDialogVisibleFor(waitingFor: WaitingFor | null | undefined): boolean {
  if (!waitingFor) return false;
  return !NON_DIALOG_WAITING_FOR_TYPES.has(waitingFor.type);
}

function isClickThroughDialog(
  waitingFor: WaitingFor | null | undefined,
  objects?: Record<ObjectId, GameObject | undefined>,
): boolean {
  if (!waitingFor) return false;
  if (isClickThroughWaitingFor(waitingFor, objects)) return true;
  // Any `ManaPayment` panel is click-through. The host still anchors the panel
  // at `fixed inset-0 z-40` (so it can't be trapped beneath the board), but
  // click-through marks it `pointer-events: none` so board taps reach the
  // battlefield — the panel's own controls re-enable events.
  //
  // Manual payment of plain/hybrid mana DOES need board interaction: the player
  // taps their own lands and clicks mana-pool pips behind the panel to pay. (In
  // auto mode an unambiguous cast finalizes before any panel is shown, so a
  // `ManaPayment` WaitingFor only ever reaches the UI when board taps are live.)
  // Convoke/improvise (CR 702.51a / CR 702.126a), which additionally tap
  // creatures/artifacts, are a subset of this same click-through behavior.
  return waitingFor.type === "ManaPayment";
}

export function DialogHost({ children }: { children: ReactNode }) {
  const waitingFor = useGameStore((s) => s.waitingFor);
  const objects = useGameStore((s) => s.gameState?.objects);
  // Only treat a `WaitingFor` as a host-anchored dialog when the local
  // player can actually act on it. Otherwise (opponent searching their
  // library, scrying, etc.) the engine's WaitingFor is on the opponent and
  // every concrete modal inside the host already returns null — wrapping
  // anyway leaves an empty `fixed inset-0 z-40` overlay that swallows
  // pointer events and prevents the local player from hovering / zooming
  // cards while they spectate.
  const canActForWaitingState = useCanActForWaitingState();
  // UI-driven dialogs (e.g. the planeswalker / multi-ability picker fired
  // from PermanentCard while the player has Priority, or the enchantments
  // dialog fired from EnchantmentsBadge in HudPlate) also need the host to
  // anchor `fixed inset-0` descendants to the viewport. Subscribing here
  // keeps the contract uniform: any modal rendered inside DialogHost is
  // centered regardless of which signal triggered it.
  const hasUiDialog = useUiStore(
    (s) => s.pendingAbilityChoice != null || s.enchantmentsDialogPlayer != null,
  );
  const [peeked, setPeeked] = useState(false);
  const shouldReduceMotion = useReducedMotion();

  // CR display contract: every new engine prompt must be visible. When the
  // WaitingFor reference changes (the store emits a new object on every
  // engine update), reset peek so the player isn't shown a hidden dialog.
  useEffect(() => {
    setPeeked(false);
  }, [waitingFor, hasUiDialog]);

  const dialogVisible =
    (isDialogVisibleFor(waitingFor) && canActForWaitingState) || hasUiDialog;
  // CR 702.51a: convoke/improvise payment marks the host click-through so board
  // taps reach creatures. But a UI-driven modal opened mid-payment — e.g. the
  // ability picker fired by tapping a mana creature like Birds of Paradise, which
  // is convoke-eligible AND has an activatable mana ability — renders INSIDE this
  // host. Leaving the host click-through makes that modal inherit
  // `pointer-events: none`, so its buttons are dead and clicks fall through to the
  // board/hand behind it. While such a modal is up the player interacts with it,
  // not the board, so suppress click-through to restore pointer events.
  const clickThrough = isClickThroughDialog(waitingFor, objects) && !hasUiDialog;
  // BASE INVARIANT: every visible prompt is anchored in this viewport-level
  // `fixed inset-0 z-40` stacking context, so no prompt can ever be trapped
  // beneath the board. The board grid is its own `relative z-10` stacking
  // context; framer-motion leaves a `transform`/`will-change: transform` on
  // this node, which would demote an un-anchored (className="") host to a
  // `z-auto` context that paints BELOW the board — burying the dialog behind
  // the HUD and hand. Anchoring at an explicit `z-40` keeps the context at
  // level 40 (above the board) regardless of any transform framer applies.
  // Click-through is achieved with `pointer-events: none` (below), NOT by
  // un-anchoring, so board taps still reach the battlefield.
  const anchored = dialogVisible;
  // Any mana-payment panel is bottom-anchored and can overlap the very lands /
  // creatures the player must tap to pay (manual mana, convoke, improvise). Unlike
  // the translucent full-screen target overlays, it benefits from the peek/slide
  // affordance so the player can collapse it off an overlapped permanent — so it
  // stays peekable even while click-through. Other click-through overlays (target
  // picking) are translucent and full-screen, so they stay put and pass taps
  // straight through.
  const isManaPayment = waitingFor?.type === "ManaPayment";
  const peekable = anchored && (!clickThrough || isManaPayment);
  const showPeekTab = peeked && peekable;

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
  // Only apply the peek slide transform while peeked. Framer-motion keeps a
  // residual `transform` (even at `{ x: 0, y: 0 }`) whenever `animate` is set,
  // which breaks `<input type="range">` hit-testing in bottom-anchored panels
  // such as ChooseXValueUI — the slider looks fine but ignores drags until
  // something else reflows the tree (issue #2427).
  const slideTransform = peeked
    ? isNarrow
      ? { x: 0, y: "calc(100vh - 64px)" }
      : { x: "calc(100vw - 32px)", y: 0 }
    : undefined;

  return (
    <DialogPeekCtx.Provider value={ctxValue}>
      {/* When a dialog is visible the host fills the viewport as a `z-40`
          stacking context so descendants render above the board; when none is
          up it collapses to an in-flow 0-size box that intercepts nothing.
          `pointer-events: none` lets taps/hovers pass through to the
          battlefield while peeked OR for click-through prompts (convoke /
          improvise / target picking); interactive children re-enable events
          with `pointer-events-auto`. Otherwise the dialog handles events
          normally. */}
      <motion.div
        className={anchored ? "fixed inset-0 z-40" : ""}
        style={
          anchored
            ? { pointerEvents: clickThrough || peeked ? "none" : undefined }
            : undefined
        }
        animate={peekable && peeked ? slideTransform : undefined}
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

export function PeekRestoreTab({
  direction,
  onClick,
}: {
  direction: "right" | "bottom";
  onClick: () => void;
}) {
  const { t } = useTranslation("game");
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
      aria-label={t("dialogShell.restoreDialog")}
      title={t("dialogShell.restoreDialog")}
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

export function useIsNarrowViewport(breakpoint = 640): boolean {
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
