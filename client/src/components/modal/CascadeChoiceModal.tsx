import { useTranslation } from "react-i18next";

import type { GameAction, ManaCost } from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { manaCostToShards } from "../../viewmodel/costLabel.ts";
import { DialogShell } from "./DialogShell.tsx";

/**
 * CR 702.85a: Cascade — when a cascade-source spell finds an eligible nonland
 * card with mana value strictly less than the source's mana value, the caster
 * may cast it without paying its mana cost or decline. Declining shuffles the
 * hit and all misses to the bottom of the library in a random order.
 */
export function CascadeChoiceModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  if (waitingFor?.type !== "CastOffer") return null;
  const kind = waitingFor.data.kind;
  if (
    kind.type !== "Cascade" &&
    kind.type !== "Discover" &&
    kind.type !== "Ripple" &&
    kind.type !== "GraveyardPaidCast"
  )
    return null;
  if (!canActForWaitingState) return null;

  if (kind.type === "Discover") {
    return (
      <CascadeChoiceContent
        actionType="DiscoverChoice"
        hitCardId={kind.hit_card}
        missCount={kind.exiled_misses.length}
        promptKind="Discover"
        dispatch={dispatch}
      />
    );
  }

  // CR 608.2g: paid graveyard cast — accepting pays the card's real cost, so
  // the copy differs from the free Cascade/Ripple/Discover casts. The offer
  // may carry a payment concession (CR 609.4b: Quistis Trepe, Tinybones the
  // Pickpocket) and an additional cost (CR 601.2b: Ogre Battlecaster); both
  // are named when present. No misses to count.
  if (kind.type === "GraveyardPaidCast") {
    return (
      <CascadeChoiceContent
        actionType="GraveyardPaidCastChoice"
        hitCardId={kind.hit_card}
        missCount={0}
        promptKind="GraveyardPaidCast"
        manaSpendPermission={kind.mana_spend_permission}
        additionalCost={kind.additional_cost}
        dispatch={dispatch}
      />
    );
  }

  // CR 702.60a: Ripple — cast the revealed same-named card for free or decline
  // (the rest go to the bottom of the library). Reuses the shared cast-offer body.
  if (kind.type === "Ripple") {
    return (
      <CascadeChoiceContent
        actionType="RippleChoice"
        hitCardId={kind.hit_card}
        missCount={kind.remaining_hits.length + kind.revealed_misses.length}
        promptKind="Ripple"
        dispatch={dispatch}
      />
    );
  }

  return (
    <CascadeChoiceContent
      actionType="CascadeChoice"
      hitCardId={kind.hit_card}
      missCount={kind.exiled_misses.length}
      promptKind="Cascade"
      sourceMv={kind.source_mv}
      dispatch={dispatch}
    />
  );
}

function CascadeChoiceContent({
  actionType,
  hitCardId,
  missCount,
  promptKind,
  sourceMv,
  manaSpendPermission,
  additionalCost,
  dispatch,
}: {
  actionType: "CascadeChoice" | "DiscoverChoice" | "RippleChoice" | "GraveyardPaidCastChoice";
  hitCardId: number;
  missCount: number;
  promptKind: "Cascade" | "Discover" | "Ripple" | "GraveyardPaidCast";
  sourceMv?: number;
  // CR 609.4b: the paid offer's payment concession, kept as the engine's
  // variant — `AnyColor` relaxes colored requirements only, `AnyTypeOrColor`
  // also lets colored mana pay {C}. The plain paid offer (Ogre Battlecaster,
  // Helmut Zemo, Toshiro Umezawa) carries none and must not claim one.
  manaSpendPermission?: "AnyTypeOrColor" | "AnyColor";
  // CR 601.2b: an additional mana cost paid on top of the printed cost.
  additionalCost?: ManaCost;
  dispatch: (action: GameAction) => Promise<unknown>;
}) {
  const { t } = useTranslation("game");
  const obj = useGameStore((s) => s.gameState?.objects[hitCardId]);

  if (!obj) return null;

  // CR 601.2b + CR 609.4b: the paid offer's copy is composed from fragments
  // so every combination of an additional cost and a payment concession is
  // named — the two are independent fields of the offer.
  const extraCostText = additionalCost
    ? manaCostToShards(additionalCost)
        .map((shard) => `{${shard}}`)
        .join("")
    : "";
  const paidExtra = extraCostText
    ? t("cascadeChoice.paidExtraFragment", { extra: extraCostText })
    : "";
  const paidSubtitleConcession =
    manaSpendPermission === "AnyTypeOrColor"
      ? t("cascadeChoice.paidAnyTypeSubtitleFragment")
      : manaSpendPermission === "AnyColor"
        ? t("cascadeChoice.paidAnyColorSubtitleFragment")
        : "";
  const paidSuffixConcession =
    manaSpendPermission === "AnyTypeOrColor"
      ? t("cascadeChoice.paidAnyTypeSuffixFragment")
      : manaSpendPermission === "AnyColor"
        ? t("cascadeChoice.paidAnyColorSuffixFragment")
        : "";

  const subtitle =
    promptKind === "Cascade"
      ? t("cascadeChoice.subtitleCascade", {
          name: obj.name,
          sourceMv,
          total: missCount + 1,
        })
      : promptKind === "Ripple"
        ? t("cascadeChoice.subtitleRipple", {
            name: obj.name,
            total: missCount + 1,
          })
        : promptKind === "GraveyardPaidCast"
          ? t("cascadeChoice.subtitleGraveyardPaid", {
              name: obj.name,
              extra: paidExtra,
              concession: paidSubtitleConcession,
            })
          : t("cascadeChoice.subtitleDiscover", {
              name: obj.name,
              missCount,
            });

  return (
    <DialogShell
      eyebrow={
        promptKind === "Cascade"
          ? t("cascadeChoice.cascadeEyebrow")
          : promptKind === "Ripple"
            ? t("cascadeChoice.rippleEyebrow")
            : promptKind === "GraveyardPaidCast"
              ? t("cascadeChoice.graveyardPaidEyebrow")
              : t("cascadeChoice.discoverEyebrow")
      }
      title={t("cascadeChoice.title", { name: obj.name })}
      subtitle={subtitle}
      previewObjectId={hitCardId}
    >
      <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
        <button
          onClick={() =>
            dispatch({
              type: actionType,
              data: { choice: { type: "Cast" } },
            })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
        >
          <span className="font-semibold text-white">
            {t("cascadeChoice.castNamed", { name: obj.name })}
          </span>
          <span className="ml-2 text-xs text-slate-400">
            {promptKind === "GraveyardPaidCast"
              ? t("cascadeChoice.castPaidSuffix", {
                  extra: paidExtra,
                  concession: paidSuffixConcession,
                })
              : t("cascadeChoice.castSuffix")}
          </span>
        </button>
        <button
          onClick={() =>
            dispatch({
              type: actionType,
              data: { choice: { type: "Decline" } },
            })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-amber-400/30"
        >
          <span className="font-semibold text-white">
            {promptKind === "Discover"
              ? t("cascadeChoice.putIntoHand")
              : t("cascadeChoice.decline")}
          </span>
          <span className="ml-2 text-xs text-slate-400">
            {promptKind === "Discover"
              ? t("cascadeChoice.discoverDeclineSuffix")
              : promptKind === "GraveyardPaidCast"
                ? t("cascadeChoice.graveyardPaidDeclineSuffix")
                : t("cascadeChoice.cascadeDeclineSuffix")}
          </span>
        </button>
      </div>
    </DialogShell>
  );
}
