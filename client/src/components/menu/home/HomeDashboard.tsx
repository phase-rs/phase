import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import { ACTIVE_DECK_KEY } from "../../../constants/storage";
import { MenuShell } from "../MenuShell";
import { MenuActionTile, type MenuTileTone } from "../MenuActionTile";
import type { TileMotif } from "../TileMotif";
import { ManaSymbol } from "../../mana/ManaSymbol";
import { useCardImage } from "../../../hooks/useCardImage";
import { useResumables } from "../../../hooks/useResumables";
import { useCardDataStore } from "../../../stores/cardDataStore";
import {
  getDeckCardCount,
  getDeckColorIdentity,
  getRepresentativeCard,
} from "../deckHelpers";

interface ActionDef {
  key: string;
  to: string;
  tone: MenuTileTone;
  icon: string;
  motif: TileMotif;
  titleKey: string;
  descKey: string;
  gated: boolean;
}
const ACTIONS: ActionDef[] = [
  { key: "setup", to: "/setup", tone: "arcane", icon: "play", motif: "swords", titleKey: "home.setup.title", descKey: "home.setup.description", gated: true },
  { key: "online", to: "/multiplayer", tone: "jade", icon: "online", motif: "network", titleKey: "home.online.title", descKey: "home.online.description", gated: true },
  { key: "draft", to: "/draft", tone: "ember", icon: "draft", motif: "pack", titleKey: "home.draft.title", descKey: "home.draft.description", gated: false },
];

/** Home action tile: adapts an ActionDef (section-icon PNG + route) onto the
 *  shared MenuActionTile bento grammar. */
function CardActionButton({ action, disabled }: { action: ActionDef; disabled: boolean }) {
  const { t } = useTranslation("menu");
  const navigate = useNavigate();
  const src = `/icons/sections/${action.icon}.png`;
  return (
    <MenuActionTile
      tone={action.tone}
      title={t(action.titleKey)}
      description={t(action.descKey)}
      enterLabel={t("home.dashboard.enter")}
      disabled={disabled}
      motif={action.motif}
      onClick={() => navigate(action.to)}
      renderIcon={(className) => (
        <img src={src} alt="" aria-hidden="true" draggable={false} className={className} />
      )}
    />
  );
}

