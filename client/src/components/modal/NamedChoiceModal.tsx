import { useCallback, useState } from "react";
import { motion } from "framer-motion";

import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import type { WaitingFor } from "../../adapter/types.ts";

type NamedChoice = Extract<WaitingFor, { type: "NamedChoice" }>;

const CHOICE_TYPE_LABELS: Record<string, string> = {
  CreatureType: "Choose a Creature Type",
  Color: "Choose a Color",
  OddOrEven: "Choose Odd or Even",
  BasicLandType: "Choose a Basic Land Type",
  CardType: "Choose a Card Type",
};

export function NamedChoiceModal({ data }: { data: NamedChoice["data"] }) {
  const dispatch = useGameDispatch();
  const [selected, setSelected] = useState<string | null>(null);

  const handleConfirm = useCallback(() => {
    if (selected !== null) {
      dispatch({ type: "ChooseOption", data: { choice: selected } });
    }
  }, [dispatch, selected]);

  const title = CHOICE_TYPE_LABELS[data.choice_type] ?? "Make a Choice";

  return (
    <ChoiceOverlay title={title} subtitle="Select one option">
      <div className="mb-6 flex w-full max-w-3xl flex-wrap items-center justify-center gap-3 sm:mb-10">
        {data.options.map((option, index) => {
          const isSelected = selected === option;
          return (
            <motion.button
              key={option}
              className={`rounded-lg border-2 px-5 py-3 text-base font-semibold transition ${
                isSelected
                  ? "border-emerald-400 bg-emerald-500/30 text-white"
                  : "border-gray-600 bg-gray-800/80 text-gray-300 hover:border-gray-400 hover:text-white"
              }`}
              initial={{ opacity: 0, y: 20, scale: 0.95 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.05 + index * 0.03, duration: 0.25 }}
              whileHover={{ scale: 1.05 }}
              onClick={() => setSelected(isSelected ? null : option)}
            >
              {option}
            </motion.button>
          );
        })}
      </div>
      <ConfirmButton onClick={handleConfirm} disabled={selected === null} />
    </ChoiceOverlay>
  );
}
