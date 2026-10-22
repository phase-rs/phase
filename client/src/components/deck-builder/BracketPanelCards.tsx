import { Fragment } from "react";
import { useTranslation } from "react-i18next";

import type { BracketCard } from "../../types/bracket";

interface CardNameProps {
  card: BracketCard;
  onCardClick: (name: string) => void;
}

export function BracketPanelCardName({ card, onCardClick }: CardNameProps) {
  const { t } = useTranslation("deck-builder");

  return (
    <span className="inline-flex items-center gap-1">
      <button
        type="button"
        onClick={() => onCardClick(card.name)}
        className="inline-flex min-h-[44px] items-center text-slate-300 underline-offset-2 hover:underline sm:min-h-0"
      >
        {card.name}
      </button>
      {card.role === "commander" && (
        <span className="rounded-full border border-white/10 px-1.5 text-[10px] text-slate-400">
          {t("bracket.card.commanderTag")}
        </span>
      )}
    </span>
  );
}

interface CardListProps {
  cards: BracketCard[];
  joiner: "comma" | "or";
  onCardClick: (name: string) => void;
}

const JOINER_I18N_KEY: Record<CardListProps["joiner"], string> = {
  comma: "bracket.card.listComma",
  or: "bracket.card.listOr",
};

// A plain inline container, not flex: a flex container drops the spaces at the
// ends of the separator text nodes. Each name is already `inline-flex`, so it
// stays whole and the line wraps between names.
export function BracketPanelCardList({ cards, joiner, onCardClick }: CardListProps) {
  const { t } = useTranslation("deck-builder");
  const separator = t(JOINER_I18N_KEY[joiner]);

  return (
    <span>
      {cards.map((card, index) => (
        // A deck can list a card twice, so the index keeps the key unique.
        <Fragment key={`${card.role}-${card.name}-${index}`}>
          {index > 0 && separator}
          <BracketPanelCardName card={card} onCardClick={onCardClick} />
        </Fragment>
      ))}
    </span>
  );
}
