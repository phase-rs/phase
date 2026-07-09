import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { PlayerId } from "../../adapter/types.ts";
import { usePlayerId, waitingPlayer } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { getWaitingForObjectChoiceIds } from "../../viewmodel/gameStateView.ts";
import { AurasHoverPreview } from "./AurasHoverPreview.tsx";

interface Props {
  playerId: PlayerId;
}

const STABLE_EMPTY: readonly never[] = [];

// Brief delay before dismissing on mouseleave. Smooths over micro-flicker
// when the cursor briefly skirts the badge edge during pointer movement —
// without the delay, normal cursor jitter on the way to/from clicks pops
// the popover open and shut visibly.
const HOVER_CLOSE_DELAY_MS = 80;

/**
 * Trailing-row HUD badge that surfaces player-attached Auras (Curse cycle,
 * Faith's Fetters, Dictate of Kruphix, etc.) without disturbing the plate
 * layout. Slots into the same row as Monarch/Initiative/Counter badges
 * because, semantically, "this player is enchanted" belongs to the same
 * vocabulary of imposed-state indicators.
 *
 * Reads `gameState.derived.auras_attached_to_player`, an engine-authored
 * projection (see `crates/engine/src/game/derived_views.rs`). Per CLAUDE.md
 * the FE never scans the battlefield for `attached_to.type === "Player"` —
 * that's game logic owned by the engine.
 *
 * Two interaction lanes:
 *   - Hover (passive): pops `<AursHoverPreview>` — a portaled `pointer-
 *     events-none` floating panel showing every Aura at full readable
 *     size. Glance only; no clicks. Dismisses with an 80ms delay on
 *     mouseleave to absorb cursor jitter.
 *   - Click (active): dispatches `setEnchantmentsDialogPlayer(playerId)`,
 *     which triggers `<PlayerEnchantmentsDialog>` (mounted in DialogHost)
 *     to render the full `<AttachmentsDialog>` modal. Dialog cards are
 *     interactive — target select / activation work as on the battlefield.
 *
 * Click-vs-hover separation gives the player a low-friction "what's
 * enchanting me right now?" glance without committing to a modal, while
 * still offering the full modal for the moments they actually need to
 * interact (e.g., destroy target Aura).
 *
 * The dialog itself is rendered by `<PlayerEnchantmentsDialog>` mounted
 * inside `<DialogHost>` (GamePage), NOT here. The badge lives inside
 * HudPlate, which sets a Tailwind `transform` CSS property and becomes a
 * containing block for any `fixed inset-0` descendants — a dialog rendered
 * as a child of the badge would shrink to HudPlate's bounding box. See
 * DialogHost.tsx:113-122 for the contract.
 */
export function EnchantmentsBadge({ playerId }: Props) {
  const { t } = useTranslation("game");
  const auraIds = useGameStore(
    (s) => s.gameState?.derived?.auras_attached_to_player?.[String(playerId)] ?? STABLE_EMPTY,
  );
  const setEnchantmentsDialogPlayer = useUiStore((s) => s.setEnchantmentsDialogPlayer);

  // A player-attached Aura has no battlefield surface — this badge is its only
  // entry point. When the engine asks THIS seat for an object choice that one
  // of these Auras satisfies (e.g. Copy Enchantment's `CopyTargetChoice`, CR
  // 707.9), the badge must advertise it; otherwise the choice is invisible and
  // the Aura reads as an illegal target. Same authority + lime vocabulary the
  // battlefield uses for a valid target.
  const localPlayerId = usePlayerId();
  const hasActionableAura = useGameStore((s) => {
    const waitingFor = s.waitingFor;
    if (waitingPlayer(waitingFor) !== localPlayerId) return false;
    const choosable = getWaitingForObjectChoiceIds(waitingFor);
    return auraIds.some((id) => choosable.includes(id));
  });

  const buttonRef = useRef<HTMLButtonElement>(null);
  const [hoverOpen, setHoverOpen] = useState(false);
  const closeTimerRef = useRef<number | null>(null);

  const cancelClose = useCallback(() => {
    if (closeTimerRef.current != null) {
      window.clearTimeout(closeTimerRef.current);
      closeTimerRef.current = null;
    }
  }, []);

  const onEnter = useCallback(() => {
    cancelClose();
    setHoverOpen(true);
  }, [cancelClose]);

  const onLeave = useCallback(() => {
    cancelClose();
    closeTimerRef.current = window.setTimeout(() => {
      setHoverOpen(false);
      closeTimerRef.current = null;
    }, HOVER_CLOSE_DELAY_MS);
  }, [cancelClose]);

  // Cleanup pending timer on unmount so a setHoverOpen never fires after
  // the component is gone (badge unmounts when the last Aura disappears).
  useEffect(() => () => cancelClose(), [cancelClose]);

  if (auraIds.length === 0) return null;

  const count = auraIds.length;
  const ariaLabel = t("enchantmentsBadge.ariaLabel", { count });
  const tooltip = t("enchantmentsBadge.tooltip", { count });

  return (
    <>
      <button
        ref={buttonRef}
        type="button"
        aria-label={ariaLabel}
        title={tooltip}
        onMouseEnter={onEnter}
        onMouseLeave={onLeave}
        onFocus={onEnter}
        onBlur={onLeave}
        onClick={() => setEnchantmentsDialogPlayer(playerId)}
        data-actionable={hasActionableAura || undefined}
        className={`relative inline-flex h-6 min-w-6 shrink-0 cursor-pointer items-center justify-center gap-0.5 rounded-full px-1.5 text-[11px] font-bold leading-none text-violet-50 bg-gradient-to-b from-violet-500 to-violet-700 transition-all duration-150 hover:from-violet-400 hover:to-violet-600 hover:shadow-[0_0_18px_rgba(167,139,250,0.7)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-violet-200 ${
          hasActionableAura
            ? "ring-2 ring-lime-300 shadow-[0_0_16px_5px_rgba(190,242,100,0.7)]"
            : "ring-1 ring-violet-300/60 shadow-[0_0_12px_rgba(139,92,246,0.45)]"
        }`}
      >
        <span aria-hidden className="text-[13px] leading-none">✧</span>
        {count > 1 ? <span className="tabular-nums">×{count}</span> : null}
      </button>
      {hoverOpen && buttonRef.current && (
        <AurasHoverPreview anchorEl={buttonRef.current} attachmentIds={auraIds} />
      )}
    </>
  );
}
