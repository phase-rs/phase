import { useCallback, useRef, useState } from "react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { DungeonMapPopover } from "../hud/DungeonMapPopover.tsx";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import type {
  DungeonId,
  DungeonPreview,
  DungeonRoomView,
  RoomPreview,
  WaitingFor,
} from "../../adapter/types.ts";

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

/** Reshape a choice option into the map panel's view. CR 309.4a: the entry
 *  room is the topmost room, where the venture marker lands the moment this
 *  dungeon is chosen — so the preview marker sits exactly where choosing
 *  would put it. A field reshaping, not game logic: every value rendered is
 *  engine-authored. */
function previewView(option: DungeonPreview): DungeonRoomView {
  return {
    dungeon: option.dungeon,
    dungeon_name: option.name,
    room: option.entry_room,
    room_count: option.room_count,
    card: option.card,
    rooms: option.rooms,
  };
}

export function DungeonChoiceModal({ data }: { data: ChooseDungeon["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const [selected, setSelected] = useState<DungeonId | null>(null);
  // Hover and focus are tracked separately: a keyboard user tabs to an option
  // (focus opens its preview) and the preview must survive the pointer
  // brushing across and off the button. Hover wins while present; focus is
  // the fallback. Touch taps focus the button as they select, which is the
  // only preview gesture those inputs have.
  const [hovered, setHovered] = useState<DungeonId | null>(null);
  const [focused, setFocused] = useState<DungeonId | null>(null);
  const buttonEls = useRef(new Map<DungeonId, HTMLButtonElement>());

  const handleConfirm = useCallback(() => {
    if (selected !== null) {
      dispatch({ type: "ChooseDungeon", data: { dungeon: selected } });
    }
  }, [dispatch, selected]);

  const previewed = hovered ?? focused;
  const previewOption =
    previewed !== null
      ? (data.options.find((option) => option.dungeon === previewed) ?? null)
      : null;
  const anchorEl = previewed !== null ? (buttonEls.current.get(previewed) ?? null) : null;

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
              ref={(el) => {
                if (el) {
                  buttonEls.current.set(option.dungeon, el);
                } else {
                  buttonEls.current.delete(option.dungeon);
                }
              }}
              className={optionClassName(isSelected)}
              initial={{ opacity: 0, y: 20, scale: 0.95 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.05 + index * 0.03, duration: 0.25 }}
              whileHover={{ scale: 1.05 }}
              onMouseEnter={() => setHovered(option.dungeon)}
              onMouseLeave={() => setHovered(null)}
              onFocus={() => setFocused(option.dungeon)}
              onBlur={() => setFocused(null)}
              onClick={() => setSelected(isSelected ? null : option.dungeon)}
            >
              {/* CR 309.4a: the entry room fires the moment this dungeon is
                  chosen, so it is shown as part of the choice. */}
              <RoomOption room={option.entry_room} label={option.name} />
            </motion.button>
          );
        })}
      </div>
      {/* The panel portals to `document.body` above the modal (z-130 over the
          overlay's z-50), so DOM placement here is only about lifecycle. */}
      {previewOption !== null && anchorEl !== null ? (
        <DungeonMapPopover anchorEl={anchorEl} view={previewView(previewOption)} announceAllRooms />
      ) : null}
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
