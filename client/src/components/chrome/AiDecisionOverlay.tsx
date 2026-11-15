import { motion, useDragControls } from "framer-motion";
import { useRef, useState, type PointerEvent } from "react";
import { useTranslation } from "react-i18next";

import type { AiDecisionDiagnosticReceipt } from "../../adapter/types";

function actionLabel(type: string): string {
  return type.replace(/([a-z])([A-Z])/g, "$1 $2");
}

function probabilityLabel(probability: number | null): string {
  if (probability == null) return "—";
  return new Intl.NumberFormat(undefined, {
    style: "percent",
    maximumFractionDigits: 1,
  }).format(probability);
}

function weightLabel(weight: number | null): string {
  if (weight == null) return "—";
  return new Intl.NumberFormat(undefined, {
    maximumFractionDigits: 3,
  }).format(weight);
}

function scoreLabel(score: number | null): string {
  if (score == null) return "—";
  return new Intl.NumberFormat(undefined, { maximumFractionDigits: 3, signDisplay: "always" }).format(score);
}

function temperatureLabel(temperature: number | null): string {
  if (temperature == null) return "—";
  return new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 }).format(temperature);
}

/**
 * A local-only view of an engine-authored AI decision receipt. Its bars read
 * the engine's normalized probability directly; it never scores or ranks an
 * action in the browser.
 */
