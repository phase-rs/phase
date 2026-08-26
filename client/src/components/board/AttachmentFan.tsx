import { type CSSProperties, useCallback, useEffect, useMemo } from "react";
import { createPortal } from "react-dom";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { InteractionSubmission } from "../../adapter/generated/interaction/index.ts";
import type { ObjectId } from "../../adapter/types.ts";
import { dispatchAction, dispatchInteraction } from "../../game/dispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { cardImageLookup, tokenFiltersForObject } from "../../services/cardImageLookup.ts";
import { useAppNotificationStore } from "../../stores/appToastStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import {
  collectObjectActions,
  deriveActivationAffordances,
  resolveObjectActivation,
} from "../../viewmodel/cardActionChoice.ts";
import { CardImage } from "../card/CardImage.tsx";
import { fanGeometry, spreadFactor } from "../card/fanGeometry.ts";

// Card sizing for the centered fan. Cards render at near card-preview size so
// oracle text / P-T are readable at rest (no separate preview needed), and scale
// down ONLY IF NEEDED to fit the viewport. The width is:
//
//   min( clamp(11rem, 26vh, 24rem) ,  ${WIDTH_BUDGET_VW}vw / spreadFactor(n) )
//        └────── desired size ──────┘  └──────── width-fit cap ────────────┘
//
//   • Desired size is height-driven (`26vh`; `* 1.4` is the standard card aspect)
//     so it's generous on a tall screen — but FLOORED at `11rem` so a SHORT screen
//     (landscape phone, where `26vh` collapses to ~100px) still gets a big card.
//     Ceiling keeps a 2-card fan from ballooning on an ultrawide monitor.
//   • Width-fit cap is an OUTER `min`, so it can pull the card BELOW the 11rem
//     floor when the whole overlapped fan (`cardW * spreadFactor`) would otherwise
//     overflow — i.e. a narrow PORTRAIT phone with several attachments. It never
//     binds on a roomy screen, so those sizes are unchanged. Budget is generous
//     (96vw) so the common 1–2 card case is never shrunk.
//
// The floor lives INSIDE the desired term (not as an outer clamp) precisely so it
// rescues short screens without re-inflating a correctly width-shrunk card.
const WIDTH_BUDGET_VW = 96;
function fanCardSizingStyle(cardCount: number): CSSProperties {
  const widthCapVw = (WIDTH_BUDGET_VW / spreadFactor(cardCount)).toFixed(1);
  const cardW = `min(clamp(11rem, 26vh, 24rem), ${widthCapVw}vw)`;
  return {
    "--fan-card-w": cardW,
    "--fan-card-h": `calc(${cardW} * 1.4)`,
  } as CSSProperties;
}

/**
 * Centered spread of a host permanent plus every permanent attached to it
 * (Aura / Equipment / Fortification), fanned out at HAND size using the shared
 * compact `fanGeometry` profile — so it reads as a familiar held hand of large,
 * legible cards rather than a bespoke overlay. Solves the reachability
 * problem where an attached Equipment/Aura renders only as a narrow peek behind
 * its host: during a target or board-choice prompt the overlapping objects are
 * all legal picks (CR 301.5 / 303.4: an attached permanent is its own
 * independent object), and the fan lets the player choose which one without
 * hunting the peek.
 *
 * Membership is not this component's decision. `viewerInteraction.attachmentViews`
 * publishes what is attached to the host, ordered and both-direction validated by
 * the engine; the fan renders that list and never scans `attachments` itself.
 *
 * The fan NEVER invents a choice either. It has exactly two engine-owned sources
 * per card: the submission the projection published for that card (mode 1), and
 * — for a card it published none for — the permanent's own legal-action bucket
 * read through `deriveActivationAffordances` (mode 2, the same authority the
 * battlefield ring uses). Both live side by side in one fan, because a published
 * pick for one attachment says nothing about its neighbours. Each card lights up
 * (cyan) and dispatches only what one of those two offers for that object.
 * One-step picks close the fan. Multi-step decisions stay in their dedicated
 * engine-authored interaction surfaces instead of asking this display to build
 * a response payload.
 * Direct clicking on the battlefield still works — this fan is an opt-in
 * convenience opened from the "⧉" badge, not a forced modal.
 */
