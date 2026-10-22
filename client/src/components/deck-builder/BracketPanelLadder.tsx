import { useTranslation } from "react-i18next";

import {
  BRACKET_TIER_CHIP_CLASS,
  BRACKET_TIER_NUMERIC,
  type BracketCut,
  type BracketLadderStep,
} from "../../types/bracket";
import { BracketPanelCardList, BracketPanelCardName } from "./BracketPanelCards";
import { reasonKey, reasonTitle } from "./bracketPanelText";

interface Props {
  steps: BracketLadderStep[];
  onCardClick: (name: string) => void;
}

function LadderCut({ cut, onCardClick }: { cut: BracketCut; onCardClick: (name: string) => void }) {
  const { t } = useTranslation("deck-builder");

  switch (cut.kind) {
    case "remove_cards":
      return (
        <div>
          {t(
            cut.count === cut.from.length
              ? "bracket.card.ladder.removeAll"
              : "bracket.card.ladder.removeSome",
            { count: cut.count },
          )}{" "}
          <BracketPanelCardList cards={cut.from} joiner="comma" onCardClick={onCardClick} />
        </div>
      );
    case "break_combo":
      return (
        <div>
          <div>
            {t("bracket.card.ladder.breakCombo")}{" "}
            <BracketPanelCardName card={cut.combo.pieces[0]} onCardClick={onCardClick} />
            {" + "}
            <BracketPanelCardName card={cut.combo.pieces[1]} onCardClick={onCardClick} />
          </div>
          <div>
            {t(
              cut.remove_one_of.length === 1
                ? "bracket.card.ladder.removeThis"
                : "bracket.card.ladder.removeOneOf",
            )}{" "}
            <BracketPanelCardList cards={cut.remove_one_of} joiner="or" onCardClick={onCardClick} />
          </div>
        </div>
      );
    case "replace_commander":
      return (
        <div>
          {t("bracket.card.ladder.replaceCommander", { count: cut.count })}{" "}
          <BracketPanelCardList cards={cut.of} joiner="comma" onCardClick={onCardClick} />
        </div>
      );
    case "change_combo_answer":
      return <div>{t("bracket.card.ladder.changeComboAnswer")}</div>;
    default: {
      const unreachable: never = cut;
      return unreachable;
    }
  }
}

export function BracketPanelLadder({ steps, onCardClick }: Props) {
  const { t } = useTranslation("deck-builder");

  return (
    <div className="text-xs text-slate-200">
      {steps.map((step) => (
        <div key={step.lands_on} className="flex gap-2 border-t border-white/10 py-1.5 first:border-t-0">
          <span
            className={`h-fit shrink-0 rounded-full border px-1.5 py-0.5 text-[10px] font-semibold ${BRACKET_TIER_CHIP_CLASS[step.lands_on]}`}
          >
            {t("bracket.card.tierShort", { tier: BRACKET_TIER_NUMERIC[step.lands_on] })}
          </span>
          <div className="min-w-0 flex-1 space-y-1">
            {step.reasons.map((reason) => (
              <div key={reasonKey(reason)}>
                <div>{reasonTitle(t, reason)}</div>
                <div className="space-y-0.5 text-[10px] text-slate-400">
                  {reason.cuts.map((cut, cutIndex) => (
                    <LadderCut key={`${cut.kind}-${cutIndex}`} cut={cut} onCardClick={onCardClick} />
                  ))}
                </div>
              </div>
            ))}
          </div>
        </div>
      ))}
      <p className="border-t border-white/10 pt-1.5 text-[10px] text-slate-500 first:border-t-0 first:pt-0">
        {t("bracket.card.ladder.footnote")}
      </p>
    </div>
  );
}
