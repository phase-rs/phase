/**
 * The player's hand display. Cards can be clicked to open a context menu with
 * available actions (play land, cast, discard, bottom). Supports both the
 * mulligan phase (keep/bottom) and active play phase.
 */
import { useState, useRef, useEffect } from "react";
import { useTranslation } from "react-i18next";
import type { PlaytestCard } from "../../services/playtestSession";
import { isLandType, isPermanentType, cardColorClass } from "../../services/playtestSession";

interface HandCardProps {
  card: PlaytestCard;
  isSelected: boolean;
  phase: "mulligan" | "bottoming" | "playing" | "cleanup" | string;
  canPlayLand: boolean;
  availableMana: number;
  onAction: (action: CardAction, cardId: number) => void;
  onSelect: (id: number) => void;
}

type CardAction = "playLand" | "cast" | "discard" | "bottom";

interface ContextMenuEntry {
  action: CardAction;
  labelKey: string;
  color: string;
}

function getActions(card: PlaytestCard, phase: string, canPlayLand: boolean, availableMana: number): ContextMenuEntry[] {
  const types = card.face.cardType?.coreTypes ?? [];
  const isLand = isLandType(types);
  const isPerm = isPermanentType(types);
  const cmc = card.face.manaValue ?? 0;
  const canAfford = cmc <= availableMana;

  if (phase === "bottoming") {
    return [{ action: "bottom", labelKey: "card.bottom", color: "text-orange-300" }];
  }

  if (phase === "mulligan") {
    return [];
  }

  const actions: ContextMenuEntry[] = [];
  if (isLand && canPlayLand) {
    actions.push({ action: "playLand", labelKey: "card.playLand", color: "text-amber-300" });
  }
  if (!isLand && isPerm) {
    actions.push({
      action: "cast",
      labelKey: canAfford ? "card.cast" : "card.castNoMana",
      color: canAfford ? "text-blue-300" : "text-slate-500",
    });
  }
  if (!isLand && !isPerm) {
    actions.push({
      action: "cast",
      labelKey: canAfford ? "card.cast" : "card.castNoMana",
      color: canAfford ? "text-blue-300" : "text-slate-500",
    });
  }
  actions.push({ action: "discard", labelKey: "card.discard", color: "text-red-400" });
  return actions;
}

function CardContextMenu({
  actions,
  onClose,
  onAction,
}: {
  actions: ContextMenuEntry[];
  onClose: () => void;
  onAction: (action: CardAction) => void;
}) {
  const { t } = useTranslation("playtest");
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    function handler(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        onClose();
      }
    }
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [onClose]);

  return (
    <div
      ref={ref}
      className="absolute bottom-full left-1/2 z-50 mb-1 w-36 -translate-x-1/2 rounded-xl border border-white/10 bg-gray-900 py-1 shadow-xl"
    >
      {actions.map((entry) => (
        <button
          key={entry.action}
          type="button"
          onClick={() => { onAction(entry.action); onClose(); }}
          className={`w-full px-3 py-1.5 text-left text-xs hover:bg-white/8 ${entry.color}`}
        >
          {t(entry.labelKey)}
        </button>
      ))}
    </div>
  );
}

function HandCard({ card, isSelected, phase, canPlayLand, availableMana, onAction, onSelect }: HandCardProps) {
  const [menuOpen, setMenuOpen] = useState(false);
  const types = card.face.cardType?.coreTypes ?? [];
  const isLand = isLandType(types);
  const cmc = card.face.manaValue;
  const actions = getActions(card, phase, canPlayLand, availableMana);

  function handleClick() {
    if (phase === "bottoming") {
      onAction("bottom", card.id);
      return;
    }
    if (actions.length === 0) return;
    if (actions.length === 1) {
      onAction(actions[0].action, card.id);
    } else {
      onSelect(card.id);
      setMenuOpen(true);
    }
  }

  return (
    <div className="relative">
      <button
        type="button"
        onClick={handleClick}
        title={card.face.name}
        className={`relative flex h-28 w-20 flex-col overflow-hidden rounded-xl border text-left shadow-md transition-all ${
          isSelected
            ? "border-blue-400 ring-1 ring-blue-400/50"
            : phase === "bottoming"
            ? "cursor-pointer border-orange-400/50 hover:border-orange-400"
            : "border-white/10 hover:border-white/30"
        } bg-gradient-to-b ${
          isLand
            ? "from-amber-900/40 to-amber-950/60"
            : "from-slate-800/60 to-slate-900/80"
        }`}
      >
        {/* CMC badge */}
        {!isLand && cmc !== null && cmc !== undefined && (
          <span className="absolute right-1 top-1 rounded-md bg-black/40 px-1 text-[0.55rem] font-bold text-white">
            {cmc}
          </span>
        )}

        {/* Color bar */}
        <div
          className={`h-1 w-full ${
            isLand ? "bg-amber-500/60" : "bg-blue-500/40"
          }`}
        />

        {/* Card name */}
        <div className="flex flex-1 flex-col justify-between p-1.5">
          <span
            className={`line-clamp-2 text-[0.6rem] font-semibold leading-tight ${cardColorClass(card)}`}
          >
            {card.face.name}
          </span>
          <span className="text-[0.5rem] text-slate-500">
            {types.slice(0, 2).join(" · ")}
          </span>
        </div>
      </button>

      {menuOpen && actions.length > 0 && (
        <CardContextMenu
          actions={actions}
          onClose={() => { setMenuOpen(false); onSelect(-1); }}
          onAction={(a) => onAction(a, card.id)}
        />
      )}
    </div>
  );
}

interface PlaytestHandProps {
  hand: PlaytestCard[];
  phase: string;
  canPlayLand: boolean;
  availableMana: number;
  selectedCardId: number | null;
  bottomingRequired: number;
  onPlayLand: (id: number) => void;
  onCast: (id: number) => void;
  onDiscard: (id: number) => void;
  onBottom: (id: number) => void;
  onSelect: (id: number | null) => void;
}

export function PlaytestHand({
  hand,
  phase,
  canPlayLand,
  availableMana,
  selectedCardId,
  bottomingRequired,
  onPlayLand,
  onCast,
  onDiscard,
  onBottom,
  onSelect,
}: PlaytestHandProps) {
  const { t } = useTranslation("playtest");

  function handleAction(action: CardAction, cardId: number) {
    switch (action) {
      case "playLand": onPlayLand(cardId); break;
      case "cast":     onCast(cardId);     break;
      case "discard":  onDiscard(cardId);  break;
      case "bottom":   onBottom(cardId);   break;
    }
  }

  if (hand.length === 0) {
    return (
      <div className="flex h-32 items-center justify-center rounded-xl border border-white/8 text-sm text-slate-500">
        {phase === "mulligan" ? t("hand.emptyMulligan") : t("hand.empty")}
      </div>
    );
  }

  return (
    <div className="rounded-xl border border-white/8 bg-black/10 p-3">
      {phase === "bottoming" && bottomingRequired > 0 && (
        <div className="mb-2 text-center text-xs text-orange-300">
          {t("hand.bottomPrompt", { count: bottomingRequired })}
        </div>
      )}
      <div className="flex flex-wrap gap-2 justify-center">
        {hand.map((card) => (
          <HandCard
            key={card.id}
            card={card}
            isSelected={selectedCardId === card.id}
            phase={phase}
            canPlayLand={canPlayLand}
            availableMana={availableMana}
            onAction={handleAction}
            onSelect={(id) => onSelect(id === selectedCardId ? null : id)}
          />
        ))}
      </div>
    </div>
  );
}
