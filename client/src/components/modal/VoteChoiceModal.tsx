import { useCallback, useState } from "react";
import { motion } from "framer-motion";

import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import type { WaitingFor } from "../../adapter/types.ts";

type VoteChoice = Extract<WaitingFor, { type: "VoteChoice" }>;

/**
 * CR 701.38a: Council's-dilemma vote prompt. The engine collects one
 * `ChooseOption { choice }` action per vote; this modal renders the canonical
 * choice list (lowercase, from `data.options`) using the original-case
 * `data.option_labels` for display.
 *
 * Display layer only — `remaining_votes`, the running tally, and the queued
 * voter list all come straight from the engine's `WaitingFor::VoteChoice`.
 */
export function VoteChoiceModal({ data }: { data: VoteChoice["data"] }) {
  const dispatch = useGameDispatch();
  const [selected, setSelected] = useState<string | null>(null);

  const handleConfirm = useCallback(() => {
    if (selected !== null) {
      dispatch({ type: "ChooseOption", data: { choice: selected } });
      setSelected(null);
    }
  }, [dispatch, selected]);

  const subtitle =
    data.remaining_votes > 1
      ? `Cast a vote (${data.remaining_votes} remaining)`
      : "Cast your vote";

  return (
    <ChoiceOverlay
      title="Vote"
      subtitle={subtitle}
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-3xl"
      footer={<ConfirmButton onClick={handleConfirm} disabled={selected === null} />}
    >
      <div className="mx-auto mb-4 flex w-fit max-w-3xl flex-wrap items-center justify-center gap-3 sm:mb-6">
        {data.options.map((option, index) => {
          const label = data.option_labels[index] ?? option;
          const tally = data.tallies[index] ?? 0;
          const isSelected = selected === option;
          return (
            <motion.button
              key={option}
              className={`min-h-11 rounded-lg border-2 px-4 py-3 text-sm font-semibold transition sm:px-5 sm:text-base ${
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
              {label}
              {tally > 0 ? (
                <span className="ml-2 rounded bg-black/30 px-2 py-0.5 text-xs">
                  {tally}
                </span>
              ) : null}
            </motion.button>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}