/* ----------------------------------------------------------------- resume -- */
function ResumeHero() {
  const { t } = useTranslation("menu");
  const navigate = useNavigate();
  const { match, matchSummary, quickDraft, pod, resumeMatch } = useResumables();

  // Build the ordered candidate list: the saved match leads (most-relevant
  // resumable), then drafts by recency. Nothing to resume → render nothing.
  const draftEntries = [
    quickDraft && { updatedAt: quickDraft.updatedAt, cta: t("home.dashboard.resumeDraft"), title: quickDraft.setName ?? quickDraft.setCode.toUpperCase(), chip: t("home.draft.title"), meta: [] as string[], onResume: () => navigate("/draft") },
    pod && { updatedAt: pod.updatedAt, cta: t("home.dashboard.resumeDraft"), title: t("home.dashboard.draftPod"), chip: t("home.dashboard.draftPod"), meta: [] as string[], onResume: () => navigate("/draft") },
  ].filter(Boolean) as { updatedAt: number; cta: string; title: string; chip: string; meta: string[]; onResume: () => void }[];
  draftEntries.sort((a, b) => b.updatedAt - a.updatedAt);

  // Build a human-readable identity from what the saved match actually carries.
  // The headline names the format (the match's identity); the chip carries
  // mode + AI difficulty; the meta line reads as plain language — turn, whose
  // turn it is, and your own life — never bare unlabeled numbers.
  const modeLabel = match ? t(`home.dashboard.mode.${match.mode}`) : "";
  const difficultyLabel =
    match && match.mode === "ai" && match.difficulty
      ? t(`aiDifficulty.levels.${match.difficulty}`)
      : null;
  // GameFormat is PascalCase ("PauperCommander"); space compound names for display.
  const formatLabel = match?.formatConfig?.format?.replace(/([a-z])([A-Z])/g, "$1 $2");

  const matchMeta: string[] = [];
  if (matchSummary) {
    matchMeta.push(t("home.dashboard.turn", { n: matchSummary.turn }));
    // "Your turn" framing only makes sense where seat 0 is the local human.
    if (match && (match.mode === "ai" || match.mode === "p2p-host")) {
      matchMeta.push(matchSummary.isYourTurn ? t("home.dashboard.yourTurn") : t("home.dashboard.opponentTurn"));
    }
    if (matchSummary.yourLife != null) {
      matchMeta.push(t("home.dashboard.yourLife", { life: matchSummary.yourLife }));
    }
  }
  // When a format names the headline, the chip carries mode + difficulty. When
  // it doesn't, the mode label becomes the headline, so the chip drops it to
  // avoid echoing the title ("vs AI" headline + "vs AI ·" chip).
  const chipParts = formatLabel ? [modeLabel, difficultyLabel] : [difficultyLabel];
  const matchEntry = match && {
    cta: t("home.resume.title"),
    title: formatLabel || modeLabel || t("home.resume.title"),
    chip: chipParts.filter(Boolean).join(" · "),
    meta: matchMeta,
    onResume: resumeMatch,
  };

  const ordered = [matchEntry, ...draftEntries].filter(Boolean) as { cta: string; title: string; chip: string; meta: string[]; onResume: () => void }[];
  if (ordered.length === 0) return null;
  const [primary, secondary] = ordered;

  return (
    <button
      type="button"
      onClick={primary.onResume}
      className="group relative w-full overflow-hidden rounded-card border border-ember/25 p-5 text-left transition-all duration-150 surface-card hover:-translate-y-[3px] hover:border-ember/40"
    >
      <div className="pointer-events-none absolute inset-0 bg-[radial-gradient(130%_120%_at_92%_0%,rgba(180,83,30,0.28),transparent_46%)]" />
      <img src="/logo_only.webp" alt="" aria-hidden="true" className="pointer-events-none absolute -right-10 top-1/2 w-44 -translate-y-1/2 -rotate-6 opacity-[0.06] grayscale" />
      <div className="relative flex flex-col gap-3">
        <div className="flex items-center gap-4">
          <div className="min-w-0 flex-1">
            <div className="text-[0.7rem] font-semibold uppercase tracking-[0.18em] text-ember-soft">
              {t("home.dashboard.continue")}
            </div>
            <div className="mt-1 truncate font-display text-[clamp(1.35rem,2.4vw,1.7rem)] font-semibold tracking-[-0.02em] text-fg">
              {primary.title}
            </div>
            <div className="mt-2 flex flex-wrap items-center gap-x-2.5 gap-y-1.5 text-xs text-fg-muted">
              {primary.chip && (
                <span className="rounded-badge border border-ember-soft/20 bg-ember/[0.12] px-2 py-0.5 font-medium text-ember-soft">
                  {primary.chip}
                </span>
              )}
              {primary.meta.length > 0 && (
                <span className="tabular-nums">{primary.meta.join(" · ")}</span>
              )}
            </div>
          </div>
          <span className="inline-flex shrink-0 items-center gap-1.5 rounded-full border border-ember-soft/30 bg-ember/[0.12] px-4 py-2 text-sm font-medium text-ember-text transition-colors group-hover:border-ember-soft/50">
            <img src="/icons/sections/resume.png" alt="" aria-hidden="true" className="h-4 w-4 opacity-80" />
            {primary.cta}
          </span>
        </div>
        {secondary && (
          <span
            role="button"
            tabIndex={0}
            onClick={(e) => { e.stopPropagation(); secondary.onResume(); }}
            onKeyDown={(e) => { if (e.key === "Enter") { e.stopPropagation(); secondary.onResume(); } }}
            className="flex w-full cursor-pointer items-center gap-2.5 rounded-[9px] border border-hairline bg-black/28 px-3 py-2 backdrop-blur-sm transition-colors hover:border-hairline-hover hover:bg-black/40"
          >
            <span className="min-w-0 truncate text-[13px] text-slate-300">
              <b className="font-semibold text-fg">{secondary.cta}</b> — {secondary.title}
            </span>
            <span className="ml-auto text-xs text-fg-meta">{secondary.chip}</span>
          </span>
        )}
      </div>
    </button>
  );
}

