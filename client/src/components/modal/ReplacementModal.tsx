import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Reorder } from "framer-motion";

import type { ReplacementCandidateSummary } from "../../adapter/types.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { RichLabel } from "../mana/RichLabel.tsx";
import { DialogShell } from "./DialogShell.tsx";

const EMPTY_CANDIDATES: ReplacementCandidateSummary[] = [];

/**
 * CR 616.1 / CR 614: Surfaced when the local player must apply an optional
 * replacement effect ("you may"), choose between alternative destinations, or
 * order two-or-more competing replacements. The engine owns all logic and
 * provides one {@link ReplacementCandidateSummary} per option plus a `kind`
 * discriminator; this component only permutes an index array or dispatches a
 * chosen index. No rules computation, and the prompt shape is NEVER inferred
 * from label text.
 *
 * CR 616.1e/f — why ordering needs its own presentation: the engine applies the
 * selected candidate, then re-prompts for whatever is still applicable, so the
 * effect applied LAST is the one whose write survives. Rendering the candidates
 * as plain "pick one" buttons made outcome-worded labels ("Enters untapped")
 * state the opposite of the result, because picking one applied it FIRST and
 * let the other overwrite it. The ordering list shows the whole sequence with
 * explicit applied-first/applied-last framing instead.
 */
