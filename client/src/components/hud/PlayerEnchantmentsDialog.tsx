import type { PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { AttachmentsDialog } from "./AttachmentsDialog.tsx";

const STABLE_EMPTY: readonly never[] = [];

/**
 * Self-mounted DialogHost child that opens the player-attached-Aura dialog.
 *
 * Subscribes to `uiStore.enchantmentsDialogPlayer` (set by
 * `EnchantmentsBadge` on click) and renders `<AttachmentsDialog>` for that
 * player. Mounted inside `<DialogHost>` in GamePage so the dialog's
 * `fixed inset-0` shell anchors to the viewport — see DialogHost.tsx:113-122
 * for why this matters (HudPlate establishes a `transform` containing block
 * that would otherwise shrink the dialog to the badge's bounding box).
 *
 * Mirrors the `AbilityChoiceModal` pattern (GamePage.tsx:2076):
 * self-contained, no props, returns null when not active.
 */
export function PlayerEnchantmentsDialog() {
  const playerId = useUiStore((s) => s.enchantmentsDialogPlayer) as PlayerId | null;
  const setEnchantmentsDialogPlayer = useUiStore((s) => s.setEnchantmentsDialogPlayer);
  const auraIds = useGameStore(
    (s) =>
      playerId == null
        ? STABLE_EMPTY
        : s.gameState?.derived?.auras_attached_to_player?.[String(playerId)] ?? STABLE_EMPTY,
  );

  // Auto-close if the player loses every Aura while the dialog is open
  // (e.g. opponent destroys the last Curse). Returning null is safe — the
  // dialog state in uiStore is independent of mount and will be cleared by
  // the next explicit user action or close.
  if (playerId == null || auraIds.length === 0) return null;

  return (
    <AttachmentsDialog
      isOpen
      onClose={() => setEnchantmentsDialogPlayer(null)}
      host={{ type: "player", playerId }}
      attachmentIds={auraIds}
    />
  );
}
