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
  // Disconnected humans get a red border that overrides the pick-status colour.
  // Bots are always "connected" by construction so we ignore the field for them.
  const showDisconnected = !seat.is_bot && !seat.connected;
  const borderColor = showDisconnected
    ? "border-rose-400/50"
    : isLocal
      ? "border-emerald-400/40"
      : PICK_STATUS_BORDER[seat.pick_status];
  const faceUpNames = seat.face_up_draft_cards.map((card) => card.name).join(", ");
  const activePackCount = seat.active_pack_count;

  return (
    <div
      data-seat-badge
      className={`relative flex h-full min-h-[40px] w-full min-w-[15ch] flex-col items-start rounded-[8px] border bg-black/18 px-1.5 py-0.5 pr-7 backdrop-blur-md ${borderColor}`}
    >
      <div className="flex min-w-0 items-center gap-1.5">
        <div
          className={`h-1.5 w-1.5 rounded-full ${
            showDisconnected
              ? "bg-rose-400"
              : PICK_STATUS_DOT[seat.pick_status]
          }`}
          aria-label={
            showDisconnected ? t("seat.disconnected") : t("seat.connected")
          }
        />
        <span
          className={`truncate ${
            showDisconnected ? "text-white/40 line-through" : "text-white/70"
          }`}
        >
          {seat.display_name || t("seat.label", { number: seat.seat_index + 1 })}
        </span>
        {seat.is_bot && <BotIndicator label={botLabel} size="sm" />}
      </div>
      {faceUpNames && (
        <span
          className="max-w-full break-words text-[10px] leading-tight text-amber-200"
          title={faceUpNames}
        >
          {t("seat.faceUpDraftCards", { cards: faceUpNames })}
        </span>
      )}
      <span className="sr-only">{t("seat.activePackCount", { count: activePackCount, player: seat.display_name || t("seat.label", { number: seat.seat_index + 1 }) })}</span>
      <span className="absolute right-0.5 top-1/2 h-7 w-7 -translate-y-1/2" aria-hidden="true">
        <img src="/icons/packs.svg" alt="" className={`h-full w-full object-contain ${activePackCount === 0 ? "opacity-35 grayscale" : ""}`} />
        <span className="absolute inset-0 flex items-center justify-center font-mono text-xs font-bold tabular-nums text-jade [-webkit-text-stroke:1px_rgb(2_6_23_/_0.95)] [paint-order:stroke_fill]">{activePackCount}</span>
      </span>
    </div>
  );
}

interface SeatStatusRingLayoutProps {
  seats: SeatPublicView[];
  passDirection: "Left" | "Right" | undefined;
  localSeat: number | null;
  passDirectionLabel: string;
}

export function SeatStatusRingLayout({
  seats,
  passDirection,
  localSeat,
  passDirectionLabel,
}: SeatStatusRingLayoutProps) {
  const arrow = passDirection === "Right" ? "←" : "→";
  const arrowElement = (
    <span
      aria-hidden="true"
      data-pass-arrow
      className="flex w-4 shrink-0 items-center justify-center text-sm text-white/40"
    >
      {arrow}
    </span>
  );

  return (
    <div
      data-seat-status-ring
      data-pass-direction={passDirection ?? "Left"}
      className="mb-2 grid grid-cols-[repeat(auto-fit,minmax(calc(15ch+3.5rem),1fr))] gap-1 text-xs"
    >
      <span className="sr-only">{passDirectionLabel}</span>
      {seats.map((seat) => (
        <div
          key={seat.seat_index}
          data-seat-pass-unit
          className="flex min-w-0 items-stretch gap-1.5"
        >
          {passDirection === "Right" && arrowElement}
          <SeatBadge
            seat={seat}
            isLocal={seat.seat_index === localSeat}
          />
          {passDirection !== "Right" && arrowElement}
        </div>
      ))}
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

  return (
    <SeatStatusRingLayout
      seats={seats}
      passDirection={passDirection}
      localSeat={localSeat}
      passDirectionLabel={passDirection === "Right"
        ? t("seat.passingRight")
        : t("seat.passingLeft")}
    />
  );
}
