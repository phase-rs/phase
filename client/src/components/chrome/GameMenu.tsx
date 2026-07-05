import { useEffect, useRef, useState } from "react";
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
  debugInteractionMode?: boolean;
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
  debugInteractionMode = false,
  debugClickModeButtonVisible = false,
  onToggleDebugClickModeButtonVisible,
  showReportCard,
  onReportCardClick,
}: GameMenuProps) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const [open, setOpen] = useState(false);
  const [sandboxOpen, setSandboxOpen] = useState(false);
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
    if (!open && !sandboxOpen) return;
    function handleClick(e: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setOpen(false);
        setSandboxOpen(false);
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [open, sandboxOpen]);

  return (
    <div
      ref={menuRef}
      className="fixed z-40 flex flex-col items-start gap-0.5"
      style={{
        left: "calc(env(safe-area-inset-left) + 0.5rem)",
        top: "calc(env(safe-area-inset-top) + var(--game-top-overlay-offset, 0px) + 0.25rem)",
      }}
    >
      <div className="flex h-7 items-center gap-1 rounded-sm border border-white/8 bg-slate-950/64 px-0.5 shadow-[0_8px_18px_rgba(0,0,0,0.24)] backdrop-blur-md">
        <button
          onClick={() => {
            setOpen(!open);
            setSandboxOpen(false);
          }}
          className="flex h-7 w-7 items-center justify-center rounded-md bg-white/6 text-gray-400 transition-colors hover:bg-white/10 hover:text-gray-200"
          aria-label={t("gameMenu.menu")}
          title={t("gameMenu.menu")}
          aria-haspopup="menu"
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
        <VolumeControl variant="game" />
        <FullscreenButton variant="game" />
        {showSandboxTools && onSandboxToolsClick && (
          <button
            onClick={() => {
              setSandboxOpen(!sandboxOpen);
              setOpen(false);
            }}
            className={`flex h-7 w-9 items-center justify-center gap-0.5 rounded-md transition-colors ${
              debugInteractionMode
                ? "bg-amber-400/20 text-amber-200 ring-1 ring-amber-300/40 hover:bg-amber-400/25"
                : "bg-white/6 text-amber-300/90 hover:bg-white/10 hover:text-amber-200"
            }`}
            aria-label={t("gameMenu.sandboxTools")}
            title={t("gameMenu.sandboxToolsTitle")}
            aria-haspopup="menu"
            aria-expanded={sandboxOpen}
          >
            <svg
              xmlns="http://www.w3.org/2000/svg"
              viewBox="0 0 20 20"
              fill="none"
              stroke="currentColor"
              strokeWidth={1.5}
              strokeLinecap="round"
              strokeLinejoin="round"
              className="h-4 w-4"
            >
              <path d="M8 2.5v4.2L4 14.2a1.6 1.6 0 0 0 1.45 2.3h9.1A1.6 1.6 0 0 0 16 14.2L12 6.7V2.5" />
              <path d="M7 2.5h6" />
              <path d="M6.3 11.5h7.4" />
            </svg>
            <svg
              viewBox="0 0 12 12"
              fill="currentColor"
              className={`h-2.5 w-2.5 transition-transform ${sandboxOpen ? "rotate-180" : ""}`}
              aria-hidden
            >
              <path d="M3 4.5h6L6 8z" />
            </svg>
          </button>
        )}
        {showReportCard && onReportCardClick && (
          <button
            onClick={onReportCardClick}
            className="flex h-7 w-7 items-center justify-center rounded-md bg-red-500/12 text-red-400 transition-colors hover:bg-red-500/20 hover:text-red-300"
            aria-label={t("gameMenu.reportCard")}
            title={t("gameMenu.reportCard")}
          >
            <svg
              xmlns="http://www.w3.org/2000/svg"
              viewBox="0 0 20 20"
              fill="none"
              stroke="currentColor"
              strokeWidth={1.5}
              strokeLinecap="round"
              strokeLinejoin="round"
              className="h-4 w-4"
            >
              <path d="M4 2.5v15" />
              <path d="M4 3.5h9.5l-1.6 3 1.6 3H4" />
            </svg>
          </button>
        )}
        {isOnlineMode && <ConnectionDot />}
      </div>
      {multiplayerBoardLayout && onToggleMultiplayerBoardLayout && (
        <button
          type="button"
          onClick={onToggleMultiplayerBoardLayout}
          className={`flex h-5 w-full items-center justify-center gap-1 rounded-sm border border-white/8 px-1.5 text-[9px] font-black uppercase tracking-[0.14em] shadow-[0_8px_18px_rgba(0,0,0,0.2)] backdrop-blur-md transition-colors ${
            multiplayerBoardLayout === "split"
              ? "bg-cyan-400/15 text-cyan-100 ring-1 ring-cyan-300/35 hover:bg-cyan-400/22"
              : "bg-slate-950/64 text-gray-300 hover:bg-white/10 hover:text-gray-100"
          }`}
          aria-label={boardLayoutToggleTitle}
          title={boardLayoutToggleTitle}
        >
          <BoardLayoutIcon layout={multiplayerBoardLayout} />
          <span>{boardLayoutToggleLabel}</span>
        </button>
      )}
      {sandboxOpen && showSandboxTools && onSandboxToolsClick && (
        <div
          role="menu"
          className="absolute left-0 top-full mt-1 w-60 rounded-lg border border-gray-700 bg-gray-900/95 py-1 shadow-xl backdrop-blur-sm"
        >
          <MenuButton
            label={t("gameMenu.openSandboxTools")}
            onClick={() => {
              onSandboxToolsClick();
              setSandboxOpen(false);
            }}
          />
          {onToggleDebugClickModeButtonVisible && (
            <MenuButton
              label={t("gameMenu.clickModeButton")}
              checked={debugClickModeButtonVisible}
              status={
                debugClickModeButtonVisible ? t("gameMenu.shown") : t("gameMenu.hidden")
              }
              onClick={() => {
                onToggleDebugClickModeButtonVisible();
                setSandboxOpen(false);
              }}
            />
          )}
        </div>
      )}
      {open && (
        <div
          role="menu"
          className="absolute left-0 top-full mt-1 w-52 rounded-lg border border-gray-700 bg-gray-900/95 py-1 shadow-xl backdrop-blur-sm"
        >
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

function BoardLayoutIcon({ layout }: { layout: MultiplayerBoardLayout }) {
  if (layout === "split") {
    return (
      <svg viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth={1.5} className="h-3.5 w-3.5" aria-hidden>
        <path d="M3 4.5h14v4H3z" />
        <path d="M3 11.5h14v4H3z" />
        <path d="M7.65 4.5v4M12.35 4.5v4" />
      </svg>
    );
  }

  return (
    <svg viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth={1.5} className="h-3.5 w-3.5" aria-hidden>
      <path d="M3 4.5h14v11H3z" />
      <path d="M6 7h8M6 10h5" />
    </svg>
  );
}

function MenuButton({
  label,
  onClick,
  variant,
  shortcut,
  checked,
  status,
}: {
  label: string;
  onClick: () => void;
  variant?: "danger";
  shortcut?: string;
  checked?: boolean;
  status?: string;
}) {
  return (
    <button
      role={checked == null ? "menuitem" : "menuitemcheckbox"}
      aria-checked={checked}
      onClick={onClick}
      className={`flex w-full items-center justify-between gap-3 px-3 py-2 text-left text-sm transition-colors ${
        variant === "danger"
          ? "text-red-400 hover:bg-red-900/30 hover:text-red-300"
          : "text-gray-300 hover:bg-gray-800 hover:text-white"
      }`}
    >
      <span className="flex min-w-0 items-center gap-2">
        {checked != null && (
          <span
            aria-hidden
            className={`h-2 w-2 rounded-full ${checked ? "bg-amber-300" : "bg-gray-700"}`}
          />
        )}
        <span className="truncate">{label}</span>
      </span>
      {(status || shortcut) && (
        <span className="shrink-0 font-mono text-xs text-gray-500">{status ?? shortcut}</span>
      )}
    </button>
  );
}
