import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router";

import { useAudioContext } from "../audio/useAudioContext";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { MainMenuActionCard } from "../components/menu/MainMenuActionCard";
import { MenuLogo } from "../components/menu/MenuLogo";
import { MenuParticles } from "../components/menu/MenuParticles";
import {
  ACTIVE_DECK_KEY,
  listSavedDeckNames,
} from "../constants/storage";
import { isTauri } from "../services/sidecar";
import { loadWsSession } from "../services/multiplayerSession";
import { loadP2PSession } from "../services/p2pSession";
import {
  clearActiveGame,
  loadActiveGame,
  loadGame,
  useGameStore,
} from "../stores/gameStore";
import type { ActiveGameMeta } from "../stores/gameStore";

interface FormatCoverageSummary {
  total_cards: number;
  supported_cards: number;
  coverage_pct: number;
}

/** Ordered by popularity/importance. */
const FORMAT_DISPLAY: { key: string; label: string }[] = [
  { key: "standard", label: "Standard" },
  { key: "commander", label: "Commander" },
  { key: "modern", label: "Modern" },
  { key: "pioneer", label: "Pioneer" },
  { key: "legacy", label: "Legacy" },
  { key: "vintage", label: "Vintage" },
  { key: "pauper", label: "Pauper" },
  { key: "historic", label: "Historic" },
];

