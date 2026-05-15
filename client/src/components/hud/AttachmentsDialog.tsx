import type { ObjectId, PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getPlayerDisplayName } from "../../stores/multiplayerStore.ts";
import { DialogShell } from "../modal/DialogShell.tsx";
import { DialogAttachmentCard } from "./DialogAttachmentCard.tsx";

/** What's enchanted/equipped/fortified — used for the dialog title text and
 *  to disambiguate the eyebrow ("Enchantments on Player" vs "Attached to").
 *  A discriminated union so the wiring is clean for both player-host (Aura
 *  on player, CR 303.4f) and object-host (Aura/Equipment/Fortification on
 *  permanent, CR 301.5 / 303.4 / 301.6) cases — the latter is what
 *  `<PermanentCard>` will adopt as its >=2-attachments expand affordance. */
export type AttachmentHost =
  | { type: "player"; playerId: PlayerId }
  | { type: "object"; objectId: ObjectId };

interface Props {
  isOpen: boolean;
  onClose: () => void;
  host: AttachmentHost;
  attachmentIds: readonly ObjectId[];
}

// 220px wide hits the readability threshold for Scryfall normal images —
// oracle text legible without zooming. Two cards fit per row in a max-w-3xl
// dialog (~768px) with chrome.
const ATTACHMENT_W_PX = 220;

/**
 * Modal listing every permanent attached to a host (player for Aura
 * curses, or object for creature/planeswalker/battle attachments). The
 * host itself is identified by the dialog header (eyebrow + title) — no
 * separate visual pane, since the cards are what the player came to read.
 *
 * Each attachment renders through `<DialogAttachmentCard>` (full Scryfall
 * normal image, click-forwards target select / activation, counter
 * overlay). Wraps to additional rows as needed; `scrollable` on the
 * DialogShell handles vertical overflow when N is large.
 *
 * Used for player-attached Aura clusters today and structured to take
 * object hosts as well — the discriminated `host` union picks the right
 * eyebrow text without forcing callers to assemble it.
 */
export function AttachmentsDialog({ isOpen, onClose, host, attachmentIds }: Props) {
  const hostName = useHostName(host);

  if (!isOpen) return null;

  const eyebrow = host.type === "player" ? "Enchantments on Player" : "Attached to";
  const title = hostName;
  const subtitle =
    attachmentIds.length === 1
      ? "1 attached permanent"
      : `${attachmentIds.length} attached permanents`;

  return (
    <DialogShell
      eyebrow={eyebrow}
      title={title}
      subtitle={subtitle}
      size="lg"
      scrollable
      onClose={onClose}
    >
      <div className="flex flex-wrap content-start justify-center gap-3 px-4 py-4 lg:px-6 lg:py-5">
        {attachmentIds.map((id) => (
          <DialogAttachmentCard key={id} objectId={id} widthPx={ATTACHMENT_W_PX} />
        ))}
      </div>
    </DialogShell>
  );
}

function useHostName(host: AttachmentHost): string {
  const objectName = useGameStore((s) =>
    host.type === "object" ? s.gameState?.objects[host.objectId]?.name ?? "Unknown" : "",
  );
  if (host.type === "object") return objectName;
  return getPlayerDisplayName(host.playerId, host.playerId);
}