export function AttachmentFan() {
  const { t } = useTranslation("game");
  const hostId = useUiStore((s) => s.attachmentFanHostId);
  const setAttachmentFanHost = useUiStore((s) => s.setAttachmentFanHost);
  const dismissPreview = useUiStore((s) => s.dismissPreview);
  const showNotification = useAppNotificationStore((s) => s.showNotification);

  const objects = useGameStore((s) => s.gameState?.objects);
  const viewerInteraction = useGameStore((s) => s.viewerInteraction);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
  const canActForWaitingState = useCanActForWaitingState();
  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);
  // Mode 2's gate: THE same affordance sets the battlefield ring uses, so the fan
  // can never offer what the board would not. `AttachmentFan` is a single portaled
  // overlay (GamePage.tsx:1885), not a per-permanent component — one subscription,
  // not an O(board) cost.
  const affordances = useMemo(
    () =>
      deriveActivationAffordances(waitingFor, canActForWaitingState, legalActionsByObject, objects),
    [waitingFor, canActForWaitingState, legalActionsByObject, objects],
  );
  const canActivate = useCallback(
    (id: ObjectId) =>
      affordances.activatableObjectIds.has(id) || affordances.manaTappableObjectIds.has(id),
    [affordances],
  );
  const host = hostId != null ? objects?.[hostId] : undefined;
  // THE membership authority: the engine publishes what is attached to this
  // host, in its own order, with a submission on any card it published a
  // one-step pick for. This display neither walks `attachments` nor decides who
  // belongs in the fan — it renders the projection and counts it.
  const attachmentView = useMemo(
    () => (hostId == null ? null : (viewerInteraction?.attachmentViews[hostId] ?? null)),
    [hostId, viewerInteraction],
  );
  const submissionById = useMemo(() => {
    const table = new Map<ObjectId, InteractionSubmission>();
    for (const card of attachmentView?.cards ?? []) {
      if (card.submission !== null) table.set(card.objectId, card.submission);
    }
    return table;
  }, [attachmentView]);

  const cardIds = host
    ? [host.id, ...(attachmentView?.cards ?? []).map((card) => card.objectId)]
    : [];

  const close = useCallback(() => {
    setAttachmentFanHost(null);
    // The fan is opened from a hovered card, so a card preview may still be up
    // (CardPreview `z-[100]`); tear it down so it doesn't linger over the board.
    dismissPreview();
  }, [setAttachmentFanHost, dismissPreview]);

  // Esc closes, mirroring every other dismissible overlay.
  useEffect(() => {
    if (hostId == null) return undefined;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [hostId, close]);

  const notifyFailure = useCallback(
    (error: unknown) => {
      showNotification({
        title: t("actionError.title", { action: t("permanent.fanPick") }),
        description: error instanceof Error ? error.message : t("actionError.unknownEngineError"),
      });
    },
    [showNotification, t],
  );

  const handlePick = useCallback(
    (id: ObjectId) => {
      // Mode 1 — the engine published a pick for THIS card: forward its opaque
      // submission, nothing else. Decided per card, because the projection
      // carries a pick for some members and none for others.
      const submission = submissionById.get(id);
      if (submission) {
        if (!viewerInteraction?.canSubmit) return;
        void dispatchInteraction(submission).then(close).catch(notifyFailure);
        return;
      }
      // Mode 2 — no prompt is open, so the fan is a reachability surface for the
      // permanent's OWN legal actions. Gate and dispatch both come from the shared
      // authority the battlefield uses. CR 301.5 / CR 303.4: an attached permanent
      // is its own object.
      if (!canActivate(id)) return;
      const store = useGameStore.getState();
      const verdict = resolveObjectActivation(
        collectObjectActions(store.legalActionsByObject, id),
        store.gameState?.objects[id],
        affordances,
        id,
      );
      // `close()` MUST precede opening the modal: the fan is a `fixed inset-0
      // z-[120]` backdrop with an `onClick={close}` catcher and DialogHost anchors
      // at z-40, so a fan left mounted would both paint over and swallow clicks for
      // the modal it just opened. It stays inside the two acting arms because
      // `kind: "none"` must leave the fan exactly as it was.
      switch (verdict.kind) {
        case "dispatch":
          close();
          void dispatchAction(verdict.action).catch(notifyFailure);
          return;
        case "choose":
          close();
          setPendingAbilityChoice({ objectId: id, actions: verdict.actions });
          return;
        case "none":
          // Reachable only through the render→click staleness window: the ring was
          // painted from a bucket this click no longer sees. Doing nothing (and
          // leaving the fan open) is correct.
          return;
        default: {
          // CLAUDE.md "exhaustive match without wildcard fallbacks": a new
          // ObjectActivation variant is a compile error here, never a silent drop.
          const _exhaustive: never = verdict;
          return _exhaustive;
        }
      }
    },
    [
      affordances,
      canActivate,
      close,
      submissionById,
      notifyFailure,
      setPendingAbilityChoice,
      viewerInteraction?.canSubmit,
    ],
  );

  // A fan of just the host is not a fan. With no published membership there is
  // nothing to spread, and this display does not go looking for members itself.
  if (hostId == null || !host || (attachmentView?.cards.length ?? 0) === 0) return null;

  // Shared compact whole-row fan — sized by the total card count so the host +
  // its attachments stay within the overlay's viewport budget.
  // The overlap basis is `--fan-card-w` (the fan's larger card width) so the
  // spread stays proportional to the bigger cards.
  const fan = fanGeometry(cardIds.length, "--fan-card-w");

  return createPortal(
    // Portaled above the card preview (`z-[100]`) so the fan and its cyan
    // affordances are never veiled. Backdrop click / Esc dismiss.
    <div
      className="fixed inset-0 z-[120] flex flex-col items-center justify-center bg-black/60 backdrop-blur-[2px]"
      onClick={close}
      data-attachment-fan
    >
      {/* `items-end` + `perspective` mirror the hand container so the cards fan
          up from a shared baseline with the same held-hand feel. The sizing style
          defines `--fan-card-w/h` — big by default, scaling down only if the fan
          would overflow a narrow screen (portrait mobile with several cards). */}
      <div
        className="flex items-end justify-center"
        style={{ perspective: "800px", ...fanCardSizingStyle(cardIds.length) }}
        onClick={(e) => e.stopPropagation()}
      >
        {cardIds.map((id, i) => (
          <FanCard
            key={id}
            objectId={id}
            marginLeft={i === 0 ? 0 : fan.overlap}
            rotation={fan.rotation(i)}
            arcOffset={fan.arc(i)}
            zIndex={i}
            selectable={
              id !== host.id &&
              ((submissionById.has(id) && viewerInteraction?.canSubmit === true) ||
                canActivate(id))
            }
            onPick={handlePick}
          />
        ))}
      </div>

    </div>,
    document.body,
  );
}