export function MenuPage() {
  const navigate = useNavigate();
  const [activeGame, setActiveGame] = useState<ActiveGameMeta | null>(null);
  const [, setDeckCount] = useState(0);
  const [, setActiveDeckName] = useState<string | null>(null);
  const [formatCoverage, setFormatCoverage] = useState<[string, FormatCoverageSummary][]>([]);
  useAudioContext("menu");

  useEffect(() => {
    const savedNames = listSavedDeckNames();
    setDeckCount(savedNames.length);
    setActiveDeckName(localStorage.getItem(ACTIVE_DECK_KEY));

    const saved = loadActiveGame();
    if (saved) {
      if (saved.mode === "online") {
        const hasSession = loadWsSession() !== null;
        if (hasSession) {
          setActiveGame(saved);
        } else {
          clearActiveGame();
        }
      } else if (saved.mode === "p2p-join" && saved.p2pRoomCode) {
        loadP2PSession(`phase-${saved.p2pRoomCode}`).then((session) => {
          if (session) {
            setActiveGame(saved);
          } else {
            clearActiveGame();
          }
        });
      } else {
        loadGame(saved.id).then((state) => {
          if (state) {
            setActiveGame(saved);
          } else {
            clearActiveGame();
          }
        });
      }
    }
  }, []);

  useEffect(() => {
    fetch(__COVERAGE_SUMMARY_URL__)
      .then((res) => (res.ok ? res.json() : null))
      .then((data) => {
        if (!data?.coverage_by_format) return;
        const byFormat = data.coverage_by_format as Record<string, FormatCoverageSummary>;
        const entries: [string, FormatCoverageSummary][] = [];
        for (const { key, label } of FORMAT_DISPLAY) {
          const s = byFormat[key];
          if (s && s.total_cards > 0) entries.push([label, s]);
        }
        setFormatCoverage(entries);
      })
      .catch(() => {});
  }, []);

  const handleResumeGame = useCallback(() => {
    if (!activeGame) return;
    useGameStore.setState({ gameId: activeGame.id });
    if (activeGame.mode === "online") {
      navigate(`/game/${activeGame.id}?mode=host`);
    } else if (activeGame.mode === "p2p-host") {
      navigate(`/game/${activeGame.id}?mode=p2p-host`);
    } else if (activeGame.mode === "p2p-join" && activeGame.p2pRoomCode) {
      navigate(`/game/${activeGame.id}?mode=p2p-join&code=${activeGame.p2pRoomCode}`);
    } else {
      navigate(`/game/${activeGame.id}?mode=${activeGame.mode}&difficulty=${activeGame.difficulty}`);
    }
  }, [activeGame, navigate]);

  const hasSavedGame = activeGame !== null;
  const menuActions = useMemo(() => {
    const actions = [];
    if (hasSavedGame) {
      actions.push({
        key: "resume",
        title: "Resume Game",
        description: "Continue the last saved match from its current turn and board state.",
        accent: "ember" as const,
        onClick: handleResumeGame,
        icon: <ResumeIcon />,
      });
    }
    actions.push(
      {
        key: "setup",
        title: hasSavedGame ? "New AI Match" : "Play vs AI",
        description: "Play a solo match against an AI opponent — choose format, deck, archetype, and difficulty.",
        accent: "arcane" as const,
        onClick: () => navigate("/setup"),
        icon: <SigilIcon />,
      },
      {
        key: "online",
        title: "Play Online",
        description: "Host a room, join by code, or reconnect to multiplayer.",
        accent: "jade" as const,
        onClick: () => navigate("/multiplayer"),
        icon: <CrownIcon />,
      },
      {
        key: "decks",
        title: "Decks",
        description: "Open saved decks, switch your active list, and edit builds.",
        accent: "stone" as const,
        onClick: () => navigate("/my-decks"),
        icon: <DeckIcon />,
      },
    );
    return actions;
  }, [hasSavedGame, navigate, handleResumeGame]);

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <MenuParticles />
      <ScreenChrome />
      <div className="menu-scene__vignette" />
      <div className="menu-scene__sigil menu-scene__sigil--left" />
      <div className="menu-scene__sigil menu-scene__sigil--right" />
      <div className="menu-scene__haze" />

      <div className="fixed left-4 top-[calc(env(safe-area-inset-top)+1rem)] z-20 flex items-center gap-2">
        <a
          href="https://discord.gg/dUZwhYHUyk"
          target="_blank"
          rel="noopener noreferrer"
          className="flex items-center gap-1.5 rounded-full border border-white/8 bg-black/20 px-3 py-1.5 text-xs font-medium text-slate-400 backdrop-blur-sm transition-colors hover:border-[#5865F2]/30 hover:text-[#5865F2]"
        >
          <DiscordIcon />
          Discord
        </a>
        <a
          href="https://github.com/phase-rs/phase"
          target="_blank"
          rel="noopener noreferrer"
          className="flex items-center gap-1.5 rounded-full border border-white/8 bg-black/20 px-3 py-1.5 text-xs font-medium text-slate-400 backdrop-blur-sm transition-colors hover:border-white/20 hover:text-white"
        >
          <GitHubIcon />
          GitHub
        </a>
        <a
          href="https://github.com/sponsors/matthewevans"
          target="_blank"
          rel="noopener noreferrer"
          className="flex items-center gap-1.5 rounded-full border border-white/8 bg-black/20 px-3 py-1.5 text-xs font-medium text-slate-400 backdrop-blur-sm transition-colors hover:border-pink-400/40 hover:text-pink-400"
        >
          <SponsorIcon />
          Sponsor
        </a>
      </div>

      <div className="relative z-10 mx-auto flex min-h-screen w-full max-w-7xl flex-col justify-center px-6 py-16 lg:px-10">
        <div className="mx-auto flex w-full max-w-3xl flex-col items-center text-center">
          <div>
            <MenuLogo />
          </div>
          {formatCoverage.length > 0 && (
            <button
              onClick={() => navigate("/coverage")}
              title="Open the coverage dashboard"
              aria-label="Open card coverage dashboard"
              className="group mt-6 flex cursor-pointer flex-col gap-2 rounded-xl border border-sky-400/30 bg-sky-500/[0.06] px-4 py-3 shadow-[0_0_0_1px_rgba(56,189,248,0.08)] transition-colors hover:border-sky-400/60 hover:bg-sky-500/[0.12] hover:shadow-[0_0_0_1px_rgba(56,189,248,0.22)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-sky-400/70"
            >
              <div className="flex items-center justify-between gap-3">
                <span className="flex items-center gap-2 text-[11px] font-semibold uppercase tracking-[0.16em] text-sky-200">
                  <span aria-hidden>&#128269;</span>
                  Card Coverage Dashboard
                </span>
                <span className="text-[11px] font-medium text-sky-300 transition-transform group-hover:translate-x-0.5">
                  View details &rarr;
                </span>
              </div>
              <div className="grid grid-cols-2 gap-x-3 gap-y-1 sm:grid-cols-4">
                {formatCoverage.map(([label, summary]) => (
                  <span key={label} className="flex items-center justify-between gap-2 px-1">
                    <span className="text-[10px] font-semibold uppercase tracking-wider text-slate-500">
                      {label}
                    </span>
                    <span className={`font-mono text-[11px] font-medium ${
                      summary.coverage_pct > 70
                        ? "text-emerald-400"
                        : summary.coverage_pct > 40
                          ? "text-yellow-400"
                          : "text-red-400"
                    }`}>
                      {summary.coverage_pct.toFixed(0)}%
                    </span>
                  </span>
                ))}
              </div>
            </button>
          )}
        </div>

        <div className="mx-auto mt-8 flex w-full max-w-3xl flex-col gap-2.5">
          {menuActions.map((action) => (
            <MainMenuActionCard
              key={action.key}
              title={action.title}
              description={action.description}
              accent={action.accent}
              onClick={action.onClick}
              icon={action.icon}
            />
          ))}
        </div>

        <div className="mx-auto mt-8 max-w-md rounded-lg border border-amber-500/20 bg-amber-950/20 px-4 py-2.5 text-center text-sm text-amber-200/70">
          <span className="font-semibold text-amber-300/90">Early Alpha</span>
          {" — expect broken cards and missing features."}
        </div>

        {hasSavedGame && (
          <div className="mt-3 flex justify-center">
            <div className="rounded-full border border-white/8 bg-black/16 px-4 py-2 text-sm text-slate-500">
              Saved match available
            </div>
          </div>
        )}

        {isTauri() && (
          <div className="mt-6 flex justify-center">
            <button
              onClick={() => {
                import("@tauri-apps/plugin-process").then((m) => m.exit(0));
              }}
              className="rounded-full border border-white/8 bg-black/20 px-5 py-1.5 text-xs font-medium text-slate-500 backdrop-blur-sm transition-colors hover:border-red-500/30 hover:text-red-400"
            >
              Exit
            </button>
          </div>
        )}

        <p className="mt-8 text-center text-[11px] tracking-wide text-slate-600">
          matt evans :: 2026
        </p>
      </div>

    </div>
  );
}

function ResumeIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-7 w-7 fill-current">
      <path d="M12 3a9 9 0 1 0 8.95 10h-2.07A7 7 0 1 1 12 5a6.96 6.96 0 0 1 4.95 2.05L14 10h7V3l-2.64 2.64A8.95 8.95 0 0 0 12 3Z" />
    </svg>
  );
}

function SigilIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-7 w-7 fill-current">
      <path d="M12 2 4 6v6c0 5.2 3.4 9.8 8 11 4.6-1.2 8-5.8 8-11V6l-8-4Zm0 5.2 2 4.05 4.5.65-3.25 3.16.77 4.47L12 17.34 7.98 19.5l.77-4.47L5.5 11.9l4.5-.65L12 7.2Z" />
    </svg>
  );
}

function CrownIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-7 w-7 fill-current">
      <path d="m3 18 1.9-9 4.35 3.76L12 6l2.75 6.76L19.1 9 21 18H3Zm1 2h16v2H4v-2Z" />
    </svg>
  );
}

function GitHubIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-3.5 w-3.5 fill-current">
      <path d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0 1 12 6.844a9.59 9.59 0 0 1 2.504.337c1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.02 10.02 0 0 0 22 12.017C22 6.484 17.522 2 12 2Z" />
    </svg>
  );
}

function SponsorIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-3.5 w-3.5 fill-current">
      <path d="M12 21s-7.5-4.6-10-9.3C.3 8.4 1.9 4.5 5.4 4c2-.3 3.9.6 5.1 2.2l1.5 1.9 1.5-1.9C14.7 4.6 16.6 3.7 18.6 4c3.5.5 5.1 4.4 3.4 7.7C19.5 16.4 12 21 12 21z" />
    </svg>
  );
}

function DiscordIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-3.5 w-3.5 fill-current">
      <path d="M20.317 4.37a19.79 19.79 0 0 0-4.885-1.515.074.074 0 0 0-.079.037c-.21.375-.444.864-.608 1.25a18.27 18.27 0 0 0-5.487 0 12.64 12.64 0 0 0-.617-1.25.077.077 0 0 0-.079-.037A19.74 19.74 0 0 0 3.677 4.37a.07.07 0 0 0-.032.027C.533 9.046-.32 13.58.099 18.057a.082.082 0 0 0 .031.057 19.9 19.9 0 0 0 5.993 3.03.078.078 0 0 0 .084-.028c.462-.63.874-1.295 1.226-1.994a.076.076 0 0 0-.041-.106 13.1 13.1 0 0 1-1.872-.892.077.077 0 0 1-.008-.128c.126-.094.252-.192.372-.291a.074.074 0 0 1 .077-.01c3.928 1.793 8.18 1.793 12.062 0a.074.074 0 0 1 .078.01c.12.098.246.198.373.292a.077.077 0 0 1-.006.127 12.3 12.3 0 0 1-1.873.892.077.077 0 0 0-.041.107c.36.698.772 1.362 1.225 1.993a.076.076 0 0 0 .084.028 19.84 19.84 0 0 0 6.002-3.03.077.077 0 0 0 .032-.054c.5-5.177-.838-9.674-3.549-13.66a.06.06 0 0 0-.031-.03ZM8.02 15.33c-1.183 0-2.157-1.085-2.157-2.419 0-1.333.956-2.419 2.157-2.419 1.21 0 2.176 1.095 2.157 2.42 0 1.333-.956 2.418-2.157 2.418Zm7.975 0c-1.183 0-2.157-1.085-2.157-2.419 0-1.333.955-2.419 2.157-2.419 1.21 0 2.176 1.095 2.157 2.42 0 1.333-.946 2.418-2.157 2.418Z" />
    </svg>
  );
}

function DeckIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-7 w-7 fill-current">
      <path d="M7 3h9a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2Zm1 3v9h7V6H8Zm-2 15h11v-2H6v2Z" />
    </svg>
  );
}
