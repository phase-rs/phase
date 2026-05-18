import type { GameAction, WaitingFor } from "../../adapter/types.ts";
import { additionalCostChoices } from "../../viewmodel/costLabel.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type OptionalCostWaitingFor = Extract<WaitingFor, { type: "OptionalCostChoice" }>;

interface OptionalCostModalProps {
  waitingFor: OptionalCostWaitingFor;
  dispatch: (action: GameAction) => void | Promise<void>;
}

/**
 * Modal for `WaitingFor::OptionalCostChoice` — kicker / multikicker / Casualty
 * / "or pay" additional-cost prompts.
 *
 * The decline option is an explicit, descriptively-labelled primary button
 * (`pay: false` → finish the cast), kept distinct from the X / backdrop abort
 * affordance (`CancelCast` → cancel the cast, CR 601.2). For repeatable
 * multikicker (CR 702.33c/d) the `times_kicked` count drives the title and
 * the "Done — finish casting (kicked N×)" decline label.
 */
export function OptionalCostModalContent({
  waitingFor,
  dispatch,
}: OptionalCostModalProps) {
  const { cost, times_kicked } = waitingFor.data;
  const { title, options } = additionalCostChoices(cost, times_kicked);
  // Mandatory Choice costs (e.g. "discard a card or pay 3 life") require
  // picking one — no abort allowed. All other costs allow aborting the cast.
  const isMandatoryChoice = cost.type === "Choice";

  return (
    <ChoiceModal
      title={title}
      options={options}
      onChoose={(id) =>
        dispatch({ type: "DecideOptionalCost", data: { pay: id === "pay" } })
      }
      onClose={isMandatoryChoice ? undefined : () => dispatch({ type: "CancelCast" })}
    />
  );
}
