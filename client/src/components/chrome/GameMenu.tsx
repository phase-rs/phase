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

interface GameMenuProps {
  gameId: string;
  isAiMode: boolean;
  isOnlineMode: boolean;
  showAiHand: boolean;
  onToggleAiHand: () => void;
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
}

export function GameMenu({
  gameId,
  isAiMode,
  isOnlineMode,
  showAiHand,
  onToggleAiHand,
  onSettingsClick,
  onHelpClick,
  onConcede,
  onRequestTakeback,
  showSandboxTools,
  onSandboxToolsClick,
}: GameMenuProps) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const [open, setOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const cardDataMeta = useCardDataMeta();
  const isDraft = searchParams.get("source") === "draft" && !!searchParams.get("draftId");
  const isDraftPodMatch = searchParams.get("mode") === "draft-match";

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
      className="fixed z-40"
      style={{
        left: "calc(env(safe-area-inset-left) + 0.5rem)",
        top: "calc(env(safe-area-inset-top) + var(--game-top-overlay-offset, 0px) + 0.5rem)",
      }}
    >
      <div className="flex items-center gap-2">
        <button
          onClick={() => setOpen(!open)}
          className="flex h-9 w-9 items-center justify-center rounded-lg bg-gray-800/80 text-gray-400 transition-colors hover:bg-gray-700/80 hover:text-gray-200"
          aria-label={t("gameMenu.menu")}
        >
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 20 20"
            fill="currentColor"
            className="h-5 w-5"
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
            onClick={onSandboxToolsClick}
            className="flex h-9 w-9 items-center justify-center rounded-lg bg-gray-800/80 text-amber-300/90 transition-colors hover:bg-gray-700/80 hover:text-amber-200"
            aria-label={t("gameMenu.sandboxTools")}
            title={t("gameMenu.sandboxToolsTitle")}
          >
            <svg
              xmlns="http://www.w3.org/2000/svg"
              viewBox="0 0 20 20"
              fill="none"
              stroke="currentColor"
              strokeWidth={1.5}
              strokeLinecap="round"
              strokeLinejoin="round"
              className="h-5 w-5"
            >
              <path d="M8 2.5v4.2L4 14.2a1.6 1.6 0 0 0 1.45 2.3h9.1A1.6 1.6 0 0 0 16 14.2L12 6.7V2.5" />
              <path d="M7 2.5h6" />
              <path d="M6.3 11.5h7.4" />
            </svg>
          </button>
        )}
        {isOnlineMode && <ConnectionDot />}
      </div>
      {open && (
        <div className="absolute left-0 top-full mt-1 w-52 rounded-lg border border-gray-700 bg-gray-900/95 py-1 shadow-xl backdrop-blur-sm">
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

function MenuButton({
  label,
  onClick,
  variant,
  shortcut,
}: {
  label: string;
  onClick: () => void;
  variant?: "danger";
  shortcut?: string;
}) {
  return (
    <button
      onClick={onClick}
      className={`flex w-full items-center justify-between gap-3 px-3 py-2 text-left text-sm transition-colors ${
        variant === "danger"
          ? "text-red-400 hover:bg-red-900/30 hover:text-red-300"
          : "text-gray-300 hover:bg-gray-800 hover:text-white"
      }`}
    >
      <span>{label}</span>
      {shortcut && <span className="font-mono text-xs text-gray-500">{shortcut}</span>}
    </button>
  );
}
