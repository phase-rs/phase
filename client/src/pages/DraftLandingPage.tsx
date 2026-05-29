import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { useNavigate } from "react-router";

import { ScreenChrome } from "../components/chrome/ScreenChrome";
import {
  loadActiveQuickDraft,
  type ActiveQuickDraftMeta,
} from "../services/quickDraftPersistence";
import {
  loadActiveDraftPod,
  type ActiveDraftPodMeta,
} from "../services/draftPersistence";
import { loadGame } from "../services/gamePersistence";

const SET_LABELS: Record<string, string> = {
  otj: "Outlaws of Thunder Junction",
  mkm: "Murders at Karlov Manor",
  lci: "The Lost Caverns of Ixalan",
  woe: "Wilds of Eldraine",
  mom: "March of the Machine",
  one: "Phyrexia: All Will Be One",
  bro: "The Brothers' War",
  dmu: "Dominaria United",
  snc: "Streets of New Capenna",
  dsk: "Duskmourn",
  blb: "Bloomburrow",
  fdn: "Foundations",
};

const DIFFICULTY_NAMES = ["VeryEasy", "Easy", "Medium", "Hard", "VeryHard"] as const;

const DIFFICULTY_LABELS = [
  "Very Easy",
  "Easy",
  "Medium",
  "Hard",
  "Very Hard",
] as const;

function formatSetLabel(code: string, name?: string): string {
  return name ?? SET_LABELS[code.toLowerCase()] ?? code.toUpperCase();
}

function formatRelativeTime(timestamp: number, t: TFunction<"draft">): string {
  const seconds = Math.floor((Date.now() - timestamp) / 1000);
  if (seconds < 60) return t("relativeTime.justNow");
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return t("relativeTime.minutes", { count: minutes });
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return t("relativeTime.hours", { count: hours });
  return t("relativeTime.days", { count: Math.floor(hours / 24) });
}

export function DraftLandingPage() {
  const { t } = useTranslation("draft");
  const navigate = useNavigate();
  const [activeDraft, setActiveDraft] = useState<ActiveQuickDraftMeta | null>(null);
  const [activePod, setActivePod] = useState<ActiveDraftPodMeta | null>(null);

  useEffect(() => {
    setActiveDraft(loadActiveQuickDraft());
    setActivePod(loadActiveDraftPod());
  }, []);

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <ScreenChrome onBack={() => navigate("/")} />
      <div className="menu-scene__vignette" />
      <div className="menu-scene__haze" />

      <div className="relative z-10 mx-auto flex w-full max-w-3xl flex-col px-6 py-20">
        <h1 className="menu-display mb-10 text-4xl text-white">{t("landing.title")}</h1>

        {activeDraft && <ActiveDraftCard meta={activeDraft} />}
        {activePod && <ActivePodCard meta={activePod} />}

        <div className="flex flex-col gap-3">
          <h2 className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
            {t("landing.startNew")}
          </h2>

          <DraftModeCard
            title={t("landing.quickDraft.title")}
            description={t("landing.quickDraft.description")}
            icon={<BotIcon />}
            onClick={() => navigate("/draft/quick")}
          />

          <DraftModeCard
            title={t("landing.cubeDraft.title")}
            description={t("landing.cubeDraft.description")}
            icon={<CubeIcon />}
            onClick={() => navigate("/draft/quick?mode=cube")}
          />

          <DraftModeCard
            title={t("landing.podDraft.title")}
            description={t("landing.podDraft.description")}
            icon={<PodIcon />}
            onClick={() => navigate("/draft-pod")}
          />
        </div>
      </div>
    </div>
  );
}

