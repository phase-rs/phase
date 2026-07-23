import { useEffect, useRef, useState, type ReactNode } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { useTranslation } from "react-i18next";

import { ConnectionDot } from "../multiplayer/ConnectionDot.tsx";
import { FullscreenButton } from "./FullscreenButton.tsx";
import { VolumeControl } from "./VolumeControl.tsx";
import { clearGame } from "../../stores/gameStore.ts";
import { useDraftStore } from "../../stores/draftStore.ts";
import { useCardDataMeta } from "../../hooks/useCardDataMeta.ts";
import { useConcedeHandler } from "../../hooks/useConcedeHandler.ts";
import type { MultiplayerBoardLayout } from "../../stores/preferencesStore.ts";

interface GameMenuProps {
  gameId: string;
  isAiMode: boolean;
  isOnlineMode: boolean;
  showAiHand: boolean;
  onToggleAiHand: () => void;
  multiplayerBoardLayout?: MultiplayerBoardLayout;
  onToggleMultiplayerBoardLayout?: () => void;
  onSettingsClick: () => void;
  onHelpClick: () => void;
  onConcede?: () => void;
  /** GH #1507: ask every other human player to approve rolling the game
   * back to the state before this player's last action. Online-only. */
  onRequestTakeback?: () => void;
  /** Show the always-visible Sandbox Tools button. Gated by the caller to
   *  game modes where debug actions actually work (vs-AI, local, or a
   *  multiplayer sandbox). */
  showSandboxTools?: boolean;
  onSandboxToolsClick?: () => void;
  debugClickModeButtonVisible?: boolean;
  onToggleDebugClickModeButtonVisible?: () => void;
  /** Show the always-visible "Report a card problem" flag button. Gated by the
   *  caller to live, participating (non-spectate) games. */
  showReportCard?: boolean;
  onReportCardClick?: () => void;
}

