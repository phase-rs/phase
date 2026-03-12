import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";

import type { FormatConfig, GameFormat, MatchType } from "../adapter/types";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { HostSetup } from "../components/lobby/HostSetup";
import { LobbyView } from "../components/lobby/LobbyView";
import { WaitingScreen } from "../components/lobby/WaitingScreen";
import { FormatPicker } from "../components/menu/FormatPicker";
import { GamePresets } from "../components/menu/GamePresets";
import { MenuParticles } from "../components/menu/MenuParticles";
import { MyDecks } from "../components/menu/MyDecks";
import { menuButtonClass } from "../components/menu/buttonStyles";
import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX, listSavedDeckNames } from "../constants/storage";
import { STARTER_DECKS } from "../data/starterDecks";
import { parseRoomCode } from "../network/connection";
import type { ParsedDeck } from "../services/deckParser";
import type { GamePreset } from "../services/presets";
import { savePreset } from "../services/presets";
import { FORMAT_DEFAULTS, useMultiplayerStore } from "../stores/multiplayerStore";
import { saveActiveGame, useGameStore } from "../stores/gameStore";
import type { HostSettings } from "../components/lobby/HostSetup";

function seedStarterDecks(): void {
  for (const starter of STARTER_DECKS) {
    const deck: ParsedDeck = { main: starter.cards, sideboard: [] };
    localStorage.setItem(STORAGE_KEY_PREFIX + starter.name, JSON.stringify(deck));
  }
}

function loadActiveDeck(): ParsedDeck | null {
  const activeName = localStorage.getItem(ACTIVE_DECK_KEY);
  if (!activeName) return null;
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + activeName);
  if (!raw) return null;
  return JSON.parse(raw) as ParsedDeck;
}

type SetupStep =
  | "format"
  | "config"
  | "deck-select"
  | "mode"
  | "lobby"
  | "host-setup"
  | "waiting";

const STEP_BACK: Record<SetupStep, SetupStep | "exit"> = {
  format: "exit",
  config: "format",
  "deck-select": "config",
  mode: "deck-select",
  lobby: "mode",
  "host-setup": "lobby",
  waiting: "lobby",
};

const DIFFICULTIES = [
  { id: "VeryEasy", label: "Very Easy" },
  { id: "Easy", label: "Easy" },
  { id: "Medium", label: "Medium" },
  { id: "Hard", label: "Hard" },
  { id: "VeryHard", label: "Very Hard" },
] as const;

