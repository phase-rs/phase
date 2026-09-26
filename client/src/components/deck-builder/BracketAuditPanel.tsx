import { useState } from "react";
import { useTranslation } from "react-i18next";

import {
  BRACKET_AXES,
  BRACKET_LABEL,
  BRACKET_TIER_CHIP_CLASS,
  BRACKET_TIER_NUMERIC,
  type BracketAxis,
  type BracketEstimate,
  type CommanderBracket,
} from "../../types/bracket";

interface Props {
  estimate: BracketEstimate | null;
  /** Player-selected bracket shown as display-only metadata. */
  manualBracket: CommanderBracket | null;
  onCardClick: (cardName: string) => void;
  /**
   * "not-commander" hides the panel; "no-commander" renders the
   * "add a commander" placeholder; "card-data-unavailable" renders the
   * card-data loading failure placeholder.
   */
  emptyReason?: "not-commander" | "no-commander" | "card-data-unavailable";
}

const AXIS_I18N_KEY: Record<BracketAxis, string> = {
  game_changers: "bracket.axis.gameChangers",
  mass_land_denial: "bracket.axis.massLandDenial",
  extra_turns: "bracket.axis.extraTurns",
  efficient_tutors: "bracket.axis.efficientTutors",
};

export function BracketAuditPanel({ estimate, manualBracket, onCardClick, emptyReason }: Props) {
  const { t } = useTranslation("deck-builder");
  const [expanded, setExpanded] = useState(false);

  if (emptyReason === "not-commander") return null;
  if (emptyReason === "card-data-unavailable") {
    return (
      <div className="rounded-md border border-white/10 bg-black/20 px-3 py-2 text-xs text-slate-400">
        {t("bracket.unavailable")}
      </div>
    );
  }
  if (emptyReason === "no-commander" || !estimate) {
    return (
      <div className="rounded-md border border-white/10 bg-black/20 px-3 py-2 text-xs text-slate-400">
        {t("bracket.addCommander")}
      </div>
    );
  }

  const tierNum = BRACKET_TIER_NUMERIC[estimate.tier];
  const tierLabel = BRACKET_LABEL[tierNum];
  // The engine owns the comparison. The panel never re-derives it: brackets are
  // a pregame-conversation tool, the declaration governs above the floor, and a
  // declaration ABOVE the estimate (a cEDH deck over an Optimized floor) is the
  // system working. See `DeclarationVerdict` in game/bracket_estimate.rs.
  const belowFloor =
    estimate.declaration?.kind === "below_floor" ? estimate.declaration : null;

  return (
    <div className="rounded-md border border-white/10 bg-black/20 px-3 py-2">
      <div className="flex flex-wrap items-center gap-1.5">
        <span
          className={`rounded-full border px-2.5 py-1 text-xs font-medium ${BRACKET_TIER_CHIP_CLASS[estimate.tier]}`}
        >
          {t("bracket.estimated", { tier: tierNum, label: tierLabel })}
        </span>
        {manualBracket !== null && (
          <span
            className={
              belowFloor !== null
                ? "rounded-full border border-amber-300/60 bg-amber-500/20 px-2.5 py-1 text-xs font-medium text-amber-100"
                : "rounded-full border border-white/10 bg-black/20 px-2.5 py-1 text-xs font-medium text-slate-400"
            }
          >
            {t("bracket.manual", { tier: manualBracket, label: BRACKET_LABEL[manualBracket] })}
            {belowFloor &&
              t("bracket.belowFloor", { tier: BRACKET_TIER_NUMERIC[belowFloor.floor] })}
          </span>
        )}
        <button
          type="button"
          aria-expanded={expanded}
          aria-label={expanded ? t("bracket.hideBreakdown") : t("bracket.showBreakdown")}
          onClick={() => setExpanded((v) => !v)}
          className="ml-auto inline-flex min-h-[44px] items-center rounded-full border border-white/10 bg-black/20 px-2.5 py-1 text-xs font-medium text-slate-400 hover:bg-white/6 sm:min-h-0 sm:py-1"
        >
          {expanded ? t("bracket.hideBreakdownButton") : t("bracket.showBreakdownButton")}
        </button>
      </div>

      {expanded && (
        <dl className="mt-3 space-y-2 text-xs">
          {estimate.checks.map((check) => {
            const outcomeText =
              check.outcome.kind === "fired"
                ? t("bracket.check.fired", {
                    observed: check.observed,
                    tier: BRACKET_TIER_NUMERIC[check.floor],
                    label: BRACKET_LABEL[BRACKET_TIER_NUMERIC[check.floor]],
                  })
                : check.outcome.cards_until_fired !== null
                  ? t("bracket.check.clear", {
                      observed: check.observed,
                      threshold: check.threshold,
                      remaining: check.outcome.cards_until_fired,
                      tier: BRACKET_TIER_NUMERIC[check.floor],
                    })
                  : t("bracket.check.clearMax", {
                      observed: check.observed,
                      tier: BRACKET_TIER_NUMERIC[check.floor],
                    });
            return (
              <div
                key={`${check.axis}-${check.threshold}-${check.floor}`}
                className="grid grid-cols-[180px_1fr] items-start gap-2"
              >
                <dt className="text-slate-300">{t(AXIS_I18N_KEY[check.axis])}</dt>
                <dd className="text-slate-400">
                  <div className={check.outcome.kind === "fired" ? "text-amber-300" : undefined}>
                    {outcomeText}
                  </div>
                  {check.axis === "extra_turns" && (
                    <div className="text-[10px] text-slate-500">{t("bracket.uncalibrated")}</div>
                  )}
                  {check.evidence.length > 0 && (
                    <div>
                      {check.evidence.map((name, index) => (
                        <span key={`${name}-${index}`}>
                          <button
                            type="button"
                            onClick={() => onCardClick(name)}
                            className="inline-flex min-h-[44px] items-center text-slate-300 underline-offset-2 hover:underline sm:min-h-0"
                          >
                            {name}
                          </button>
                          {index < check.evidence.length - 1 && ", "}
                        </span>
                      ))}
                    </div>
                  )}
                  <div className="mt-1 text-[10px] text-slate-500">
                    <span>{check.official_line} </span>
                    <a
                      href={check.source_url}
                      target="_blank"
                      rel="noreferrer"
                      className="underline-offset-2 hover:underline"
                    >
                      {t("bracket.check.source", {
                        document: check.source_document,
                        published: check.source_published,
                      })}
                    </a>
                  </div>
                </dd>
              </div>
            );
          })}
          {BRACKET_AXES.filter(
            (axis) => !estimate.checks.some((check) => check.axis === axis),
          ).map((axis) => {
            const reading = estimate.axes[axis];
            return (
              <div key={axis} className="grid grid-cols-[180px_1fr] items-start gap-2">
                <dt className="text-slate-300">{t(AXIS_I18N_KEY[axis])}</dt>
                <dd className="text-slate-400">
                  <div>
                    {reading.count} — {t("bracket.uncounted")}
                  </div>
                  {reading.contributing.map((name, index) => (
                    <span key={`${name}-${index}`}>
                      <button
                        type="button"
                        onClick={() => onCardClick(name)}
                        className="inline-flex min-h-[44px] items-center text-slate-300 underline-offset-2 hover:underline sm:min-h-0"
                      >
                        {name}
                      </button>
                      {index < reading.contributing.length - 1 && ", "}
                    </span>
                  ))}
                </dd>
              </div>
            );
          })}
          <div className="border-t border-white/5 pt-2 text-slate-400">
            {t("bracket.coverage", {
              resolved: estimate.coverage.resolved,
              counted: estimate.coverage.counted,
            })}
          </div>
          {estimate.coverage.confidence === "partial" && (
            <div className="space-y-1 text-slate-400">
              <div>
                {t("bracket.confidencePartial", { n: estimate.coverage.unresolved.length })}
              </div>
              <div className="text-slate-300">{t("bracket.unresolvedHeading")}</div>
              <ul className="list-disc pl-5">
                {estimate.coverage.unresolved.map((name) => (
                  <li key={name}>{name}</li>
                ))}
              </ul>
            </div>
          )}
          <div className="border-t border-white/5 pt-2 text-[10px] text-slate-500">
            {t("bracket.dataVersion", { version: estimate.data_version })} · {t("bracket.baseFloor")} ·{" "}
            <a
              href="https://magic.wizards.com/en/news/announcements/introducing-commander-brackets-beta"
              target="_blank"
              rel="noreferrer"
              className="underline-offset-2 hover:underline"
            >
              {t("bracket.aboutBrackets")}
            </a>
          </div>
        </dl>
      )}
    </div>
  );
}