/* ------------------------------------------------------------- info cards -- */
const INFO_CARD = "rounded-card border border-hairline p-5 surface-card";
const SECTION_LABEL = "text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-fg-meta";

function ActiveDeckCard() {
  const { t } = useTranslation("menu");
  const navigate = useNavigate();
  const [name, setName] = useState<string | null>(null);
  useEffect(() => setName(localStorage.getItem(ACTIVE_DECK_KEY)), []);

  // Resolve the deck's representative card so the card can show real art, the
  // same way the deck tiles do. Hook runs unconditionally (Rules of Hooks);
  // an empty name yields no src and falls back to the loading shimmer.
  const repCard = name ? getRepresentativeCard(name) : null;
  const { src: artSrc } = useCardImage(repCard ?? "", { size: "art_crop" });

  if (!name) {
    return (
      <button type="button" onClick={() => navigate("/my-decks")} className={`${INFO_CARD} flex cursor-pointer flex-col items-start text-left transition-colors hover:border-hairline-hover`}>
        <div className={SECTION_LABEL}>{t("home.dashboard.activeDeck")}</div>
        <p className="mt-2 text-sm text-fg-muted">{t("home.dashboard.noActiveDeck")}</p>
      </button>
    );
  }
  const count = getDeckCardCount(name);
  const colors = getDeckColorIdentity(name);
  return (
    <button
      type="button"
      onClick={() => navigate("/my-decks")}
      className="group flex cursor-pointer flex-col overflow-hidden rounded-card border border-hairline surface-card text-left transition-colors hover:border-hairline-hover"
    >
      {/* Art header: representative card art with the section label and the
          color-identity mana pips overlaid on a scrim — the pips carry the same
          dark backing chip so they stay legible over any artwork. */}
      <div className="relative h-24 overflow-hidden">
        {artSrc ? (
          <img src={artSrc} alt="" className="absolute inset-0 h-full w-full object-cover" />
        ) : (
          <div className="absolute inset-0 animate-pulse bg-gray-800" />
        )}
        <div className="absolute inset-0 bg-gradient-to-t from-black/85 via-black/25 to-transparent" />
        <div className={`${SECTION_LABEL} absolute left-3 top-2.5 z-10 drop-shadow-[0_1px_2px_rgba(0,0,0,0.7)]`}>
          {t("home.dashboard.activeDeck")}
        </div>
        {colors.length > 0 && (
          <div className="absolute right-2.5 top-2.5 z-10 flex items-center gap-0.5 rounded-full bg-black/75 px-2 py-1 shadow-[0_2px_8px_rgba(0,0,0,0.6)] ring-1 ring-white/20 backdrop-blur-sm">
            {colors.map((c) => <ManaSymbol key={c} shard={c} size="xs" />)}
          </div>
        )}
      </div>
      {/* Body: deck name + card count. */}
      <div className="flex flex-1 flex-col justify-center gap-1 px-4 py-3">
        <div className="truncate font-display text-[1.12rem] font-semibold tracking-[-0.02em] text-fg">{name}</div>
        <span className="text-xs text-fg-muted tabular-nums">{t("home.dashboard.cards", { count })}</span>
      </div>
    </button>
  );
}

