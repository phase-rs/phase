import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { useCardImage } from "../../hooks/useCardImage";
import { useDraftStore } from "../../stores/draftStore";
import type { DraftCardInstance, DraftPlayerView } from "../../adapter/draft-adapter";
import type { CardHoverInfo } from "../card/CardPreview";

const EMPTY_DRAFT_EFFECTS: DraftCardInstance[] = [];

// ── Card tile ───────────────────────────────────────────────────────────

interface PackCardProps {
  card: DraftCardInstance;
  isSelected: boolean;
  onSelect: (instanceId: string) => void;
  onConfirm: () => void;
  confirmDisabled?: boolean;
  onHover: (info: CardHoverInfo | null) => void;
}

function PackCard({
  card,
  isSelected,
  onSelect,
  onConfirm,
  confirmDisabled = false,
  onHover,
}: PackCardProps) {
  const { t } = useTranslation("draft");
  const { src, isLoading } = useCardImage(card.name, {
    size: "normal",
    sourcePrinting: { setCode: card.set_code, collectorNumber: card.collector_number },
  });

  return (
    <div
      className={`relative cursor-pointer overflow-hidden rounded-[14px] transition-all duration-150 ${
        isSelected
          ? "z-10 scale-105 ring-2 ring-amber-400 shadow-lg shadow-amber-400/20"
          : "ring-1 ring-white/10 hover:scale-[1.02] hover:ring-white/20"
      }`}
      onMouseEnter={() => onHover({ name: card.name, sourcePrinting: { setCode: card.set_code, collectorNumber: card.collector_number } })}
      onMouseLeave={() => onHover(null)}
    >
      <button
        onClick={() => onSelect(card.instance_id)}
        className="w-full"
      >
        {isLoading || !src ? (
          <div className="flex aspect-[488/680] animate-pulse items-center justify-center bg-white/5">
            <span className="px-2 text-center text-xs text-white/40">{card.name}</span>
          </div>
        ) : (
          <img
            src={src}
            alt={card.name}
            draggable={false}
            className="aspect-[488/680] w-full object-cover"
          />
        )}
      </button>
      <div className="absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/80 to-transparent px-3 py-2">
        {isSelected ? (
          <button
            onClick={onConfirm}
            disabled={confirmDisabled}
            className="w-full rounded-lg bg-amber-500 py-0.5 text-xs font-semibold text-black transition-colors hover:bg-amber-400 disabled:cursor-not-allowed disabled:opacity-50"
          >
            {t("pack.confirmPick")}
          </button>
        ) : (
          <span className="line-clamp-1 text-[10px] leading-tight text-white/80">
            {card.name}
          </span>
        )}
      </div>
    </div>
  );
}

// ── Rarity helpers ─────────────────────────────────────────────────────

const RARITY_ORDER = ["mythic", "rare", "uncommon", "common"] as const;

const RARITY_LABELS: Record<string, string> = {
  mythic: "Mythic Rare",
  rare: "Rare",
  uncommon: "Uncommon",
  common: "Common",
};

// Distinct hues per rarity (mythic burnt-orange vs rare pale-gold reads at a glance),
// with a matching left-rule accent on the section header.
const RARITY_STYLES: Record<string, { text: string; accent: string }> = {
  mythic: { text: "text-orange-400", accent: "border-orange-400/55" },
  rare: { text: "text-amber-200", accent: "border-amber-300/45" },
  uncommon: { text: "text-slate-300", accent: "border-slate-300/30" },
  common: { text: "text-white/45", accent: "border-white/15" },
};

const RARITY_STYLE_FALLBACK = { text: "text-white/45", accent: "border-white/15" } as const;

function groupByRarity(cards: DraftCardInstance[]) {
  const groups: [string, DraftCardInstance[]][] = [];
  for (const rarity of RARITY_ORDER) {
    const matched = cards.filter((c) => c.rarity === rarity);
    if (matched.length > 0) groups.push([rarity, matched]);
  }
  const unmatched = cards.filter(
    (c) => !RARITY_ORDER.includes(c.rarity as (typeof RARITY_ORDER)[number]),
  );
  if (unmatched.length > 0) groups.push(["other", unmatched]);
  return groups;
}

// ── Main component ──────────────────────────────────────────────────────

interface PackDisplayProps {
  onCardHover: (info: CardHoverInfo | null) => void;
  /** Show the "Auto-pick" button when the active draft mode supports it. */
  showAutoPick?: boolean;
  /** Show draft-effect controls for the local Draft page. */
  enableDraftEffects?: boolean;
  view?: DraftPlayerView | null;
  selectedCard?: string | null;
  onSelectCard?: (instanceId: string | null) => void;
  /** Receives one whole CR 903.13b pick step: every selected card, primary first. */
  onConfirmPick?: (cardInstanceIds: string[]) => Promise<void> | void;
  onPickWithDraftEffect?: (effectCardInstanceId: string, cardInstanceIds: string[]) => Promise<void> | void;
  onAutoPick?: () => Promise<void> | void;
}