export function AiDecisionOverlay({
  receipt,
  visible,
  onClose,
}: {
  receipt: AiDecisionDiagnosticReceipt | null;
  visible: boolean;
  onClose: () => void;
}) {
  const { t } = useTranslation("game");
  const dragControls = useDragControls();
  const constraintsRef = useRef<HTMLDivElement>(null);
  const [collapsed, setCollapsed] = useState(false);

  if (!visible || !receipt) return null;

  const startDrag = (event: PointerEvent<HTMLDivElement>) => {
    dragControls.start(event);
  };

  return (
    <div ref={constraintsRef} className="pointer-events-none fixed inset-0 z-[9998]">
      <motion.aside
        drag
        dragControls={dragControls}
        dragListener={false}
        dragMomentum={false}
        dragElastic={0}
        dragConstraints={constraintsRef}
        initial={{ opacity: 0, y: 12, filter: "blur(4px)" }}
        animate={{ opacity: 1, y: 0, filter: "blur(0px)" }}
        exit={{ opacity: 0, y: 8, filter: "blur(4px)" }}
        transition={{ duration: 0.18, ease: "easeOut" }}
        aria-label={t("aiDecisionOverlay.title")}
        className="pointer-events-auto absolute bottom-24 right-5 max-h-[calc(100vh-3rem)] w-[min(23rem,calc(100vw-2rem))] overflow-y-auto rounded-2xl bg-slate-950/95 text-slate-100 shadow-[0_20px_55px_rgba(2,6,23,0.55)] ring-1 ring-white/10 backdrop-blur-xl"
      >
        <div
          onPointerDown={startDrag}
          className="flex cursor-grab touch-none select-none items-center justify-between border-b border-white/10 bg-slate-900/80 px-4 py-3 active:cursor-grabbing"
        >
          <div className="min-w-0">
            <p className="text-sm font-semibold text-white">{t("aiDecisionOverlay.title")}</p>
            <p className="mt-0.5 text-xs text-slate-400">
              {receipt.status === "ranked"
                ? t("aiDecisionOverlay.rankedSubtitle", { temperature: temperatureLabel(receipt.samplingTemperature) })
                : t("aiDecisionOverlay.directSubtitle")}
            </p>
          </div>
          <div
            className="flex shrink-0 items-center gap-1"
            onPointerDown={(event) => event.stopPropagation()}
          >
            <button
              type="button"
              aria-label={t(collapsed ? "aiDecisionOverlay.expand" : "aiDecisionOverlay.collapse")}
              title={t(collapsed ? "aiDecisionOverlay.expand" : "aiDecisionOverlay.collapse")}
              onClick={() => setCollapsed((value) => !value)}
              className="flex h-7 w-7 items-center justify-center rounded-md text-slate-400 transition-colors hover:bg-white/10 hover:text-white focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-cyan-300"
            >
              <svg aria-hidden="true" viewBox="0 0 16 16" className="h-4 w-4 fill-none stroke-current stroke-[1.75]">
                {collapsed ? <path d="m4 6 4 4 4-4" /> : <path d="m4 10 4-4 4 4" />}
              </svg>
            </button>
            <button
              type="button"
              aria-label={t("aiDecisionOverlay.close")}
              title={t("aiDecisionOverlay.close")}
              onClick={onClose}
              className="flex h-7 w-7 items-center justify-center rounded-md text-slate-400 transition-colors hover:bg-white/10 hover:text-white focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-cyan-300"
            >
              <svg aria-hidden="true" viewBox="0 0 16 16" className="h-4 w-4 fill-none stroke-current stroke-[1.75]">
                <path d="m4.5 4.5 7 7m0-7-7 7" />
              </svg>
            </button>
            <span aria-hidden className="ml-1 text-lg leading-none text-slate-500">⠿</span>
          </div>
        </div>

        {!collapsed ? (
          <>
            <ol className="divide-y divide-white/5 px-3 py-1">
              {receipt.candidates.map((candidate, index) => {
                const color = candidate.isTopRanked
                  ? "bg-cyan-400"
                  : candidate.isSelected
                    ? "bg-amber-400"
                    : "bg-slate-500";
                const label = actionLabel(candidate.action.type);

                return (
                  <li key={`${candidate.action.type}-${index}`} className="py-2">
                    <div className="flex items-center gap-2">
                      <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-white/5 text-xs font-semibold text-slate-400">
                        {candidate.rank ?? "—"}
                      </span>
                      <span className="min-w-0 flex-1 text-sm font-medium text-slate-100" title={candidate.action.type}>
                        {candidate.objectName ? `${label} — ${candidate.objectName}` : label}
                      </span>
                      {candidate.isTopRanked ? (
                        <span className="rounded-full bg-cyan-400/15 px-2 py-0.5 text-[0.65rem] font-semibold tracking-wide text-cyan-200">
                          {t("aiDecisionOverlay.top")}
                        </span>
                      ) : null}
                      {candidate.isSelected ? (
                        <span className="rounded-full bg-amber-400/15 px-2 py-0.5 text-[0.65rem] font-semibold tracking-wide text-amber-200">
                          {t("aiDecisionOverlay.chosen")}
                        </span>
                      ) : null}
                    </div>
                    {candidate.details.length > 0 ? (
                      <dl className="mt-1 grid grid-cols-[auto_1fr] gap-x-2 gap-y-0.5 px-2 font-mono text-[0.65rem] leading-4">
                        {candidate.details.map((detail) => (
                          <div key={detail.label} className="contents">
                            <dt className="text-slate-500">{detail.label}</dt>
                            <dd className="break-words text-slate-300">{detail.value}</dd>
                          </div>
                        ))}
                      </dl>
                    ) : null}
                    {receipt.status === "ranked" ? (
                      <div className="mt-1.5 flex items-center gap-2 pl-8">
                        <span className="w-[6.5rem] shrink-0 font-mono text-[0.65rem] tabular-nums text-slate-500">
                          {t("aiDecisionOverlay.metrics", {
                            score: scoreLabel(candidate.score),
                            weight: weightLabel(candidate.weight),
                          })}
                        </span>
                        <div className="h-2 flex-1 overflow-hidden rounded-full bg-white/5">
                          <div
                            className={`h-full origin-left rounded-full ${color}`}
                            style={{ transform: `scaleX(${candidate.probability ?? 0})` }}
                          />
                        </div>
                        <span className="w-10 text-right font-mono text-[0.7rem] tabular-nums text-slate-300">
                          {probabilityLabel(candidate.probability)}
                        </span>
                      </div>
                    ) : null}
                  </li>
                );
              })}
            </ol>

            {receipt.status === "ranked" ? (
              <div className="border-t border-white/10 px-3 py-2 text-[0.68rem] text-slate-400">
                <p>{receipt.selectionExplanation}</p>
                <div className="mt-1 flex items-center gap-3">
                  <span className="flex items-center gap-1.5"><i className="h-2 w-2 rounded-full bg-cyan-400" />{t("aiDecisionOverlay.legendTop")}</span>
                  <span className="flex items-center gap-1.5"><i className="h-2 w-2 rounded-full bg-amber-400" />{t("aiDecisionOverlay.legendChosen")}</span>
                </div>
              </div>
            ) : null}
          </>
        ) : null}
      </motion.aside>
    </div>
  );
}