interface FormatCoverageSummary { total_cards: number; supported_cards: number; coverage_pct: number; }
const FORMAT_DISPLAY = [
  ["standard", "Standard"], ["commander", "Commander"], ["modern", "Modern"],
  ["premodern", "Premodern"], ["pioneer", "Pioneer"], ["legacy", "Legacy"],
  ["vintage", "Vintage"], ["pauper", "Pauper"], ["historic", "Historic"],
] as const;

function CoverageCard() {
  const { t } = useTranslation("menu");
  const navigate = useNavigate();
  const [rows, setRows] = useState<[string, number][]>([]);
  useEffect(() => {
    fetch(__COVERAGE_SUMMARY_URL__)
      .then((r) => (r.ok ? r.json() : null))
      .then((data) => {
        if (!data?.coverage_by_format) return;
        const by = data.coverage_by_format as Record<string, FormatCoverageSummary>;
        setRows(
          FORMAT_DISPLAY.flatMap(([k, label]) => {
            const s = by[k];
            return s && s.total_cards > 0 ? [[label, s.coverage_pct] as [string, number]] : [];
          }),
        );
      })
      .catch(() => {});
  }, []);
  if (rows.length === 0) return null;
  const color = (p: number) => (p > 70 ? "text-jade" : p > 40 ? "text-ember-soft" : "text-rose");
  return (
    <button type="button" onClick={() => navigate("/coverage")} className={`${INFO_CARD} cursor-pointer text-left transition-colors hover:border-hairline-hover`}>
      <div className={`${SECTION_LABEL} mb-3 flex items-center gap-2`}>
        <img src="/icons/sections/coverage.png" alt="" aria-hidden="true" className="h-3.5 w-3.5 opacity-70" />
        {t("home.coverage.heading")}
      </div>
      <div className="grid grid-cols-2 gap-x-5 gap-y-1.5">
        {rows.map(([label, pct]) => (
          <div key={label} className="flex items-center justify-between text-[12.5px] text-fg-muted">
            <span className="text-[11px] font-semibold uppercase tracking-wide text-fg-meta">{label}</span>
            <span className={`font-mono tabular-nums ${color(pct)}`}>{pct.toFixed(0)}%</span>
          </div>
        ))}
      </div>
    </button>
  );
}

/* ------------------------------------------------------------- dashboard --- */
export function HomeDashboard() {
  const { t } = useTranslation("menu");
  const cardStatus = useCardDataStore((s) => s.status);
  const cardsPending = cardStatus !== "ready" && cardStatus !== "error";

  const masthead = (
    <div className="w-full">
      <div className="flex items-center gap-3">
        <img
          src="/logo_only.webp"
          alt=""
          aria-hidden="true"
          className="h-9 w-9 drop-shadow-[0_0_14px_rgba(251,146,60,0.45)]"
        />
        <span className="font-display text-[1.7rem] font-semibold tracking-[-0.02em] text-fg">phase.rs</span>
        <span className="rounded-full border border-ember-soft/25 bg-ember/[0.12] px-2 py-0.5 text-[9px] font-bold uppercase tracking-[0.14em] text-ember-soft">
          {t("nav.alpha")}
        </span>
      </div>
      <p className="mt-2.5 max-w-2xl text-[0.95rem] leading-relaxed text-fg-muted">
        {t("home.dashboard.tagline")}
      </p>
    </div>
  );

  return (
    <MenuShell header={masthead} layout="stacked">
      <div className="flex w-full flex-col gap-4">
        <ResumeHero />
        <div className="grid grid-cols-1 gap-4 min-[680px]:grid-cols-3">
          {ACTIONS.map((a) => (
            <CardActionButton key={a.key} action={a} disabled={a.gated && cardsPending} />
          ))}
        </div>
        <div className="grid grid-cols-1 gap-4 min-[760px]:grid-cols-2">
          <ActiveDeckCard />
          <CoverageCard />
        </div>
      </div>
    </MenuShell>
  );
}
