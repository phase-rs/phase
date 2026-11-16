import { useState } from "react";
import { useTranslation } from "react-i18next";

import type { BracketReason, ExplainedCombo } from "../../types/bracket";
import { BracketPanelCardList, BracketPanelCardName } from "./BracketPanelCards";
import { comboOutcomesText, reasonExplanation, reasonTitle } from "./bracketPanelText";

interface Props {
  reason: BracketReason;
  onCardClick: (name: string) => void;
}

function ComboPieces({
  combo,
  onCardClick,
}: {
  combo: ExplainedCombo;
  onCardClick: (name: string) => void;
}) {
  return (
    <>
      <BracketPanelCardName card={combo.pieces[0]} onCardClick={onCardClick} />
      {" + "}
      <BracketPanelCardName card={combo.pieces[1]} onCardClick={onCardClick} />
    </>
  );
}

const SUBLINE_CLASS = "text-[10px] text-slate-400";

/** The collapsed subline, or nothing when the reason has nothing to list. */
function ReasonSubline({ reason, onCardClick }: Props) {
  const { t } = useTranslation("deck-builder");
  const { source } = reason;

  switch (source.kind) {
    case "card_rule":
      // `cards` is empty for a rule counted only from combo pairs (mass land
      // denial), so each line renders only when it has something to list.
      if (source.cards.length === 0 && source.combos.length === 0) return null;
      return (
        <div className={SUBLINE_CLASS}>
          {source.cards.length > 0 && (
            <div>
              <BracketPanelCardList cards={source.cards} joiner="comma" onCardClick={onCardClick} />
            </div>
          )}
          {source.combos.map((combo) => (
            <div key={`${combo.pieces[0].name}+${combo.pieces[1].name}`}>
              <ComboPieces combo={combo} onCardClick={onCardClick} />{" "}
              {t("bracket.card.reason.comboFromTable")}
            </div>
          ))}
        </div>
      );
    case "combo_floor": {
      if (source.combos.length === 0) return null;
      const early = source.trigger.kind === "early_two_card";
      return (
        <div className={SUBLINE_CLASS}>
          {source.combos.map((combo) => (
            <div key={`${combo.pieces[0].name}+${combo.pieces[1].name}`}>
              <ComboPieces combo={combo} onCardClick={onCardClick} />
              {` · ${comboOutcomesText(t, combo.outcomes)}`}
              {early && ` · ${t("bracket.card.combo.assembles", { cost: combo.assemble_cost })}`}
            </div>
          ))}
        </div>
      );
    }
    case "owner_combo_answer": {
      const { declaration } = source;
      if (declaration.kind !== "intended") return null;
      const key =
        declaration.window === "early_game"
          ? "bracket.card.reason.ownerIntendedEarly"
          : declaration.window === "late_game"
            ? "bracket.card.reason.ownerIntendedLate"
            : "bracket.card.reason.ownerIntended";
      return <div className={SUBLINE_CLASS}>{t(key)}</div>;
    }
    default: {
      const unreachable: never = source;
      return unreachable;
    }
  }
}

export function BracketPanelReason({ reason, onCardClick }: Props) {
  const { t } = useTranslation("deck-builder");
  const [open, setOpen] = useState(false);

  return (
    <div className="border-t border-white/10 py-1.5 first:border-t-0">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="flex min-h-[44px] w-full items-center justify-between gap-2 text-left text-xs text-slate-200 sm:min-h-0"
      >
        <span>{reasonTitle(t, reason)}</span>
        <span aria-hidden="true" className="text-slate-500">
          {open ? "⌄" : "›"}
        </span>
      </button>
      <ReasonSubline reason={reason} onCardClick={onCardClick} />
      {open && (
        <div className="mt-1 space-y-1 text-[10px] text-slate-400">
          <p>{reasonExplanation(t, reason)}</p>
          <blockquote className="border-l-2 border-white/10 pl-2 text-slate-500">
            {`“${reason.official_line}”`}
          </blockquote>
          <a
            href={reason.source_url}
            target="_blank"
            rel="noreferrer"
            className="inline-flex min-h-[44px] items-center text-slate-300 underline-offset-2 hover:underline sm:min-h-0"
          >
            {t("bracket.card.source", {
              document: reason.source_document,
              published: reason.source_published,
            })}
          </a>
        </div>
      )}
    </div>
  );
}
