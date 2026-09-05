import { useCallback, useMemo, useState } from "react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { CardImage } from "../card/CardImage.tsx";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import type {
  OutsideGameChoiceEntry,
  OutsideGameSelection,
  WaitingFor,
} from "../../adapter/types.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { CHOICE_CARD_IMAGE_CLASS } from "./cardChoice/shared.tsx";

type OutsideGameChoice = Extract<WaitingFor, { type: "OutsideGameChoice" }>;

/** One booster-pack candidate, narrowed from the generic outside-game entry. */
interface BoosterEntry {
  packSlot: number;
  setCode: string;
  name: string;
  oracleId?: string;
}

/**
 * Narrow an outside-game choice list to its booster-pack entries.
 *
 * Returns `null` unless EVERY entry came from an opened pack, so the generic
 * wishboard modal keeps rendering sideboard and face-up-exile pools (and any
 * mixed pool a future effect offers) exactly as before.
 */
export function boosterPackEntries(
  choices: OutsideGameChoiceEntry[],
): BoosterEntry[] | null {
  if (choices.length === 0) return null;
  const entries: BoosterEntry[] = [];
  for (const choice of choices) {
    if (choice.source.type !== "BoosterPack") return null;
    entries.push({
      packSlot: choice.source.data.pack_slot,
      setCode: choice.source.data.set_code,
      name: choice.source.data.card.name,
      oracleId: choice.source.data.card.scryfall_oracle_id ?? undefined,
    });
  }
  return entries;
}

/**
 * The opened booster pack: every revealed card, laid out face up, with the
 * cards the player keeps highlighted.
 *
 * The engine decides which cards the pack holds, how many may be taken, and
 * where they go (`Effect::OpenBoosterPack`); this renders that decision and
 * dispatches the picks back as `ChooseOutsideGameCards`. Cards left in the pack
 * were never in the game and go nowhere — the engine simply drops them.
 */
export function BoosterPackModal({
  data,
  entries,
}: {
  data: OutsideGameChoice["data"];
  entries: BoosterEntry[];
}) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const [selected, setSelected] = useState<Set<number>>(new Set());

  // Every card in one pack shares its set, so the header names it once.
  const setCode = entries[0]?.setCode ?? "";

  const selections: OutsideGameSelection[] = useMemo(
    () =>
      Array.from(selected, (packSlot) => ({
        type: "BoosterPack" as const,
        data: { pack_slot: packSlot },
      })),
    [selected],
  );

  const minCount = data.up_to ? 0 : data.count;
  const countValid =
    selections.length >= minCount && selections.length <= data.count;

  const toggle = useCallback(
    (packSlot: number) => {
      setSelected((prev) => {
        const next = new Set(prev);
        if (next.has(packSlot)) {
          next.delete(packSlot);
        } else if (next.size < data.count) {
          next.add(packSlot);
        }
        return next;
      });
    },
    [data.count],
  );

  const handleConfirm = useCallback(() => {
    if (countValid) {
      dispatch({ type: "ChooseOutsideGameCards", data: { selections } });
    }
  }, [countValid, dispatch, selections]);

  return (
    <ChoiceOverlay
      title={t("boosterPack.title", { setCode })}
      subtitle={
        data.up_to
          ? t("boosterPack.subtitleUpTo", { count: data.count })
          : t("boosterPack.subtitleExact", { count: data.count })
      }
      footer={<ConfirmButton onClick={handleConfirm} disabled={!countValid} />}
    >
      <div className="flex max-h-[62vh] flex-wrap justify-center gap-2 overflow-y-auto p-2">
        {/* `collate_pack` deals the rare first so it falls inside the AI's
            `SELECTION_POOL_CAP` window; presentation reverses here so the
            pack reads commons-first with the rare last. */}
        {[...entries].reverse().map((entry, index) => {
          const isSelected = selected.has(entry.packSlot);
          return (
            <motion.button
              key={entry.packSlot}
              type="button"
              className={`relative rounded-lg transition ${
                isSelected
                  ? "z-10 ring-2 ring-emerald-400/80"
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 40, scale: 0.9 }}
              animate={{ opacity: isSelected ? 1 : 0.78, y: 0, scale: 1 }}
              transition={{ delay: index * 0.04, duration: 0.28 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => toggle(entry.packSlot)}
              aria-pressed={isSelected}
              aria-label={entry.name}
            >
              <CardImage
                cardName={entry.name}
                oracleId={entry.oracleId}
                faceIndex={0}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-emerald-500/20">
                  <span className="rounded-full bg-emerald-500/90 px-3 py-1 text-xs font-bold text-white">
                    {t("cardChoice.badges.choose")}
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}
