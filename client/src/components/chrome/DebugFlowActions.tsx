import { useTranslation } from "react-i18next";
import { useState } from "react";

import type { DebugAction, Phase, PlayerId } from "../../adapter/types";
import {
  AccordionItem,
  FieldRow,
  PlayerSelect,
  SelectInput,
  SubmitButton,
  useAccordion,
} from "./debugFields";

const PHASES: readonly Phase[] = [
  "Untap",
  "Upkeep",
  "Draw",
  "PreCombatMain",
  "BeginCombat",
  "DeclareAttackers",
  "DeclareBlockers",
  "CombatDamage",
  "EndCombat",
  "PostCombatMain",
  "End",
  "Cleanup",
] as const;

interface Props {
  onDispatch: (action: DebugAction) => void;
}

function SetPhaseForm({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const [phase, setPhase] = useState<Phase>("PreCombatMain");
  const [activePlayer, setActivePlayer] = useState<PlayerId>(0);

  return (
    <>
      <FieldRow label={t("sandboxTools.phase")}>
        <SelectInput value={phase} onChange={setPhase} options={PHASES} />
      </FieldRow>
      <FieldRow label={t("sandboxTools.activePlayer")}>
        <PlayerSelect value={activePlayer} onChange={setActivePlayer} />
      </FieldRow>
      <SubmitButton
        onClick={() =>
          onDispatch({ type: "SetPhase", data: { phase, active_player: activePlayer } })
        }
      >
        {t("sandboxTools.setPhase")}
      </SubmitButton>
    </>
  );
}

export function DebugFlowActions({ onDispatch }: Props) {
  const { t } = useTranslation("game");
  const { expanded, toggle } = useAccordion();

  return (
    <div>
      <AccordionItem label={t("sandboxTools.setPhase")} expanded={expanded === "phase"} onToggle={() => toggle("phase")}>
        <SetPhaseForm onDispatch={onDispatch} />
      </AccordionItem>
      <AccordionItem label={t("sandboxTools.runSBAs")} expanded={expanded === "sba"} onToggle={() => toggle("sba")}>
        <SubmitButton onClick={() => onDispatch({ type: "RunStateBasedActions" })}>
          {t("sandboxTools.runStateBasedActions")}
        </SubmitButton>
      </AccordionItem>
    </div>
  );
}
