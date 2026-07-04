import { useCallback, useState } from "react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import type { DoorLockOp, RoomDoor, WaitingFor } from "../../adapter/types.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";

type ChooseRoomDoor = Extract<WaitingFor, { type: "ChooseRoomDoor" }>;

// CR 709.5c: i18n keys for each door (half) of a Room permanent.
const DOOR_LABEL_KEYS: Record<RoomDoor, string> = {
  Left: "roomDoor.left",
  Right: "roomDoor.right",
};

// CR 709.5f-g: i18n keys for the operation applied to the chosen half. A
// resolving prompt never offers `LockOrUnlock` as a leaf option (the engine
// expands it into concrete `Unlock`/`Lock` entries per eligible door), but the
// key is mapped for exhaustiveness over the typed union.
const OP_LABEL_KEYS: Record<DoorLockOp["type"], string> = {
  Unlock: "roomDoor.unlock",
  Lock: "roomDoor.lock",
  LockOrUnlock: "roomDoor.lockOrUnlock",
};

export function RoomDoorChoiceModal({
  data,
}: {
  data: ChooseRoomDoor["data"];
}) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  // CR 709.5f-g: select by option index — the same door can appear under both
  // Lock and Unlock in a "lock or unlock" prompt, so (op, door) is the unit of
  // choice, not the door alone.
  const [selected, setSelected] = useState<number | null>(null);

  const handleConfirm = useCallback(() => {
    if (selected !== null) {
      const [op, door] = data.options[selected];
      dispatch({
        type: "ChooseRoomDoor",
        data: { object_id: data.object_id, op, door },
      });
    }
  }, [dispatch, selected, data.options, data.object_id]);

  return (
    <ChoiceOverlay
      title={t("roomDoor.title")}
      subtitle={t("roomDoor.subtitle")}
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-3xl"
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={selected === null}
          label={t("roomDoor.confirm")}
        />
      }
    >
      <div className="mx-auto mb-6 flex w-fit max-w-3xl flex-wrap items-center justify-center gap-3 sm:mb-10">
        {data.options.map(([op, door], index) => {
          const isSelected = selected === index;
          return (
            <motion.button
              key={`${op.type}-${door}`}
              type="button"
              className={`min-h-11 rounded-lg border-2 px-4 py-3 text-sm font-semibold transition sm:px-5 sm:text-base ${
                isSelected
                  ? "border-emerald-400 bg-emerald-500/30 text-white"
                  : "border-gray-600 bg-gray-800/80 text-gray-300 hover:border-gray-400 hover:text-white"
              }`}
              initial={{ opacity: 0, y: 20, scale: 0.95 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.05 + index * 0.03, duration: 0.25 }}
              whileHover={{ scale: 1.05 }}
              onClick={() => setSelected(isSelected ? null : index)}
            >
              {t("roomDoor.option", {
                op: t(OP_LABEL_KEYS[op.type]),
                door: t(DOOR_LABEL_KEYS[door]),
              })}
            </motion.button>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}
