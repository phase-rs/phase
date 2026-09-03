import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { useNavigate } from "react-router";

import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { COMMANDER_DRAFT_ENTRY, draftKindLabels } from "../components/draft/draftKind";
import { MenuShell } from "../components/menu/MenuShell";
import { MenuActionTile } from "../components/menu/MenuActionTile";
import { PodIcon } from "../components/draft/PodIcon";
import {
  loadActiveQuickDraft,
  type ActiveQuickDraftMeta,
} from "../services/quickDraftPersistence";
import {
  loadActiveDraftGuest,
  loadActiveDraftPod,
  type ActiveDraftGuestMeta,
  type ActiveDraftPodMeta,
} from "../services/draftPersistence";
import { loadGame } from "../services/gamePersistence";
import { useEffectiveOffline } from "../stores/connectivityStore";

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

/**
 * The set whose icon stands for a draft.
 *
 * A multi-set draft's `setCode` joins its distinct sets (`"ISD+DKA+AVR"`,
 * matching the engine's own `DraftSource::set_code` label), which resolves to
 * no icon of its own; the first set it opened represents it.
 */
function primarySetCode(code: string): string {
  return code.split("+")[0] ?? code;
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
  // The shared bento tile's "Enter" CTA lives in the menu namespace; reuse it
  // rather than duplicating the string into draft locales.
  const { t: tMenu } = useTranslation("menu");
  const navigate = useNavigate();
  const effectiveOffline = useEffectiveOffline();
  const [activeDraft, setActiveDraft] = useState<ActiveQuickDraftMeta | null>(null);
  const [activePod, setActivePod] = useState<ActiveDraftPodMeta | null>(null);
  const [activeGuestPod, setActiveGuestPod] = useState<ActiveDraftGuestMeta | null>(null);

  useEffect(() => {
    setActiveDraft(loadActiveQuickDraft());
    setActivePod(loadActiveDraftPod());
    setActiveGuestPod(loadActiveDraftGuest());
  }, []);

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <ScreenChrome onBack={() => navigate("/")} />
      <div className="menu-scene__vignette" />
      <div className="menu-scene__haze" />

      <MenuShell
        eyebrow={t("landing.eyebrow")}
        title={t("landing.title")}
        description={t("landing.description")}
        layout="stacked"
        contentWidthClass="max-w-3xl"
      >
        <div className="flex w-full flex-col">
          {activeDraft && <ActiveDraftCard meta={activeDraft} />}
          {activePod && <ActivePodCard meta={activePod} />}
          {activeGuestPod && <ActiveGuestPodCard meta={activeGuestPod} />}

          <div className="flex flex-col gap-3">
            <h2 className="text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-fg-meta">
              {t("landing.startNew")}
            </h2>

            {/* Same bento action tiles as the home dashboard — one accent tone
                per mode — so the draft landing shares the home card grammar. */}
            <div className="grid grid-cols-1 gap-4 min-[640px]:grid-cols-2">
              <MenuActionTile
                tone="arcane"
                motif="pack"
                title={t("landing.quickDraft.title")}
                description={t("landing.quickDraft.description")}
                enterLabel={tMenu("home.dashboard.enter")}
                renderIcon={(cls) => <BotIcon className={cls} />}
                onClick={() => navigate("/draft/quick")}
              />
              <MenuActionTile
                tone="ember"
                motif="pack"
                title={t("landing.sealed.title")}
                description={t("landing.sealed.description")}
                enterLabel={tMenu("home.dashboard.enter")}
                renderIcon={(cls) => <PackIcon className={cls} />}
                onClick={() => navigate("/draft/quick?mode=sealed")}
              />
              <MenuActionTile
                tone="ember"
                motif="pack"
                title={t("landing.cubeDraft.title")}
                description={t("landing.cubeDraft.description")}
                enterLabel={tMenu("home.dashboard.enter")}
                renderIcon={(cls) => <CubeIcon className={cls} />}
                onClick={() => navigate("/draft/quick?mode=cube")}
              />
              <MenuActionTile
                tone="jade"
                motif="network"
                title={t("landing.podDraft.title")}
                description={effectiveOffline
                  ? t("offline.startUnavailable")
                  : t("landing.podDraft.description")}
                enterLabel={tMenu("home.dashboard.enter")}
                renderIcon={(cls) => <PodIcon className={cls} />}
                onClick={() => navigate("/draft-pod")}
                disabled={effectiveOffline}
              />
              <MenuActionTile
                tone="arcane"
                motif="network"
                title={t("landing.commanderDraft.title")}
                description={effectiveOffline
                  ? t("offline.startUnavailable")
                  : t("landing.commanderDraft.description")}
                enterLabel={tMenu("home.dashboard.enter")}
                renderIcon={(cls) => <CrownIcon className={cls} />}
                onClick={() => navigate(`/draft-pod?kind=${COMMANDER_DRAFT_ENTRY}`)}
                disabled={effectiveOffline}
              />
            </div>
          </div>
        </div>
      </MenuShell>
    </div>
  );
}

