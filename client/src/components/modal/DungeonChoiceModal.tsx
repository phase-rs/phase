import { useCallback, useState } from "react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import type { DungeonId, WaitingFor } from "../../adapter/types.ts";

type ChooseDungeon = Extract<WaitingFor, { type: "ChooseDungeon" }>;
type ChooseDungeonRoom = Extract<WaitingFor, { type: "ChooseDungeonRoom" }>;

const DUNGEON_DISPLAY_NAMES: Record<DungeonId, string> = {
  LostMineOfPhandelver: "Lost Mine of Phandelver",
  DungeonOfTheMadMage: "Dungeon of the Mad Mage",
  TombOfAnnihilation: "Tomb of Annihilation",
  Undercity: "Undercity",
  BaldursGateWilderness: "Baldur's Gate Wilderness",
};

export function DungeonChoiceModal({ data }: { data: ChooseDungeon["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const [selected, setSelected] = useState<DungeonId | null>(null);

  const handleConfirm = useCallback(() => {
    if (selected !== null) {
      dispatch({ type: "ChooseDungeon", data: { dungeon: selected } });
    }
  }, [dispatch, selected]);

  return (
    <ChoiceOverlay
      title={t("dungeonChoice.title")}
      subtitle={t("dungeonChoice.subtitle")}
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-3xl"
      footer={<ConfirmButton onClick={handleConfirm} disabled={selected === null} />}
    >
      <div className="mx-auto mb-6 flex w-fit max-w-3xl flex-wrap items-center justify-center gap-3 sm:mb-10">
        {data.options.map((dungeonId, index) => {
          const isSelected = selected === dungeonId;
          return (
            <motion.button
              key={dungeonId}
              className={`min-h-11 rounded-lg border-2 px-4 py-3 text-sm font-semibold transition sm:px-5 sm:text-base ${
                isSelected
                  ? "border-emerald-400 bg-emerald-500/30 text-white"
                  : "border-gray-600 bg-gray-800/80 text-gray-300 hover:border-gray-400 hover:text-white"
              }`}
              initial={{ opacity: 0, y: 20, scale: 0.95 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.05 + index * 0.03, duration: 0.25 }}
              whileHover={{ scale: 1.05 }}
              onClick={() => setSelected(isSelected ? null : dungeonId)}
            >
              {DUNGEON_DISPLAY_NAMES[dungeonId] ?? dungeonId}
            </motion.button>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}

export function RoomChoiceModal({ data }: { data: ChooseDungeonRoom["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const [selectedIndex, setSelectedIndex] = useState<number | null>(null);

  const handleConfirm = useCallback(() => {
    if (selectedIndex !== null) {
      dispatch({ type: "ChooseDungeonRoom", data: { room_index: data.options[selectedIndex] } });
    }
  }, [dispatch, selectedIndex, data.options]);

  const dungeonName = DUNGEON_DISPLAY_NAMES[data.dungeon] ?? data.dungeon;

  return (
    <ChoiceOverlay
      title={t("dungeonChoice.roomTitle")}
      subtitle={t("dungeonChoice.roomSubtitle", { name: dungeonName })}
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-3xl"
      footer={<ConfirmButton onClick={handleConfirm} disabled={selectedIndex === null} />}
    >
      <div className="mx-auto mb-6 flex w-fit max-w-3xl flex-wrap items-center justify-center gap-3 sm:mb-10">
        {data.option_names.map((roomName, index) => {
          const isSelected = selectedIndex === index;
          return (
            <motion.button
              key={data.options[index]}
              className={`min-h-11 rounded-lg border-2 px-4 py-3 text-sm font-semibold transition sm:px-5 sm:text-base ${
                isSelected
                  ? "border-emerald-400 bg-emerald-500/30 text-white"
                  : "border-gray-600 bg-gray-800/80 text-gray-300 hover:border-gray-400 hover:text-white"
              }`}
              initial={{ opacity: 0, y: 20, scale: 0.95 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.05 + index * 0.03, duration: 0.25 }}
              whileHover={{ scale: 1.05 }}
              onClick={() => setSelectedIndex(isSelected ? null : index)}
            >
              {roomName}
            </motion.button>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}
