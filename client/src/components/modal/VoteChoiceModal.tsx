import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion } from "framer-motion";

import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import type { WaitingFor } from "../../adapter/types.ts";

type VoteChoice = Extract<WaitingFor, { type: "VoteChoice" }>;

/**
 * CR 701.38a/b: Council's-dilemma vote prompt. The engine collects one ballot
 * per vote; this modal renders the canonical choice list using the original-case
 * `data.option_labels` for display.
 *
 * Two ballot shapes (selection is always by index, so same-named object
 * candidates are disambiguated):
 *   - Named vote (`data.candidate_objects` empty): dispatch
 *     `ChooseOption { choice }` with the canonical lowercase option word.
 *   - Object-pool vote (`data.candidate_objects` non-empty — Council's
 *     Judgment, Prime Minister's Cabinet Room): dispatch
 *     `SubmitVoteCandidate { candidate_index }`.
 *
 * `data.actor` describes who submits the choice; `data.player` is the SUBJECT
 * being voted-for/labeled.
 *   - `{ type: "SubjectActs" }`: classic Council's-dilemma (the subject votes
 *     for themselves).
 *   - `{ type: "Delegated", data: <controller> }`: Battlebond friend-or-foe
 *     (no explicit CR section) — the controller labels each player one-by-one.
 * Labeling mode is `data.actor.type === "Delegated"`.
 *
 * Display layer only — `remaining_votes`, the running tally, and the queued
 * voter list all come straight from the engine's `WaitingFor::VoteChoice`. For
 * secret votes the engine delivers zeroed tallies, so the running-tally badge
 * is naturally hidden.
 */
export function VoteChoiceModal({ data }: { data: VoteChoice["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const [selected, setSelected] = useState<number | null>(null);
  // Frontend renders engine-provided state. The Player struct does not yet
  // carry a display name (would require lobby/persistence plumbing across
  // engine + server), so we fall back to the 1-indexed seat ordinal here.
  // Tracked as a follow-up; the engine-side `Player.name` field is the
  // correct long-term home for this label.
  const subjectName = t("voteChoice.subjectName", { number: data.player + 1 });
  const isObjectVote = data.candidate_objects.length > 0;

  const handleConfirm = useCallback(() => {
    if (selected === null) return;
    if (isObjectVote) {
      dispatch({ type: "SubmitVoteCandidate", data: { candidate_index: selected } });
    } else {
      const choice = data.options[selected];
      if (choice !== undefined) {
        dispatch({ type: "ChooseOption", data: { choice } });
      }
    }
    setSelected(null);
  }, [dispatch, selected, isObjectVote, data.options]);

  const isLabelingMode = data.actor.type === "Delegated";
  const title = isLabelingMode ? t("voteChoice.titleLabel") : t("voteChoice.titleVote");
  const subtitle = isLabelingMode
    ? t("voteChoice.subtitleLabel", { name: subjectName })
    : data.remaining_votes > 1
      ? t("voteChoice.subtitleVoteRemaining", { count: data.remaining_votes })
      : t("voteChoice.subtitleVote");

  return (
    <ChoiceOverlay
      title={title}
      subtitle={subtitle}
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-3xl"
      footer={<ConfirmButton onClick={handleConfirm} disabled={selected === null} />}
    >
      <div className="mx-auto mb-4 flex w-fit max-w-3xl flex-wrap items-center justify-center gap-3 sm:mb-6">
        {data.options.map((option, index) => {
          const label = data.option_labels[index] ?? option;
          const tally = data.tallies[index] ?? 0;
          const isSelected = selected === index;
          return (
            <motion.button
              key={index}
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