function ActivePodCard({ meta }: { meta: ActiveDraftPodMeta }) {
  const { t } = useTranslation("draft");
  const navigate = useNavigate();
  // `landing.podLabel` interpolates the kind into a sentence, so a raw enum reads
  // "CommanderDraft Pod". `draftKindLabels` is the single authority for that
  // rendering, shared with the pod lobby header.
  const kindLabel = draftKindLabels(t);

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
      <h2 className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-fg-meta">
        {t("landing.podInProgress")}
      </h2>
      <button
        type="button"
        onClick={() => navigate("/draft-pod?entry=host")}
        className="group flex w-full cursor-pointer items-center gap-5 rounded-[20px] border border-cyan-300/20 bg-cyan-400/[0.06] p-5 text-left transition-colors hover:border-cyan-300/35 hover:bg-cyan-400/[0.10]"
      >
        <div className="flex h-14 w-14 shrink-0 items-center justify-center rounded-2xl border border-white/8 bg-black/24">
          <PodIcon />
        </div>

        <div className="min-w-0 flex-1">
          <div className="text-lg font-semibold text-white">
            {t("landing.podLabel", { kind: kindLabel[meta.kind] })}
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

function ActiveGuestPodCard({ meta }: { meta: ActiveDraftGuestMeta }) {
  const { t } = useTranslation("draft");
  const navigate = useNavigate();

  return (
    <div className="mb-8">
      <h2 className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-fg-meta">
        {t("landing.guestPodInProgress")}
      </h2>
      <button
        type="button"
        onClick={() => navigate("/draft-pod?entry=guest")}
        className="group flex w-full cursor-pointer items-center gap-5 rounded-[20px] border border-violet-300/20 bg-violet-400/[0.06] p-5 text-left transition-colors hover:border-violet-300/35 hover:bg-violet-400/[0.10]"
      >
        <div className="flex h-14 w-14 shrink-0 items-center justify-center rounded-2xl border border-white/8 bg-black/24">
          <PodIcon />
        </div>
        <div className="min-w-0 flex-1">
          <div className="text-lg font-semibold text-white">{t("landing.guestPodLabel")}</div>
          <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-white/45">
            <span>{t("landing.roomLabel", { code: meta.roomCode })}</span>
            <span>{formatRelativeTime(meta.timestamp, t)}</span>
          </div>
        </div>
        <div className="flex items-center self-stretch pl-2">
          <div className="rounded-full border border-violet-300/15 bg-violet-400/10 px-4 py-2 text-sm font-medium text-violet-100 transition-colors group-hover:border-violet-300/30 group-hover:bg-violet-400/18">
            {t("landing.reconnect")}
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
        const icon = data?.[primarySetCode(meta.setCode).toLowerCase()]?.icon_svg_uri;
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
      case "opening": return t("quickPhase.opening");
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
    return t("quickPhase.drafting");
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
      <h2 className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-fg-meta">
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

function BotIcon({ className = "h-6 w-6" }: { className?: string }) {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className={`${className} fill-current`}>
      <path d="M17.753 14a2.25 2.25 0 0 1 2.25 2.25v.904A3.75 3.75 0 0 1 18.696 20H5.304a3.75 3.75 0 0 1-1.307-2.846v-.904A2.25 2.25 0 0 1 6.247 14h11.506ZM11 15.5H8a.5.5 0 0 0-.5.5v1a.5.5 0 0 0 .5.5h3a.5.5 0 0 0 .5-.5v-1a.5.5 0 0 0-.5-.5Zm5 0h-1.5a.5.5 0 0 0-.5.5v1a.5.5 0 0 0 .5.5H16a.5.5 0 0 0 .5-.5v-1a.5.5 0 0 0-.5-.5ZM12 2a4 4 0 0 1 4 4v4a4 4 0 0 1-8 0V6a4 4 0 0 1 4-4Zm-1.5 5a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3Zm3 0a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3Z" />
    </svg>
  );
}

function CrownIcon({ className = "h-6 w-6" }: { className?: string }) {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className={`${className} fill-current`}>
      <path d="M5 18h14l1.6-9.5-4.6 3.2L12 4.5 7.99 11.7 3.4 8.5 5 18Zm0 1.5h14V21H5v-1.5Z" />
    </svg>
  );
}

function CubeIcon({ className = "h-6 w-6" }: { className?: string }) {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className={`${className} fill-current`}>
      <path d="M12 2.4 3.5 6.8v10.4L12 21.6l8.5-4.4V6.8L12 2.4Zm0 2.25 5.55 2.88L12 10.4 6.45 7.53 12 4.65Zm-6.5 4.5 5.5 2.85v6.8l-5.5-2.85v-6.8Zm7.5 9.65V12l5.5-2.85v6.8L13 18.8Z" />
    </svg>
  );
}

function PackIcon({ className = "h-6 w-6" }: { className?: string }) {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className={`${className} fill-current`}>
      <path d="M5.25 2.5h13.5l1.75 19H3.5l1.75-19ZM7.1 4.5 5.72 19.5h12.56L16.9 4.5H7.1Zm4.9 2.25 2.75 3.75L12 14.25 9.25 10.5 12 6.75Z" />
    </svg>
  );
}
