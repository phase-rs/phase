import { useEffect, useState } from "react";
import { createPortal } from "react-dom";

import type { ObjectId } from "../../adapter/types.ts";
import { DialogAttachmentCard } from "./DialogAttachmentCard.tsx";

interface Props {
  anchorEl: HTMLElement;
  attachmentIds: readonly ObjectId[];
}

const CARD_W = 200;
const ANCHOR_GAP_PX = 10;

/**
 * Lightweight passive popover that floats the player's enchanting Auras
 * near the EnchantmentsBadge while the cursor is on it. Renders the same
 * `<DialogAttachmentCard>` (full readable Scryfall image) used in the
 * full dialog — so what you glance at in hover is exactly what you'd
 * read in the dialog.
 *
 * Passive by design: the wrapper is `pointer-events-none`, so the cursor
 * passes straight through to whatever is underneath. This means leaving
 * the badge dismisses the popover cleanly (no "did I land in the
 * popover or just slide off the badge edge?" ambiguity), and clicking a
 * card in the popover is impossible — to interact with an Aura the
 * player clicks the badge to open the full `<AttachmentsDialog>`. Click
 * = active mode; hover = passive mode. Two interaction lanes, one
 * visual surface.
 *
 * Portals to `document.body` for the same reason the dialog does:
 * HudPlate sets a Tailwind `transform` CSS property and would otherwise
 * become this popover's containing block, shrinking it to the badge's
 * bounding box. Mirrors the `PortaledPopover` pattern in
 * `OpponentHud.tsx` (PortaledPopover function near the bottom).
 *
 * Anchor placement is auto-flipped based on the badge's vertical
 * position: top half of the viewport → popover below the badge,
 * bottom half → popover above. So the player HUD (screen bottom)
 * lands the popover above its badge, and opponent HUDs (screen top)
 * land it below — both directions point into that player's territory,
 * matching where their Auras "live."
 */
export function AurasHoverPreview({ anchorEl, attachmentIds }: Props) {
  const [pos, setPos] = useState<{
    left: number;
    top: number;
    placement: "above" | "below";
  } | null>(null);

  useEffect(() => {
    function recompute() {
      const rect = anchorEl.getBoundingClientRect();
      const placement: "above" | "below" =
        rect.top < window.innerHeight / 2 ? "below" : "above";
      const left = rect.left + rect.width / 2;
      const top = placement === "above" ? rect.top - ANCHOR_GAP_PX : rect.bottom + ANCHOR_GAP_PX;
      setPos({ left, top, placement });
    }
    recompute();
    // The badge can shift when other trailing badges appear/disappear (life
    // change, monarch toggle, etc.) — recompute on resize and on scroll
    // (`true` for capture so nested scrollers also fire).
    window.addEventListener("resize", recompute);
    window.addEventListener("scroll", recompute, true);
    return () => {
      window.removeEventListener("resize", recompute);
      window.removeEventListener("scroll", recompute, true);
    };
  }, [anchorEl]);

  if (!pos) return null;

  const transform =
    pos.placement === "above" ? "translate(-50%, -100%)" : "translate(-50%, 0)";

  return createPortal(
    <div
      className="pointer-events-none fixed z-50"
      style={{ left: pos.left, top: pos.top, transform }}
      aria-hidden
    >
      <div className="flex flex-wrap justify-center gap-2 rounded-2xl border border-violet-400/40 bg-slate-950/95 p-3 shadow-[0_18px_36px_rgba(0,0,0,0.55)] backdrop-blur-md">
        {attachmentIds.map((id) => (
          <DialogAttachmentCard key={id} objectId={id} widthPx={CARD_W} />
        ))}
      </div>
    </div>,
    document.body,
  );
}