export function GameMenu({
  gameId,
  isAiMode,
  isOnlineMode,
  showAiHand,
  onToggleAiHand,
  multiplayerBoardLayout,
  onToggleMultiplayerBoardLayout,
  onSettingsClick,
  onHelpClick,
  onConcede,
  onRequestTakeback,
  showSandboxTools,
  onSandboxToolsClick,
  debugClickModeButtonVisible = false,
  onToggleDebugClickModeButtonVisible,
  showReportCard,
  onReportCardClick,
}: GameMenuProps) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const [open, setOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const cardDataMeta = useCardDataMeta();
  const isDraft = searchParams.get("source") === "draft" && !!searchParams.get("draftId");
  const isDraftPodMatch = searchParams.get("mode") === "draft-match";
  const boardLayoutToggleLabel = multiplayerBoardLayout === "split"
    ? t("gameMenu.boardLayoutSplit")
    : t("gameMenu.boardLayoutLegacy");
  const boardLayoutToggleTitle = multiplayerBoardLayout === "split"
    ? t("gameMenu.switchToLegacyView")
    : t("gameMenu.switchToSplitView");

  const handleConcede = useConcedeHandler({
    gameId,
    isOnlineMode,
    isDraft,
    isDraftPodMatch,
    onConcede,
  });

  useEffect(() => {
    if (!open) return;
    function handleClick(e: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [open]);

  return (
    <div
      ref={menuRef}
      className="fixed z-40 flex flex-col items-start"
      style={{
        left: "calc(env(safe-area-inset-left) + 0.5rem)",
        top: "calc(env(safe-area-inset-top) + var(--game-top-overlay-offset, 0px) + 0.75rem)",
      }}
    >
      <div className="flex h-9 w-9 items-center justify-center rounded-full border border-cyan-200/45 bg-slate-950/84 shadow-[0_8px_22px_rgba(0,0,0,0.32),0_0_14px_rgba(34,211,238,0.22)] backdrop-blur-md">
        <button
          onClick={() => {
            setOpen(!open);
          }}
          className={`flex h-7 w-7 items-center justify-center rounded-full border transition-colors ${
            open
              ? "border-cyan-200/80 bg-cyan-300/18 text-cyan-50"
              : "border-white/15 bg-white/7 text-gray-100 hover:border-cyan-200/70 hover:bg-cyan-300/14"
          }`}
          aria-label={t("gameMenu.menu")}
          title={t("gameMenu.menu")}
          aria-haspopup="true"
          aria-expanded={open}
        >
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 20 20"
            fill="currentColor"
            className="h-4 w-4"
          >
            <path
              fillRule="evenodd"
              d="M2 4.75A.75.75 0 0 1 2.75 4h14.5a.75.75 0 0 1 0 1.5H2.75A.75.75 0 0 1 2 4.75ZM2 10a.75.75 0 0 1 .75-.75h14.5a.75.75 0 0 1 0 1.5H2.75A.75.75 0 0 1 2 10Zm0 5.25a.75.75 0 0 1 .75-.75h14.5a.75.75 0 0 1 0 1.5H2.75a.75.75 0 0 1-.75-.75Z"
              clipRule="evenodd"
            />
          </svg>
        </button>
      </div>
      {open && (
        <div
          aria-label={t("gameMenu.menu")}
          className="game-menu-scroll thin-scrollbar absolute left-0 top-full mt-1 w-72 max-w-[calc(100vw-1rem)] touch-pan-y overflow-y-auto overscroll-contain rounded-lg border border-gray-700 bg-gray-900/95 py-1 shadow-xl backdrop-blur-sm"
        >
          <div className="mb-1 flex items-center gap-1 border-b border-gray-700/80 px-2 pb-1">
            <VolumeControl variant="game" />
            <FullscreenButton variant="game" />
            {isOnlineMode && <ConnectionDot />}
          </div>
          <MenuSectionLabel label={t("gameMenu.sections.view")} />
          {multiplayerBoardLayout && onToggleMultiplayerBoardLayout && (
            <MenuButton
              label={boardLayoutToggleTitle}
              active={multiplayerBoardLayout === "split"}
              status={boardLayoutToggleLabel}
              onClick={() => {
                onToggleMultiplayerBoardLayout();
                setOpen(false);
              }}
            />
          )}
          <div className="my-1 border-t border-gray-700/70" />
          <MenuSectionLabel label={t("gameMenu.sections.tools")} />
          {showReportCard && onReportCardClick && (
            <MenuButton
              label={t("gameMenu.reportCard")}
              icon={<FlagIcon />}
              onClick={() => {
                onReportCardClick();
                setOpen(false);
              }}
            />
          )}
          {showSandboxTools && onSandboxToolsClick && (
            <MenuButton
              label={t("gameMenu.openSandboxTools")}
              onClick={() => {
                onSandboxToolsClick();
                setOpen(false);
              }}
            />
          )}
          {onToggleDebugClickModeButtonVisible && (
            <MenuButton
              label={t("gameMenu.clickModeButton")}
              active={debugClickModeButtonVisible}
              pressed={debugClickModeButtonVisible}
              status={
                debugClickModeButtonVisible ? t("gameMenu.shown") : t("gameMenu.hidden")
              }
              onClick={() => {
                onToggleDebugClickModeButtonVisible();
                setOpen(false);
              }}
            />
          )}
          <div className="my-1 border-t border-gray-700/70" />
          <MenuSectionLabel label={t("gameMenu.sections.game")} />
          <MenuButton label={t("gameMenu.resume")} onClick={() => setOpen(false)} />
          <MenuButton
            label={t("gameMenu.settings")}
            onClick={() => {
              setOpen(false);
              onSettingsClick();
            }}
          />
          <MenuButton
            label={t("gameMenu.helpShortcuts")}
            shortcut="?"
            onClick={() => {
              setOpen(false);
              onHelpClick();
            }}
          />
          {isAiMode && (
            <MenuButton
              label={showAiHand ? t("gameMenu.hideAiHand") : t("gameMenu.showAiHand")}
              onClick={() => {
                onToggleAiHand();
                setOpen(false);
              }}
            />
          )}
          {isOnlineMode && onRequestTakeback && (
            <MenuButton
              label={t("gameMenu.requestTakeback")}
              onClick={() => {
                setOpen(false);
                onRequestTakeback();
              }}
            />
          )}
          <div className="my-1 border-t border-gray-700" />
          <MenuButton
            label={t("gameMenu.concede")}
            variant="danger"
            onClick={() => {
              setOpen(false);
              // Online concedes route through the confirmation dialog
              // (`onConcede` opens it). All other modes go straight through
              // the unified concede hook, which dispatches `Concede` to the
              // engine before clearing local state — see useConcedeHandler.
              if (isOnlineMode && onConcede) {
                onConcede();
                return;
              }
              handleConcede();
            }}
          />
          <MenuButton
            label={isDraft || isDraftPodMatch ? t("gameMenu.backToDraft") : t("gameMenu.mainMenu")}
            onClick={() => {
              setOpen(false);
              if (isDraft) {
                useDraftStore.getState().recordMatchResult(gameId, "loss").then(() => {
                  clearGame(gameId);
                  navigate("/draft/quick?resume=1");
                });
              } else if (isDraftPodMatch) {
                navigate("/draft-pod");
              } else {
                navigate("/");
              }
            }}
          />
          <div className="my-1 border-t border-gray-700" />
          <div className="flex flex-wrap items-center gap-x-1.5 gap-y-0.5 px-3 py-1.5 text-[10px] text-slate-500">
            <a
              href={`${__GIT_REPO_URL__}/commit/${__BUILD_HASH__}`}
              target="_blank"
              rel="noopener noreferrer"
              className="transition-colors hover:text-white"
            >
              v{__APP_VERSION__} {__BUILD_HASH__}
            </a>
            {cardDataMeta && (
              <>
                <span className="text-slate-700">·</span>
                <a
                  href={`${__GIT_REPO_URL__}/commit/${cardDataMeta.commit}`}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="transition-colors hover:text-white"
                  title={t("gameMenu.cardDataTitle", { date: cardDataMeta.generated_at })}
                >
                  {t("gameMenu.cards", { commit: cardDataMeta.commit_short })}
                </a>
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function MenuSectionLabel({ label }: { label: string }) {
  return (
    <div className="px-3 pb-1 pt-1.5 text-[10px] font-black uppercase tracking-[0.18em] text-slate-500">
      {label}
    </div>
  );
}

function MenuButton({
  label,
  onClick,
  variant,
  shortcut,
  active,
  pressed,
  status,
  icon,
}: {
  label: string;
  onClick: () => void;
  variant?: "danger";
  shortcut?: string;
  active?: boolean;
  pressed?: boolean;
  status?: string;
  icon?: ReactNode;
}) {
  return (
    <button
      aria-pressed={pressed}
      onClick={onClick}
      className={`flex w-full items-center justify-between gap-3 px-3 py-2 text-left text-sm leading-tight transition-colors ${
        variant === "danger"
          ? "text-red-400 hover:bg-red-900/30 hover:text-red-300"
          : "text-gray-300 hover:bg-gray-800 hover:text-white"
      }`}
    >
      <span className="flex min-w-0 items-center gap-2">
        {icon}
        {active != null && (
          <span
            aria-hidden
            className={`h-2 w-2 shrink-0 rounded-full ${active ? "bg-amber-300" : "bg-gray-700"}`}
          />
        )}
        <span className="min-w-0 whitespace-normal">{label}</span>
      </span>
      {(status || shortcut) && (
        <span className="shrink-0 font-mono text-xs text-gray-500">{status ?? shortcut}</span>
      )}
    </button>
  );
}

function FlagIcon() {
  return (
    <svg
      aria-hidden
      className="h-4 w-4 shrink-0 text-red-400"
      fill="none"
      stroke="currentColor"
      strokeLinecap="round"
      strokeLinejoin="round"
      strokeWidth={1.7}
      viewBox="0 0 20 20"
    >
      <path d="M5 3v14" />
      <path d="M5 4h9l-1.5 3L14 10H5" />
    </svg>
  );
}