export function ReplacementModal() {
  const { t } = useTranslation("game");
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const hoverProps = useInspectHoverProps();

  const isReplacementChoice = waitingFor?.type === "ReplacementChoice";
  const candidateCount = isReplacementChoice
    ? waitingFor.data.candidate_count
    : 0;
  const candidates: ReplacementCandidateSummary[] = isReplacementChoice
    ? (waitingFor.data.candidates ?? EMPTY_CANDIDATES)
    : EMPTY_CANDIDATES;
  // CR 616.1: only a distinct multi-candidate set is an ordering decision. The
  // engine defaults to `Order`; an accept/decline or destination pick is never
  // sortable. A 1-candidate prompt has no order to choose.
  const kindType =
    waitingFor?.type === "ReplacementChoice"
      ? (waitingFor.data.kind?.type ?? "Order")
      : "Order";
  const isOrdering = kindType === "Order" && candidateCount > 1;
  // CR 616.1f: engine-computed — does the last-applied candidate alone decide
  // the outcome? Absent (legacy/unknown) means "don't claim a winner".
  const lastAppliedDecides =
    waitingFor?.type === "ReplacementChoice"
      ? (waitingFor.data.last_applied_decides ?? false)
      : false;

  // Local UI state: the chosen permutation (indices into `candidates`).
  // Identity to start; reset on every new prompt because successive CR 616.1f
  // rounds can carry the same candidate count.
  const [order, setOrder] = useState<number[]>(() =>
    Array.from({ length: candidateCount }, (_, i) => i),
  );
  // Reset on a genuinely NEW prompt, keyed on stable CONTENT rather than array
  // identity. `candidates` is a fresh array on every engine snapshot commit, so
  // depending on it would blow away an in-progress reorder whenever any
  // unrelated state pushed — the user would watch their arrangement snap back
  // mid-drag. Successive CR 616.1f rounds change the candidate set, so the
  // identity key changes and the reset still fires when it should.
  const promptKey = isReplacementChoice
    ? `${candidateCount}|${candidates.map((c) => `${c.source_id}:${c.description}`).join("|")}`
    : "";
  useEffect(() => {
    setOrder(Array.from({ length: candidateCount }, (_, i) => i));
    // eslint-disable-next-line react-hooks/exhaustive-deps -- promptKey encodes
    // candidateCount + candidate content; adding `candidates` would reintroduce
    // the identity-churn reset this key exists to avoid.
  }, [promptKey]);

  const move = useCallback((from: number, to: number) => {
    setOrder((prev) => {
      if (to < 0 || to >= prev.length) return prev;
      const next = prev.slice();
      const [item] = next.splice(from, 1);
      next.splice(to, 0, item);
      return next;
    });
  }, []);

  const handleChoose = useCallback(
    (index: number) => {
      dispatch({ type: "ChooseReplacement", data: { index } });
    },
    [dispatch],
  );

  // CR 616.1f: submit only the first entry. The engine applies it and re-prompts
  // with whatever remains applicable — which may legitimately differ from this
  // plan (CR 616.2: applying one effect can add or remove others, as with a
  // copy effect that strips the enters-tapped ability). Sending the whole
  // permutation blind would be wrong for those classes.
  const handleConfirmOrder = useCallback(() => {
    const first = order[0];
    if (first !== undefined) handleChoose(first);
  }, [handleChoose, order]);

  if (!isReplacementChoice || candidateCount === 0) return null;

  const indices = Array.from({ length: candidateCount }, (_, i) => i);
  const labelFor = (index: number) =>
    candidates[index]?.description ||
    t("replacement.candidateFallback", { number: index + 1 });

  if (!isOrdering) {
    return (
      <DialogShell
        eyebrow={t("replacement.eyebrow")}
        title={t("replacement.title")}
        subtitle={t("replacement.chooseSubtitle")}
        size="md"
        scrollable
      >
        <div className="px-3 py-3 lg:px-5 lg:py-5">
          <div className="flex flex-col gap-2">
            {indices.map((index) => {
              const candidate = candidates[index];
              return (
                <button
                  key={index}
                  onClick={() => handleChoose(index)}
                  {...(candidate ? hoverProps(candidate.source_id) : {})}
                  className="min-h-11 rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/40"
                >
                  <span className="block font-semibold text-white">
                    <RichLabel text={labelFor(index)} size="sm" />
                  </span>
                  {candidate?.source_name && (
                    <span className="mt-0.5 block text-xs text-white/60">
                      {candidate.source_name}
                    </span>
                  )}
                </button>
              );
            })}
          </div>
        </div>
      </DialogShell>
    );
  }

  // CR 616.1f: name a concrete winning result ONLY when the engine says the
  // last-applied effect decides the outcome. That holds for whole-field
  // overwrites (the enters-tapped class) but NOT for compositional collisions
  // — a damage doubler ordered against a damage adder has both effects apply,
  // so there is no winner to name and the order changes the arithmetic instead.
  // The engine owns this distinction; never infer it from the candidate labels.
  const winningLabel = lastAppliedDecides
    ? labelFor(order[order.length - 1] ?? 0)
    : null;

  return (
    <DialogShell
      eyebrow={t("replacement.eyebrow")}
      title={t("replacement.title")}
      subtitle={
        lastAppliedDecides
          ? t("replacement.orderSubtitle")
          : t("replacement.orderSubtitleComposing")
      }
      size="md"
      scrollable
      footer={
        <button
          type="button"
          onClick={handleConfirmOrder}
          className="min-h-11 rounded-[16px] bg-cyan-500/80 px-5 py-3 font-semibold text-white transition hover:bg-cyan-500"
        >
          {t("replacement.confirmOrder")}
        </button>
      }
    >
      <div className="px-3 py-3 lg:px-5 lg:py-5">
        <div className="mb-2 text-xs uppercase tracking-wide text-white/50">
          {lastAppliedDecides
            ? t("replacement.appliedFirst")
            : t("replacement.appliedEarlier")}
        </div>
        <Reorder.Group
          as="ol"
          axis="y"
          values={order}
          onReorder={setOrder}
          className="flex flex-col gap-2"
        >
          {order.map((candidateIndex, position) => {
            const candidate = candidates[candidateIndex];
            return (
              <Reorder.Item
                as="li"
                // Index-keyed, not source-keyed: an optional replacement sends
                // the same `source_id` twice, so source ids are not unique.
                key={candidateIndex}
                value={candidateIndex}
                whileDrag={{ scale: 1.03, zIndex: 20 }}
                // `!touch-none` (note the `!` — it emits `!important`) keeps a
                // touch-drag reordering the list instead of scrolling the
                // dialog. The bang is load-bearing and must not be "cleaned up":
                // `Reorder.Item` writes its own INLINE `touch-action` for the
                // drag axis (`pan-x` when `axis="y"`), and framer re-asserts it,
                // so neither a plain `touch-none` class nor an inline
                // `style={{ touchAction }}` survives — only an `!important` rule
                // wins the cascade against it. Verified in-browser under touch
                // emulation: without the bang the computed value is `pan-x` and
                // a vertical touch-drag is claimed by page scroll. The
                // codebase's other `touch-none` usages sit on plain elements
                // driven by the hand-rolled pointer engine, where nothing
                // competes with the class.
                className="flex !touch-none cursor-grab items-start gap-2 rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 active:cursor-grabbing"
                {...(candidate ? hoverProps(candidate.source_id) : {})}
              >
                <div className="flex-1 text-left">
                  {/* Position is visually obvious from the list order but
                      invisible to a screen reader, which reads rows in isolation. */}
                  <span className="sr-only">
                    {t("replacement.positionAnnounce", {
                      position: position + 1,
                      total: order.length,
                    })}
                  </span>
                  <div className="font-semibold text-white">
                    <RichLabel text={labelFor(candidateIndex)} size="sm" />
                  </div>
                  {candidate?.source_name && (
                    <div className="mt-0.5 text-xs text-white/60">
                      {candidate.source_name}
                    </div>
                  )}
                </div>
                {/* Arrow controls are the accessible path: a drag-only list is
                    unusable by keyboard and screen-reader users. */}
                <div className="flex flex-col gap-1">
                  <button
                    type="button"
                    aria-label={t("replacement.moveUpNamed", {
                      name: labelFor(candidateIndex),
                    })}
                    disabled={position === 0}
                    onClick={() => move(position, position - 1)}
                    className="min-h-8 rounded border border-white/10 px-2 text-white/80 transition hover:bg-white/10 disabled:opacity-30"
                  >
                    ▲
                  </button>
                  <button
                    type="button"
                    aria-label={t("replacement.moveDownNamed", {
                      name: labelFor(candidateIndex),
                    })}
                    disabled={position === order.length - 1}
                    onClick={() => move(position, position + 1)}
                    className="min-h-8 rounded border border-white/10 px-2 text-white/80 transition hover:bg-white/10 disabled:opacity-30"
                  >
                    ▼
                  </button>
                </div>
              </Reorder.Item>
            );
          })}
        </Reorder.Group>
        <div className="mt-2 text-xs uppercase tracking-wide text-cyan-300/80">
          {lastAppliedDecides
            ? t("replacement.appliedLast")
            : t("replacement.appliedLater")}
        </div>
        {/* State the concrete outcome so the player never has to infer it from
            the ordering semantics — but only when one effect actually wins. */}
        {winningLabel !== null ? (
          <div className="mt-3 rounded-[12px] border border-cyan-400/20 bg-cyan-400/5 px-3 py-2 text-sm font-semibold text-cyan-200">
            <RichLabel
              text={t("replacement.resultLabel", { outcome: winningLabel })}
              size="sm"
            />
          </div>
        ) : (
          <div className="mt-3 rounded-[12px] border border-white/10 bg-white/5 px-3 py-2 text-sm text-white/70">
            {t("replacement.composesNote")}
          </div>
        )}
        <p className="mt-2 text-center text-xs text-white/40">
          {t("replacement.dragHint")}
        </p>
      </div>
    </DialogShell>
  );
}