function FanCard({
  objectId,
  marginLeft,
  rotation,
  arcOffset,
  zIndex,
  selectable,
  onPick,
}: {
  objectId: ObjectId;
  marginLeft: string | number;
  rotation: number;
  arcOffset: number;
  zIndex: number;
  selectable: boolean;
  onPick: (id: ObjectId) => void;
}) {
  const { t } = useTranslation("game");
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);
  if (!obj) return null;

  const lookup = cardImageLookup(obj);
  const isToken = obj.display_source === "Token";
  // The whole fan speaks one "pick me" color — cyan — so a spread of a host and
  // its attachments reads as one direct engine-authorized chooser.
  const ring = selectable ? "ring-2 ring-cyan-400 shadow-[0_0_16px_5px_rgba(34,211,238,0.55)]" : "";

  // Mirror the hand card's resting animation (arc + tilt) and hover lift so the
  // attachment fan feels identical to picking a card out of hand.
  return (
    <motion.div
      layout
      initial={{ opacity: 0, y: 40 }}
      animate={{ opacity: 1, y: 30 + arcOffset, rotate: rotation }}
      whileHover={{ y: arcOffset - 12, scale: 1.12, zIndex: 999 }}
      transition={{ duration: 0.2 }}
      onClick={(e) => {
        e.stopPropagation();
        if (selectable) onPick(objectId);
      }}
      aria-label={obj.name}
      className={`relative leading-[0] select-none ${selectable ? "cursor-pointer" : "cursor-default"}`}
      style={{ marginLeft, zIndex }}
    >
      <div className={`relative overflow-hidden rounded-lg ${ring} ${selectable ? "" : "opacity-60"}`}>
        <CardImage
          cardName={lookup.name}
          faceIndex={lookup.faceIndex}
          oracleId={lookup.oracleId}
          faceName={lookup.faceName}
          size="large"
          isToken={isToken}
          tokenFilters={isToken ? tokenFiltersForObject(obj) : undefined}
          tokenImageRef={isToken ? obj.token_image_ref : undefined}
          oracleText={isToken ? obj.token_rules_text : undefined}
          faceDown={obj.face_down === true}
          faceDownCause={obj.face_down ? obj.face_down_cause : undefined}
          className="!w-[var(--fan-card-w)] !h-[var(--fan-card-h)]"
        />
      </div>
      {selectable && (
        <span className="pointer-events-none absolute left-1 top-1 z-10 rounded bg-cyan-400 px-1.5 py-0.5 text-[9px] font-black uppercase leading-none tracking-normal text-cyan-950 ring-1 ring-cyan-950/50 shadow">
          {t("permanent.fanPick")}
        </span>
      )}
    </motion.div>
  );
}