function ActivePodCard({ meta }: { meta: ActiveDraftPodMeta }) {
  const { t } = useTranslation("draft");
  const navigate = useNavigate();

  function getPhaseLabel(): string {
    switch (meta.phase) {
      case "lobby": return t("podPhase.lobby");
      case "drafting": return t("podPhase.drafting");
      case "deckbuilding": return t("podPhase.deckbuilding");
      case "pairing": return t("podPhase.pairing");
      case "matchInProgress": return t("podPhase.matchInProgress");
      case "complete": return t("podPhase.complete");
    }
  }

  return (
    <div className="mb-8">
      <h2 className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
        {t("landing.podInProgress")}
      </h2>
      <button
        type="button"
        onClick={() => navigate("/draft-pod?resume=1")}
        className="group flex w-full cursor-pointer items-center gap-5 rounded-[20px] border border-cyan-300/20 bg-cyan-400/[0.06] p-5 text-left transition-colors hover:border-cyan-300/35 hover:bg-cyan-400/[0.10]"
      >
        <div className="flex h-14 w-14 shrink-0 items-center justify-center rounded-2xl border border-white/8 bg-black/24">
          <PodIcon />
        </div>

        <div className="min-w-0 flex-1">
          <div className="text-lg font-semibold text-white">
            {t("landing.podLabel", { kind: meta.kind })}
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-white/45">
            <span className="rounded-md border border-cyan-300/20 bg-cyan-400/10 px-2 py-0.5 text-xs font-medium text-cyan-100">
              {getPhaseLabel()}
            </span>
            <span>{t("landing.seatCount", { count: meta.podSize })}</span>
            {meta.roomCode && <span>{t("landing.roomLabel", { code: meta.roomCode })}</span>}
            {meta.phase === "drafting" && meta.pickCount > 0 && (
              <span>{t("landing.cardsPicked", { count: meta.pickCount })}</span>
            )}
            <span>{formatRelativeTime(meta.updatedAt, t)}</span>
          </div>
        </div>

        <div className="flex items-center self-stretch pl-2">
          <div className="rounded-full border border-cyan-300/15 bg-cyan-400/10 px-4 py-2 text-sm font-medium text-cyan-100 transition-colors group-hover:border-cyan-300/30 group-hover:bg-cyan-400/18">
            {t("landing.resume")}
          </div>
        </div>
      </button>
    </div>
  );
}

function ActiveDraftCard({ meta }: { meta: ActiveQuickDraftMeta }) {
  const { t } = useTranslation("draft");
  const navigate = useNavigate();
  const [setIcon, setSetIcon] = useState<string | null>(null);

  useEffect(() => {
    fetch(__SCRYFALL_SETS_URL__)
      .then((res) => (res.ok ? res.json() : null))
      .then((data: Record<string, { icon_svg_uri?: string }> | null) => {
        const icon = data?.[meta.setCode.toLowerCase()]?.icon_svg_uri;
        if (icon) setSetIcon(icon);
      })
      .catch(() => {});
  }, [meta.setCode]);

  const difficultyLabel = DIFFICULTY_LABELS[meta.difficulty] ?? "Medium";
  const [midMatchGameId, setMidMatchGameId] = useState<string | null>(null);

  useEffect(() => {
    if (!meta.currentGameId) return;
    loadGame(meta.currentGameId).then((saved) => {
      if (saved) setMidMatchGameId(meta.currentGameId!);
    });
  }, [meta.currentGameId]);

  function getPhaseLabel(): string {
    switch (meta.phase) {
      case "drafting": return t("quickPhase.drafting");
      case "deckbuilding": return t("quickPhase.deckbuilding");
      case "playing": {
        const w = meta.runWins ?? 0;
        const l = meta.runLosses ?? 0;
        const matchNum = w + l + (meta.runDraws ?? 0) + 1;
        return midMatchGameId
          ? t("quickPhase.matchRecord", { number: matchNum, wins: w, losses: l })
          : t("quickPhase.record", { wins: w, losses: l });
      }
      case "complete":
        return t("quickPhase.runComplete", { wins: meta.runWins ?? 0, losses: meta.runLosses ?? 0 });
    }
  }

  function handleClick() {
    if (midMatchGameId) {
      navigate(`/game/${midMatchGameId}?mode=ai&difficulty=${DIFFICULTY_NAMES[meta.difficulty] ?? "Medium"}&format=Limited&match=bo1&source=draft&draftId=${meta.id}`);
    } else {
      navigate("/draft/quick?resume=1");
    }
  }

  const resumeLabel = midMatchGameId
    ? t("landing.resumeMatch")
    : meta.phase === "complete"
      ? t("landing.viewResults")
      : t("landing.resume");
  const heading = meta.phase === "complete" ? t("landing.draftComplete") : t("landing.draftInProgress");

  return (
    <div className="mb-8">
      <h2 className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
        {heading}
      </h2>
      <button
        type="button"
        onClick={handleClick}
        className="group flex w-full cursor-pointer items-center gap-5 rounded-[20px] border border-amber-400/20 bg-amber-500/[0.06] p-5 text-left transition-colors hover:border-amber-400/35 hover:bg-amber-500/[0.10]"
      >
        <div className="flex h-14 w-14 shrink-0 items-center justify-center rounded-2xl border border-white/8 bg-black/24">
          {setIcon ? (
            <img
              src={setIcon}
              alt={t("landing.setIconAlt", { code: meta.setCode })}
              className="h-8 w-8 opacity-80 invert"
            />
          ) : (
            <span className="text-lg font-bold tracking-wider text-white/60">
              {meta.setCode.toUpperCase()}
            </span>
          )}
        </div>

        <div className="min-w-0 flex-1">
          <div className="text-lg font-semibold text-white">
            {formatSetLabel(meta.setCode, meta.setName)}
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-white/45">
            <span className="rounded-md border border-amber-400/20 bg-amber-400/10 px-2 py-0.5 text-xs font-medium text-amber-200">
              {getPhaseLabel()}
            </span>
            <span>{difficultyLabel}</span>
            {meta.phase === "drafting" && meta.pickCount > 0 && (
              <span>{t("landing.cardsPicked", { count: meta.pickCount })}</span>
            )}
            <span>{formatRelativeTime(meta.updatedAt, t)}</span>
          </div>
        </div>

        <div className="flex items-center self-stretch pl-2">
          <div className="rounded-full border border-amber-400/15 bg-amber-500/10 px-4 py-2 text-sm font-medium text-amber-200 transition-colors group-hover:border-amber-400/30 group-hover:bg-amber-500/18">
            {resumeLabel}
          </div>
        </div>
      </button>
    </div>
  );
}

