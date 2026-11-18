import { useId, useRef, useState, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";

import {
  BRACKET_LABEL,
  BRACKET_TIER_CHIP_CLASS,
  BRACKET_TIER_NUMERIC,
  type BracketEstimate,
  type CommanderBracket,
} from "../../types/bracket";
import { BracketPanelLadder } from "./BracketPanelLadder";
import { BracketPanelReason } from "./BracketPanelReason";
import { BracketPanelTrack } from "./BracketPanelTrack";
import { reasonKey } from "./bracketPanelText";

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

const TABS = ["why", "bringDown"] as const;
type Tab = (typeof TABS)[number];

export function BracketAuditPanel({ estimate, manualBracket, onCardClick, emptyReason }: Props) {
  const { t } = useTranslation("deck-builder");
  const [tab, setTab] = useState<Tab>("why");
  const [missingOpen, setMissingOpen] = useState(false);
  const idBase = useId();
  const missingListId = `${idBase}-missing`;
  const tabRefs = useRef<Record<Tab, HTMLButtonElement | null>>({ why: null, bringDown: null });

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
  // The engine owns the comparison. The panel never re-derives it: brackets are
  // a pregame-conversation tool, the declaration governs above the floor, and a
  // declaration ABOVE the estimate (a cEDH deck over an Optimized floor) is the
  // system working. See `DeclarationVerdict` in game/bracket_estimate.rs.
  const belowFloorWarning =
    estimate.declaration?.kind === "below_floor" && manualBracket !== null
      ? { declared: manualBracket, floor: BRACKET_TIER_NUMERIC[estimate.declaration.floor] }
      : null;
  const { why, steps } = estimate.explanation;
  const unresolved = estimate.coverage.unresolved;
  const tabId = (which: Tab) => `${idBase}-tab-${which}`;
  const panelId = (which: Tab) => `${idBase}-panel-${which}`;

  const onTabKeyDown = (event: KeyboardEvent<HTMLButtonElement>) => {
    if (event.key !== "ArrowRight" && event.key !== "ArrowLeft") return;
    event.preventDefault();
    const step = event.key === "ArrowRight" ? 1 : -1;
    const next = TABS[(TABS.indexOf(tab) + step + TABS.length) % TABS.length];
    setTab(next);
    tabRefs.current[next]?.focus();
  };

  const tabLabel: Record<Tab, string> = {
    why: t("bracket.card.tabWhy", { tier: tierNum }),
    bringDown: t("bracket.card.tabBringDown"),
  };

  return (
    <div className="rounded-md border border-white/10 bg-black/20 px-3 py-2">
      <BracketPanelTrack tier={estimate.tier} declared={manualBracket} />

      <div className="mt-2">
        <div className="text-[10px] uppercase tracking-wider text-slate-500">
          {t("bracket.card.estimatedLabel")}
        </div>
        <div
          className={`inline-block rounded-md border px-2 py-0.5 text-xl font-semibold ${BRACKET_TIER_CHIP_CLASS[estimate.tier]}`}
        >
          {t("bracket.card.tierName", { tier: tierNum, label: BRACKET_LABEL[tierNum] })}
        </div>
        <div className="text-xs text-slate-400">
          {manualBracket === null
            ? t("bracket.card.declaredNone")
            : t("bracket.card.declared", {
                tier: manualBracket,
                label: BRACKET_LABEL[manualBracket],
              })}
        </div>
      </div>

      {belowFloorWarning && (
        <p className="mt-2 text-xs text-amber-300">
          {t("bracket.card.belowFloor", belowFloorWarning)}
        </p>
      )}

      {estimate.coverage.confidence === "partial" && (
        <div className="mt-2 text-xs text-amber-300">
          <button
            type="button"
            aria-expanded={missingOpen}
            aria-controls={missingListId}
            onClick={() => setMissingOpen((v) => !v)}
            className="inline-flex min-h-[44px] items-center text-left underline-offset-2 hover:underline sm:min-h-0"
          >
            {t("bracket.card.partialCoverage", { count: unresolved.length })}
          </button>
          {/* Always in the page so `aria-controls` names a real element; like the
              tab panels, it is hidden and empty while collapsed. */}
          <ul id={missingListId} hidden={!missingOpen} className="list-disc pl-5 text-amber-100">
            {missingOpen && unresolved.map((name) => <li key={name}>{name}</li>)}
          </ul>
        </div>
      )}

      {estimate.barometers?.two_card_combos === "unanswered" && (
        <p className="mt-2 text-xs text-slate-500">{t("bracket.card.combosUnchecked")}</p>
      )}

      <div
        role="tablist"
        aria-label={t("bracket.card.tabsLabel")}
        className="mt-3 flex gap-1 border-b border-white/10"
      >
        {TABS.map((which) => {
          const selected = tab === which;
          return (
            <button
              key={which}
              ref={(node) => {
                tabRefs.current[which] = node;
              }}
              type="button"
              role="tab"
              id={tabId(which)}
              aria-selected={selected}
              aria-controls={panelId(which)}
              tabIndex={selected ? 0 : -1}
              onClick={() => setTab(which)}
              onKeyDown={onTabKeyDown}
              className={`inline-flex min-h-[44px] items-center border-b-2 px-2.5 text-xs font-medium sm:min-h-0 sm:py-1.5 ${
                selected
                  ? "border-slate-200 text-slate-100"
                  : "border-transparent text-slate-400 hover:text-slate-200"
              }`}
            >
              {tabLabel[which]}
            </button>
          );
        })}
      </div>

      <div
        role="tabpanel"
        id={panelId("why")}
        aria-labelledby={tabId("why")}
        hidden={tab !== "why"}
        className="pt-2 text-xs"
      >
        {tab === "why" && (
          <>
            {steps.length === 0 && (
              <p className="text-slate-300">{t("bracket.card.why.nothingRaises")}</p>
            )}
            {steps.length === 0 && why.length > 0 && (
              <p className="mt-1 text-slate-400">{t("bracket.card.why.rulesOutB1")}</p>
            )}
            {why.map((reason) => (
              <BracketPanelReason
                key={reasonKey(reason)}
                reason={reason}
                onCardClick={onCardClick}
              />
            ))}
          </>
        )}
      </div>
      <div
        role="tabpanel"
        id={panelId("bringDown")}
        aria-labelledby={tabId("bringDown")}
        hidden={tab !== "bringDown"}
        className="pt-2"
      >
        {tab === "bringDown" && <BracketPanelLadder steps={steps} onCardClick={onCardClick} />}
      </div>

      <div className="mt-2 border-t border-white/5 pt-2 text-[10px] text-slate-500">
        <span>{t("bracket.dataVersion", { version: estimate.data_version })}</span>
        {estimate.combo_coverage === "measured" && estimate.combo_provenance && (
          <>
            {" · "}
            <span>
              {t("bracket.combo.attribution", {
                attribution: estimate.combo_provenance.attribution,
                date: estimate.combo_provenance.snapshot_date,
              })}
            </span>
          </>
        )}
        {" · "}
        <a
          href="https://magic.wizards.com/en/news/announcements/introducing-commander-brackets-beta"
          target="_blank"
          rel="noreferrer"
          className="underline-offset-2 hover:underline"
        >
          {t("bracket.aboutBrackets")}
        </a>
      </div>
    </div>
  );
}