export function PackDisplay({
  onCardHover,
  showAutoPick = false,
  enableDraftEffects = false,
  view: viewOverride,
  selectedCard: selectedCardOverride,
  onSelectCard,
  onConfirmPick,
  onPickWithDraftEffect,
  onAutoPick,
}: PackDisplayProps) {
  const { t } = useTranslation("draft");
  const quickView = useDraftStore((s) => s.view);
  const quickSelectedCard = useDraftStore((s) => s.selectedCard);
  const quickSelectCard = useDraftStore((s) => s.selectCard);
  const quickConfirmPick = useDraftStore((s) => s.confirmPick);
  const quickPickCardWithDraftEffect = useDraftStore((s) => s.pickCardWithDraftEffect);
  const quickAutoPickCard = useDraftStore((s) => s.autoPickCard);
  const [autoPicking, setAutoPicking] = useState(false);

  const view = viewOverride !== undefined ? viewOverride : quickView;
  const selectedCard = selectedCardOverride !== undefined
    ? selectedCardOverride
    : quickSelectedCard;
  // The primary is a selection only while the card it names is still in the
  // pack on screen. Its owner (the store) nulls it ONLY when this client
  // submits, so a step advanced from the outside — a server auto-pick on
  // timeout, the documented default under `PodPolicy::Competitive`, or the P2P
  // host's timer sweep — pushes a new view with the old id left behind. At
  // `requiredCount > 1` that stale id is unrecoverable by clicking: the deselect
  // arm below is unsatisfiable for every card in the new pack, so every click
  // lands in an additional slot and the step can only ever dispatch the dead id.
  // The gate is pack membership rather than step identity because membership is
  // the total condition — it also covers a pack replaced within one step and a
  // primary invalidated by any other cause — and it routes the stale case into
  // the same `!primaryCard` arm the guest's post-submit window already uses.
  const primaryCard =
    selectedCard && view?.current_pack?.some((card) => card.instance_id === selectedCard)
      ? selectedCard
      : null;
  const selectCard = onSelectCard ?? quickSelectCard;
  const confirmPick = onConfirmPick ?? quickConfirmPick;
  const pickCardWithDraftEffect = onPickWithDraftEffect ?? quickPickCardWithDraftEffect;
  const autoPickCard = onAutoPick ?? quickAutoPickCard;
  const [activeDraftEffect, setActiveDraftEffect] = useState<string | null>(null);
  // Selections beyond the primary `selectedCard`, in click order. A list, not a
  // nullable single: a Commander pick step takes two (CR 903.13b) and a draft
  // effect takes two, and the shipped `string | null` was the N = 2 special case.
  const [additionalCards, setAdditionalCards] = useState<string[]>([]);

  useEffect(() => {
    if (view?.current_pack?.length === 1 && !primaryCard) {
      selectCard(view.current_pack[0].instance_id);
    }
  }, [view?.current_pack, primaryCard, selectCard]);

  const draftEffects = view?.draft_effects ?? EMPTY_DRAFT_EFFECTS;

  useEffect(() => {
    if (
      activeDraftEffect &&
      !draftEffects.some((card) => card.instance_id === activeDraftEffect)
    ) {
      setActiveDraftEffect(null);
    }
  }, [activeDraftEffect, draftEffects]);

  useEffect(() => {
    if (!activeDraftEffect) setAdditionalCards([]);
  }, [activeDraftEffect]);

  // The step's identity is the engine's `(pack number, pick number)`, never
  // `current_pack`'s array identity: a guest receives a fresh array object on
  // every `draft_state_update` for the SAME step, and clearing on that would
  // wipe a half-made selection mid-pick.
  useEffect(() => {
    setAdditionalCards([]);
  }, [view?.current_pack_number, view?.pick_number]);

  if (!view) return null;

  const pack = view.current_pack;

  // CR 903.13b's per-seat pick-step count, read from the engine and never
  // re-derived: `view.required_pick_count` is `min(cards_per_pick, pack size)`,
  // so it is 1 for the four CR 905.1a kinds, 2 for a Commander pod, and 1 again
  // on an odd pack's final step — a distinction no per-kind lookup can make.
  //
  // A draft effect is the one case that is NOT the procedure's count: the engine
  // hard-bounds `apply_pick_with_effect_inner` at two cards and
  // `validateDraftEffectPick` requires exactly two, so the effect branch keeps
  // that authority rather than borrowing the published one.
  const requiredCount = activeDraftEffect ? 2 : view.required_pick_count;
  // Primary first, then the additional slots in click order — the order
  // `apply_pick_inner` receives. The additionals can never outlive their
  // primary, so a null primary means nothing is selected.
  const selectedIds = primaryCard ? [primaryCard, ...additionalCards] : [];

  if (!pack || pack.length === 0) {
    return (
      <div className="flex items-center justify-center py-12 text-white/40">
        {t("pack.waitingNext")}
      </div>
    );
  }

  const handleAutoPick = async () => {
    setAutoPicking(true);
    try {
      await autoPickCard();
    } finally {
      setAutoPicking(false);
    }
  };

  const handleConfirmPick = async () => {
    // The engine enforces the exact count (`apply_pick_inner`); this gate only
    // keeps the UI from dispatching a submission it already knows is wrong.
    if (selectedIds.length !== requiredCount) return;
    if (activeDraftEffect) {
      await pickCardWithDraftEffect(activeDraftEffect, selectedIds);
      setActiveDraftEffect(null);
      setAdditionalCards([]);
      return;
    }
    await confirmPick(selectedIds);
  };

  const handleSelectCard = (instanceId: string) => {
    // A one-card step re-selects rather than toggling, which is exactly today's
    // behaviour for the four CR 905.1a kinds — and `<=` rather than `===` so a
    // published 0 (which no engine invariant forbids the client receiving) takes
    // the same path instead of silently falling into multi-select.
    if (requiredCount <= 1) {
      selectCard(instanceId);
      return;
    }
    if (primaryCard === instanceId) {
      selectCard(null);
      setAdditionalCards([]);
    } else if (additionalCards.includes(instanceId)) {
      setAdditionalCards((current) => current.filter((id) => id !== instanceId));
    } else if (!primaryCard) {
      // A fresh primary starts a fresh step: the store can null `selectedCard`
      // on a completed pick (multiplayerDraftStore's guest branch) before the
      // engine's next view arrives, and it can still be holding a card from a
      // step someone else advanced, so additionals must not survive either gap.
      selectCard(instanceId);
      setAdditionalCards([]);
    } else {
      // Full slots slide: the newest click evicts the oldest additional. At the
      // shipped capacity of one this is exactly today's unconditional replace.
      setAdditionalCards((current) =>
        [...current, instanceId].slice(-(requiredCount - 1)),
      );
    }
  };

  const handleToggleDraftEffect = (instanceId: string) => {
    setAdditionalCards([]);
    setActiveDraftEffect((current) => (current === instanceId ? null : instanceId));
  };

  const sections = groupByRarity(pack);

  return (
    <div className="flex flex-col gap-4">
      {enableDraftEffects && draftEffects.length > 0 && (
        <div className="flex flex-wrap items-center gap-x-4 gap-y-2 rounded-lg border border-amber-300/15 bg-amber-300/[0.04] px-3 py-2">
          <span className="shrink-0 text-xs font-semibold text-amber-100">
            {t("pack.draftEffects")}
          </span>
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
            {draftEffects.map((card) => (
              <label
                key={card.instance_id}
                className="flex min-h-11 cursor-pointer items-center gap-2 text-xs text-white/75"
              >
                <input
                  type="checkbox"
                  checked={activeDraftEffect === card.instance_id}
                  onChange={() => handleToggleDraftEffect(card.instance_id)}
                  className="h-4 w-4 accent-amber-400"
                />
                <span>{card.name}</span>
              </label>
            ))}
          </div>
        </div>
      )}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <span className="text-xs text-white/40">{t("pack.cardsInPack", { count: pack.length })}</span>
          {requiredCount > 1 && (
            <span aria-live="polite" className="text-xs font-medium text-amber-200">
              {t("pack.selectionProgress", {
                selected: selectedIds.length,
                required: requiredCount,
              })}
            </span>
          )}
        </div>
        {showAutoPick && (
          <button
            type="button"
            onClick={handleAutoPick}
            disabled={autoPicking || activeDraftEffect !== null}
            className="rounded-lg border border-white/15 bg-white/[0.04] px-3 py-1 text-xs font-medium text-white/80 transition-colors hover:border-white/25 hover:bg-white/[0.08] disabled:cursor-not-allowed disabled:opacity-50"
          >
            {autoPicking ? t("pack.picking") : t("pack.autoPick")}
          </button>
        )}
      </div>
      {sections.map(([rarity, cards]) => {
        const rarityStyle = RARITY_STYLES[rarity] ?? RARITY_STYLE_FALLBACK;
        return (
          <div key={rarity}>
            <h3
              className={`mb-2 border-l-2 pl-2 text-xs font-semibold uppercase tracking-wider ${rarityStyle.text} ${rarityStyle.accent}`}
            >
              {RARITY_LABELS[rarity] ?? rarity}
            </h3>
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5">
              {cards.map((card) => (
                <PackCard
                  key={card.instance_id}
                  card={card}
                  isSelected={selectedIds.includes(card.instance_id)}
                  onSelect={handleSelectCard}
                  onConfirm={handleConfirmPick}
                  confirmDisabled={selectedIds.length !== requiredCount}
                  onHover={onCardHover}
                />
              ))}
            </div>
          </div>
        );
      })}
    </div>
  );
}
