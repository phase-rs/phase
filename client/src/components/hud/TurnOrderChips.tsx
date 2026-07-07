import { useMemo } from "react";
import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";

import type { PlayerId, TurnOrderSlotView } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";

interface TurnOrderChipsProps {
  playerId: PlayerId;
  compact?: boolean;
}

function chipText(slot: TurnOrderSlotView, t: TFunction<"game">) {
  if (slot.turns_from_now === 0) return t("turnOrder.now");
  if (slot.turns_from_now === 1) return t("turnOrder.next");
  return t("turnOrder.future", { count: slot.turns_from_now });
}

function chipTitle(slot: TurnOrderSlotView, t: TFunction<"game">) {
  if (slot.turns_from_now === 0) return t("turnOrder.nowTooltip");
  if (slot.turns_from_now === 1) return t("turnOrder.nextTooltip");
  return t("turnOrder.futureTooltip", { count: slot.turns_from_now });
}

export function TurnOrderChips({ playerId, compact = false }: TurnOrderChipsProps) {
  const { t } = useTranslation("game");
  const rows = useGameStore((s) => s.gameState?.derived?.turn_order);
  const playerRows = useMemo(
    () => rows?.filter((row) => row.player === playerId).sort((a, b) => a.slot_index - b.slot_index) ?? [],
    [playerId, rows],
  );

  if (playerRows.length === 0) return null;

  return (
    <span className="inline-flex shrink-0 items-center gap-0.5" data-testid={`turn-order-chips-${playerId}`}>
      {playerRows.map((slot) => {
        const title = chipTitle(slot, t);
        return (
          <span
            key={`${slot.slot_index}-${slot.turns_from_now}`}
            aria-label={title}
            title={title}
            className={`inline-flex shrink-0 items-center justify-center rounded-sm border border-sky-200/45 bg-sky-400/16 font-black uppercase text-sky-100 shadow-[0_0_8px_rgba(56,189,248,0.24)] ${compact ? "h-4 min-w-5 px-1 text-[8px]" : "h-5 min-w-6 px-1.5 text-[9px]"}`}
          >
            {chipText(slot, t)}
          </span>
        );
      })}
    </span>
  );
}
