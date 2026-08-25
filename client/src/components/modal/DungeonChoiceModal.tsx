import { useCallback, useState } from "react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import type { DungeonId, RoomPreview, WaitingFor } from "../../adapter/types.ts";

type ChooseDungeon = Extract<WaitingFor, { type: "ChooseDungeon" }>;
type ChooseDungeonRoom = Extract<WaitingFor, { type: "ChooseDungeonRoom" }>;

/** Shared card body: the room's printed name over its printed effect.
 *  Both strings come from the engine (CR 309.4b-c); the client only lays
 *  them out. */
function RoomOption({ room, label }: { room: RoomPreview; label?: string }) {
  return (
    <>
      <div className="text-sm font-semibold sm:text-base">{label ?? room.name}</div>
      {label ? (
        <div className="mt-1 text-[11px] font-medium uppercase tracking-wide text-gray-400 sm:text-xs">
          {room.name}
        </div>
      ) : null}
      {room.text ? (
        <div className="mt-1.5 text-xs font-normal leading-snug text-gray-300 sm:text-sm">
          {room.text}
        </div>
      ) : null}
    </>
  );
}

const OPTION_CLASSES = "flex min-h-11 w-64 max-w-full flex-col rounded-lg border-2 px-4 py-3 text-left transition sm:px-5";

function optionClassName(isSelected: boolean) {
  return `${OPTION_CLASSES} ${
    isSelected
      ? "border-emerald-400 bg-emerald-500/30 text-white"
      : "border-gray-600 bg-gray-800/80 text-gray-300 hover:border-gray-400 hover:text-white"
  }`;
}

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
      <div className="mx-auto mb-6 flex w-fit max-w-3xl flex-wrap items-stretch justify-center gap-3 sm:mb-10">
        {data.options.map((option, index) => {
          const isSelected = selected === option.dungeon;
          return (
            <motion.button
              key={option.dungeon}
              className={optionClassName(isSelected)}
              initial={{ opacity: 0, y: 20, scale: 0.95 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.05 + index * 0.03, duration: 0.25 }}
              whileHover={{ scale: 1.05 }}
              onClick={() => setSelected(isSelected ? null : option.dungeon)}
            >
              {/* CR 309.4a: the entry room fires the moment this dungeon is
                  chosen, so it is shown as part of the choice. */}
              <RoomOption room={option.entry_room} label={option.name} />
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
      dispatch({ type: "ChooseDungeonRoom", data: { room_index: data.options[selectedIndex].index } });
    }
  }, [dispatch, selectedIndex, data.options]);

  return (
    <ChoiceOverlay
      title={t("dungeonChoice.roomTitle")}
      subtitle={t("dungeonChoice.roomSubtitle", { name: data.dungeon_name })}
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-3xl"
      footer={<ConfirmButton onClick={handleConfirm} disabled={selectedIndex === null} />}
    >
      <div className="mx-auto mb-6 flex w-fit max-w-3xl flex-wrap items-stretch justify-center gap-3 sm:mb-10">
        {data.options.map((room, index) => {
          const isSelected = selectedIndex === index;
          return (
            <motion.button
              key={room.index}
              className={optionClassName(isSelected)}
              initial={{ opacity: 0, y: 20, scale: 0.95 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.05 + index * 0.03, duration: 0.25 }}
              whileHover={{ scale: 1.05 }}
              onClick={() => setSelectedIndex(isSelected ? null : index)}
            >
              <RoomOption room={room} />
            </motion.button>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}
