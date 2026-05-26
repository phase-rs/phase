import { useTranslation } from "react-i18next";

import type { SeatPublicView } from "../../adapter/draft-adapter";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";
import { BotIndicator } from "./BotIndicator";

const EMPTY_SEATS: SeatPublicView[] = [];

// ── Pick status colors ──────────────────────────────────────────────────

const PICK_STATUS_BORDER: Record<SeatPublicView["pick_status"], string> = {
  Pending: "border-white/20",
  Picked: "border-green-400/30",
  TimedOut: "border-red-400/30",
  NotDrafting: "border-white/10",
};

const PICK_STATUS_DOT: Record<SeatPublicView["pick_status"], string> = {
  Pending: "bg-white/30",
  Picked: "bg-green-400",
  TimedOut: "bg-red-400",
  NotDrafting: "bg-white/10",
};

// ── Seat Badge ──────────────────────────────────────────────────────────

interface SeatBadgeProps {
  seat: SeatPublicView;
  isLocal: boolean;
}

function SeatBadge({ seat, isLocal }: SeatBadgeProps) {
  const { t } = useTranslation("draft");
  const botLabel = t("lobby.botSeat");
  const borderColor = isLocal
    ? "border-emerald-400/40"
    : PICK_STATUS_BORDER[seat.pick_status];

  return (
    <div
      className={`flex items-center gap-1.5 rounded-[12px] border bg-black/18 px-2 py-1 backdrop-blur-md ${borderColor}`}
    >
      <div className={`h-1.5 w-1.5 rounded-full ${PICK_STATUS_DOT[seat.pick_status]}`} />
      <span className="truncate text-xs text-white/70">
        {seat.display_name || t("seat.label", { number: seat.seat_index + 1 })}
      </span>
      {seat.is_bot && <BotIndicator label={botLabel} size="sm" />}
    </div>
  );
}

// ── Component ───────────────────────────────────────────────────────────

/** 8-seat status ring showing each player's name and pick status with pass direction. */
export function SeatStatusRing() {
  const { t } = useTranslation("draft");
  const seats = useMultiplayerDraftStore((s) => s.view?.seats ?? EMPTY_SEATS);
  const passDirection = useMultiplayerDraftStore((s) => s.view?.pass_direction);
  const localSeat = useMultiplayerDraftStore((s) => s.seatIndex);

  if (seats.length === 0) return null;

  // Top row: seats 0-3, Bottom row: seats 4-7
  const topRow = seats.slice(0, 4);
  const bottomRow = seats.slice(4, 8);

  return (
    <div className="flex flex-col gap-2 mb-4">
      <div className="grid grid-cols-4 gap-2">
        {topRow.map((seat) => (
          <SeatBadge
            key={seat.seat_index}
            seat={seat}
            isLocal={seat.seat_index === localSeat}
          />
        ))}
      </div>
      {/* Pass direction indicator */}
      <div className="flex justify-center text-white/40 text-sm">
        {passDirection === "Left"
          ? t("seat.passingLeft")
          : t("seat.passingRight")}
      </div>
      <div className="grid grid-cols-4 gap-2">
        {bottomRow.map((seat) => (
          <SeatBadge
            key={seat.seat_index}
            seat={seat}
            isLocal={seat.seat_index === localSeat}
          />
        ))}
      </div>
    </div>
  );
}