function DraftModeCard({
  title,
  description,
  icon,
  onClick,
}: {
  title: string;
  description: string;
  icon: ReactNode;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="group flex w-full cursor-pointer items-center gap-4 rounded-[18px] border border-white/10 bg-white/[0.02] p-4 text-left transition-colors hover:border-white/18 hover:bg-white/[0.05]"
    >
      <div className="flex h-11 w-11 shrink-0 items-center justify-center rounded-xl border border-white/8 bg-black/18 text-white/50 group-hover:text-white/70">
        {icon}
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <div className="text-base font-semibold text-white">{title}</div>
        </div>
        <p className="mt-1 text-sm text-white/40">{description}</p>
      </div>
      <div className="rounded-full border border-white/10 bg-black/18 px-3 py-2 text-white/40 transition-colors group-hover:text-white/70">
        <svg aria-hidden="true" viewBox="0 0 24 24" className="h-5 w-5 fill-current">
          <path d="m13.2 5.4 1.4-1.4 8 8-8 8-1.4-1.4 5.6-5.6H2v-2h16.8l-5.6-5.6Z" />
        </svg>
      </div>
    </button>
  );
}

function BotIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-6 w-6 fill-current">
      <path d="M17.753 14a2.25 2.25 0 0 1 2.25 2.25v.904A3.75 3.75 0 0 1 18.696 20H5.304a3.75 3.75 0 0 1-1.307-2.846v-.904A2.25 2.25 0 0 1 6.247 14h11.506ZM11 15.5H8a.5.5 0 0 0-.5.5v1a.5.5 0 0 0 .5.5h3a.5.5 0 0 0 .5-.5v-1a.5.5 0 0 0-.5-.5Zm5 0h-1.5a.5.5 0 0 0-.5.5v1a.5.5 0 0 0 .5.5H16a.5.5 0 0 0 .5-.5v-1a.5.5 0 0 0-.5-.5ZM12 2a4 4 0 0 1 4 4v4a4 4 0 0 1-8 0V6a4 4 0 0 1 4-4Zm-1.5 5a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3Zm3 0a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3Z" />
    </svg>
  );
}

function CubeIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-6 w-6 fill-current">
      <path d="M12 2.4 3.5 6.8v10.4L12 21.6l8.5-4.4V6.8L12 2.4Zm0 2.25 5.55 2.88L12 10.4 6.45 7.53 12 4.65Zm-6.5 4.5 5.5 2.85v6.8l-5.5-2.85v-6.8Zm7.5 9.65V12l5.5-2.85v6.8L13 18.8Z" />
    </svg>
  );
}

function PodIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-6 w-6 fill-current">
      <path d="M16 11c1.66 0 2.99-1.34 2.99-3S17.66 5 16 5s-3 1.34-3 3 1.34 3 3 3Zm-8 0c1.66 0 2.99-1.34 2.99-3S9.66 5 8 5 5 6.34 5 8s1.34 3 3 3Zm0 2c-2.33 0-7 1.17-7 3.5V19h14v-2.5c0-2.33-4.67-3.5-7-3.5Zm8 0c-.29 0-.62.02-.97.05 1.16.84 1.97 1.97 1.97 3.45V19h6v-2.5c0-2.33-4.67-3.5-7-3.5Z" />
    </svg>
  );
}