export function GameSetupPage() {
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();

  const [step, setStep] = useState<SetupStep>("format");
  const [selectedFormat, setSelectedFormat] = useState<GameFormat | null>(null);
  const [formatConfig, setFormatConfig] = useState<FormatConfig | null>(null);
  const [playerCount, setPlayerCount] = useState(2);
  const [activeDeckName, setActiveDeckName] = useState<string | null>(null);
  const [difficulty, setDifficulty] = useState("Medium");
  const [matchType, setMatchType] = useState<MatchType>("Bo1");

  // Multiplayer state
  const [hostGameCode, setHostGameCode] = useState<string | null>(null);
  const [hostIsPublic, setHostIsPublic] = useState(true);
  const [connectionMode, setConnectionMode] = useState<"server" | "p2p">("server");
  const hostWsRef = useRef<WebSocket | null>(null);
  const serverAddress = useMultiplayerStore((s) => s.serverAddress);
  const setFormatConfigStore = useMultiplayerStore((s) => s.setFormatConfig);

  useEffect(() => {
    const names = listSavedDeckNames();
    if (names.length === 0) {
      seedStarterDecks();
    }
    setActiveDeckName(localStorage.getItem(ACTIVE_DECK_KEY));

    // Allow direct format entry via search param
    const fmt = searchParams.get("format") as GameFormat | null;
    if (fmt && FORMAT_DEFAULTS[fmt]) {
      handleFormatSelect(fmt);
    }
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const handleFormatSelect = (format: GameFormat) => {
    const defaults = FORMAT_DEFAULTS[format];
    setSelectedFormat(format);
    setFormatConfig(defaults);
    setPlayerCount(defaults.min_players);
    if (defaults.min_players !== 2) {
      setMatchType("Bo1");
    }
    setStep("config");
  };

  const handleConfigConfirm = () => {
    setStep("deck-select");
  };

  const handleSelectDeck = (name: string) => {
    setActiveDeckName(name);
    localStorage.setItem(ACTIVE_DECK_KEY, name);
  };

  const handleDeckConfirm = () => {
    setStep("mode");
  };

  const handleStartAI = () => {
    if (!activeDeckName || !formatConfig) return;
    const gameId = crypto.randomUUID();
    saveActiveGame({ id: gameId, mode: "ai", difficulty });
    useGameStore.setState({ gameId });
    navigate(
      `/game/${gameId}?mode=ai&difficulty=${difficulty}&format=${formatConfig.format}&players=${playerCount}&match=${matchType.toLowerCase()}`,
    );
  };

  const handleSavePreset = () => {
    if (!selectedFormat || !formatConfig) return;
    const name = prompt("Preset name:");
    if (!name) return;
    savePreset({
      id: crypto.randomUUID(),
      name,
      format: selectedFormat,
      formatConfig,
      deckId: activeDeckName,
      aiDifficulty: difficulty,
      playerCount,
    });
  };

  const handlePresetSelect = (preset: GamePreset) => {
    const defaults = FORMAT_DEFAULTS[preset.format];
    setSelectedFormat(preset.format);
    setFormatConfig({ ...defaults, ...preset.formatConfig });
    setPlayerCount(preset.playerCount);
    setDifficulty(preset.aiDifficulty ?? "Medium");
    if (preset.deckId) {
      setActiveDeckName(preset.deckId);
      localStorage.setItem(ACTIVE_DECK_KEY, preset.deckId);
    }
    setStep("mode");
  };

  const handleHostWithSettings = useCallback(
    (settings: HostSettings) => {
      if (!activeDeckName) {
        setStep("deck-select");
        return;
      }
      sessionStorage.removeItem("phase-ws-session");
      setHostIsPublic(settings.public);

      const deck = loadActiveDeck();
      if (!deck) {
        setStep("deck-select");
        return;
      }

      const mainDeck: string[] = [];
      for (const entry of deck.main) {
        for (let i = 0; i < entry.count; i++) {
          mainDeck.push(entry.name);
        }
      }
      const sideboard: string[] = [];
      for (const entry of deck.sideboard) {
        for (let i = 0; i < entry.count; i++) {
          sideboard.push(entry.name);
        }
      }

      const ws = new WebSocket(serverAddress);
      hostWsRef.current = ws;

      ws.onopen = () => {
        ws.send(
          JSON.stringify({
            type: "CreateGameWithSettings",
            data: {
              deck: { main_deck: mainDeck, sideboard },
              display_name: settings.displayName,
              public: settings.public,
              password: settings.password || null,
              timer_seconds: settings.timerSeconds,
              player_count: settings.formatConfig.max_players,
              match_config: { match_type: settings.matchType },
              format_config: settings.formatConfig,
              ai_seats: settings.aiSeats,
            },
          }),
        );
      };

      ws.onmessage = (event) => {
        const msg = JSON.parse(event.data as string) as { type: string; data?: unknown };

        if (msg.type === "GameCreated") {
          const data = msg.data as { game_code: string; player_token: string };
          setHostGameCode(data.game_code);
          sessionStorage.setItem(
            "phase-ws-session",
            JSON.stringify({ gameCode: data.game_code, playerToken: data.player_token }),
          );
          setStep("waiting");
        } else if (msg.type === "GameStarted") {
          ws.close();
          hostWsRef.current = null;
          const gameId = crypto.randomUUID();
          saveActiveGame({ id: gameId, mode: "online", difficulty: "" });
          useGameStore.setState({ gameId });
          navigate(`/game/${gameId}?mode=host`);
        } else if (msg.type === "Error") {
          const data = msg.data as { message: string };
          console.error("Host error:", data.message);
        }
      };

      ws.onerror = () => {
        console.error("Failed to connect to server");
      };
    },
    [activeDeckName, serverAddress, navigate],
  );

  const handleHostP2P = useCallback((settings: HostSettings) => {
    if (!activeDeckName) {
      setStep("deck-select");
      return;
    }
    const gameId = crypto.randomUUID();
    useGameStore.setState({ gameId });
    navigate(`/game/${gameId}?mode=p2p-host&match=${settings.matchType.toLowerCase()}`);
  }, [activeDeckName, navigate]);

  const handleJoinWithPassword = useCallback(
    (code: string, password?: string) => {
      if (!activeDeckName) {
        setStep("deck-select");
        return;
      }

      const p2pCode = parseRoomCode(code);
      if (p2pCode && code.trim().length === 5) {
        const gameId = crypto.randomUUID();
        useGameStore.setState({ gameId });
        navigate(`/game/${gameId}?mode=p2p-join&code=${p2pCode}`);
        return;
      }

      sessionStorage.removeItem("phase-ws-session");
      const gameId = crypto.randomUUID();
      saveActiveGame({ id: gameId, mode: "online", difficulty: "" });
      useGameStore.setState({ gameId });
      const params = new URLSearchParams({ mode: "join", code });
      if (password) {
        params.set("password", password);
      }
      navigate(`/game/${gameId}?${params.toString()}`);
    },
    [activeDeckName, navigate],
  );

  const handleCancelHost = useCallback(() => {
    if (hostWsRef.current) {
      hostWsRef.current.close();
      hostWsRef.current = null;
    }
    setHostGameCode(null);
    sessionStorage.removeItem("phase-ws-session");
    setStep("lobby");
  }, []);

  const handleBack = () => {
    if (step === "waiting") {
      handleCancelHost();
      return;
    }
    const target = STEP_BACK[step];
    if (target === "exit") {
      navigate("/");
    } else {
      setStep(target);
    }
  };

  const needsServer = playerCount > 2;

  return (
    <div className="relative flex min-h-screen flex-col items-center justify-center">
      <MenuParticles />
      <ScreenChrome onBack={handleBack} />

      <div className="relative z-10 flex w-full justify-center py-8">
        {step === "format" && (
          <div className="flex flex-col items-center gap-8">
            <FormatPicker onFormatSelect={handleFormatSelect} />
            <div className="w-full max-w-2xl border-t border-gray-800 pt-6">
              <GamePresets onSelectPreset={handlePresetSelect} />
            </div>
          </div>
        )}

        {step === "config" && formatConfig && (
          <div className="flex w-full max-w-md flex-col items-center gap-6 px-4">
            <h2 className="text-2xl font-bold tracking-tight">
              {selectedFormat === "TwoHeadedGiant" ? "Two-Headed Giant" : selectedFormat} Settings
            </h2>

            <label className="flex w-full flex-col gap-1">
              <span className="text-sm text-gray-400">Starting Life</span>
              <input
                type="number"
                value={formatConfig.starting_life}
                onChange={(e) =>
                  setFormatConfig({ ...formatConfig, starting_life: Number(e.target.value) })
                }
                className="rounded-lg border border-gray-700 bg-gray-800/60 px-3 py-2 text-white"
              />
            </label>

            {!formatConfig.team_based && formatConfig.max_players > 2 && (
              <label className="flex w-full flex-col gap-1">
                <span className="text-sm text-gray-400">
                  Players ({formatConfig.min_players}-{formatConfig.max_players})
                </span>
                <input
                  type="range"
                  min={formatConfig.min_players}
                  max={formatConfig.max_players}
                  value={playerCount}
                  onChange={(e) => {
                    const nextCount = Number(e.target.value);
                    setPlayerCount(nextCount);
                    if (nextCount !== 2) {
                      setMatchType("Bo1");
                    }
                  }}
                  className="w-full"
                />
                <span className="text-center text-lg font-semibold">{playerCount}</span>
              </label>
            )}

            <label className="flex w-full flex-col gap-2">
              <span className="text-sm text-gray-400">Match Type</span>
              <div className="flex overflow-hidden rounded-lg border border-gray-700">
                <button
                  type="button"
                  onClick={() => setMatchType("Bo1")}
                  className={`flex-1 px-3 py-2 text-sm font-medium transition-colors ${
                    matchType === "Bo1"
                      ? "bg-indigo-600 text-white"
                      : "bg-gray-800 text-gray-400 hover:bg-gray-700 hover:text-gray-200"
                  }`}
                >
                  BO1
                </button>
                <button
                  type="button"
                  onClick={() => setMatchType("Bo3")}
                  disabled={playerCount !== 2}
                  className={`flex-1 px-3 py-2 text-sm font-medium transition-colors ${
                    matchType === "Bo3"
                      ? "bg-indigo-600 text-white"
                      : "bg-gray-800 text-gray-400 hover:bg-gray-700 hover:text-gray-200"
                  } ${playerCount !== 2 ? "cursor-not-allowed opacity-40" : ""}`}
                >
                  BO3
                </button>
              </div>
              {playerCount !== 2 && (
                <span className="text-xs text-gray-500">BO3 is available only for 2-player matches.</span>
              )}
            </label>

            {formatConfig.command_zone && (
              <div className="w-full rounded-lg border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-sm text-amber-200">
                Commander rules: 100-card singleton, commander damage at {formatConfig.commander_damage_threshold}
              </div>
            )}

            <button
              onClick={handleConfigConfirm}
              className={menuButtonClass({ tone: "indigo", size: "md" })}
            >
              Choose Deck
            </button>
          </div>
        )}

        {step === "deck-select" && (
          <MyDecks
            mode="select"
            onSelectDeck={handleSelectDeck}
            activeDeckName={activeDeckName}
            onConfirmSelection={handleDeckConfirm}
            confirmLabel="Continue"
            selectedFormat={selectedFormat ?? undefined}
            selectedMatchType={matchType}
            showDifficultySelector
            difficulty={difficulty}
            onDifficultyChange={setDifficulty}
          />
        )}

        {step === "mode" && (
          <div className="flex w-full max-w-md flex-col items-center gap-6 px-4">
            <h2 className="text-2xl font-bold tracking-tight">Game Mode</h2>

            <div className="flex w-full flex-col gap-3">
              <div className="flex w-full flex-col gap-2">
                <h3 className="text-sm font-medium text-gray-400">AI Difficulty</h3>
                <div className="flex overflow-hidden rounded-lg border border-gray-700">
                  {DIFFICULTIES.map((d) => (
                    <button
                      key={d.id}
                      onClick={() => setDifficulty(d.id)}
                      className={`flex-1 px-3 py-1.5 text-xs font-medium transition-colors ${
                        difficulty === d.id
                          ? "bg-indigo-600 text-white"
                          : "bg-gray-800 text-gray-400 hover:bg-gray-700 hover:text-gray-200"
                      }`}
                    >
                      {d.label}
                    </button>
                  ))}
                </div>
              </div>

              <button
                onClick={handleStartAI}
                className={menuButtonClass({ tone: "indigo", size: "md" })}
              >
                Play vs AI ({playerCount > 2 ? `${playerCount - 1} opponents` : "1 opponent"})
              </button>

              <button
                onClick={() => {
                  if (formatConfig) setFormatConfigStore(formatConfig);
                  setConnectionMode("server");
                  setStep("lobby");
                }}
                className={menuButtonClass({ tone: "emerald", size: "md" })}
              >
                Play Online
              </button>

              {!needsServer && (
                <button
                  onClick={() => {
                    setConnectionMode("p2p");
                    setStep("lobby");
                  }}
                  className={menuButtonClass({ tone: "cyan", size: "md" })}
                >
                  Play P2P
                </button>
              )}

              {needsServer && (
                <p className="text-center text-xs text-gray-500">
                  P2P not available for 3+ player games
                </p>
              )}

              <button
                onClick={handleSavePreset}
                className="mt-2 text-xs text-gray-500 transition-colors hover:text-gray-300"
              >
                Save as Preset
              </button>
            </div>
          </div>
        )}

        {step === "lobby" && (
          <LobbyView
            onHostGame={() => { setStep("host-setup"); }}
            onHostP2P={() => { setStep("host-setup"); }}
            onJoinGame={handleJoinWithPassword}
            activeDeckName={activeDeckName}
            onChangeDeck={() => setStep("deck-select")}
            connectionMode={connectionMode}
          />
        )}

        {step === "host-setup" && (
          <HostSetup
            onHost={connectionMode === "p2p" ? handleHostP2P : handleHostWithSettings}
            onBack={() => setStep("lobby")}
            connectionMode={connectionMode}
          />
        )}

        {step === "waiting" && hostGameCode && (
          <WaitingScreen
            gameCode={hostGameCode}
            isPublic={hostIsPublic}
            onCancel={handleCancelHost}
          />
        )}
      </div>
    </div>
  );
}
