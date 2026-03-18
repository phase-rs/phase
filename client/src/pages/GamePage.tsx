import {
  type CSSProperties,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useNavigate, useParams, useSearchParams } from "react-router";
import { AnimatePresence, motion } from "framer-motion";

import type { GameFormat, MatchConfig } from "../adapter/types";
import { AnimationOverlay } from "../components/animation/AnimationOverlay.tsx";
import { TurnBanner } from "../components/animation/TurnBanner.tsx";
import { BattlefieldBackground } from "../components/board/BattlefieldBackground.tsx";
import { BlockAssignmentLines } from "../components/board/BlockAssignmentLines.tsx";
import { GameBoard } from "../components/board/GameBoard.tsx";
import { CardImage } from "../components/card/CardImage.tsx";
import { CardPreview } from "../components/card/CardPreview.tsx";
import { ActionButton } from "../components/board/ActionButton.tsx";
import { FullControlToggle } from "../components/controls/FullControlToggle.tsx";
import { CombatPhaseIndicator } from "../components/controls/PhaseStopBar.tsx";
import { OpponentHand } from "../components/hand/OpponentHand.tsx";
import { PlayerHand } from "../components/hand/PlayerHand.tsx";
import { GameLogPanel } from "../components/log/GameLogPanel.tsx";
import { DamageAssignmentModal } from "../components/combat/DamageAssignmentModal.tsx";
import { ManaPaymentUI } from "../components/mana/ManaPaymentUI.tsx";
import { CardDataMissingModal } from "../components/modal/CardDataMissingModal.tsx";
import { AdventureCastModal } from "../components/modal/AdventureCastModal.tsx";
import { CardChoiceModal } from "../components/modal/CardChoiceModal.tsx";
import { ChoiceModal } from "../components/modal/ChoiceModal.tsx";
import { ModeChoiceModal } from "../components/modal/ModeChoiceModal.tsx";
import { ReplacementModal } from "../components/modal/ReplacementModal.tsx";
import { StackDisplay } from "../components/stack/StackDisplay.tsx";
import { TargetingOverlay } from "../components/targeting/TargetingOverlay.tsx";
import { PlayerHud } from "../components/hud/PlayerHud.tsx";
import { OpponentHud } from "../components/hud/OpponentHud.tsx";
import { GraveyardPile } from "../components/zone/GraveyardPile.tsx";
import { LibraryPile } from "../components/zone/LibraryPile.tsx";
import { ZoneIndicator } from "../components/zone/ZoneIndicator.tsx";
import { ZoneViewer } from "../components/zone/ZoneViewer.tsx";
import { PreferencesModal } from "../components/settings/PreferencesModal.tsx";
import { GameMenu } from "../components/chrome/GameMenu.tsx";
import { ConcedeDialog } from "../components/multiplayer/ConcedeDialog.tsx";
import { ConnectionDot } from "../components/multiplayer/ConnectionDot.tsx";
import { ConnectionToast } from "../components/multiplayer/ConnectionToast.tsx";
import { EmoteOverlay } from "../components/multiplayer/EmoteOverlay.tsx";
import type { P2PAdapterEvent } from "../adapter/p2p-adapter.ts";
import { WebSocketAdapter } from "../adapter/ws-adapter.ts";
import type { WsAdapterEvent } from "../adapter/ws-adapter.ts";
import { useGameDispatch } from "../hooks/useGameDispatch.ts";
import { useKeyboardShortcuts } from "../hooks/useKeyboardShortcuts.ts";
import { useGameStore } from "../stores/gameStore.ts";
import { useUiStore } from "../stores/uiStore.ts";
import { usePreferencesStore } from "../stores/preferencesStore.ts";
import {
  FORMAT_DEFAULTS,
  useMultiplayerStore,
} from "../stores/multiplayerStore.ts";
import { GameProvider } from "../providers/GameProvider.tsx";
import { usePlayerId } from "../hooks/usePlayerId.ts";
import { abilityChoiceLabel, additionalCostChoices } from "../viewmodel/costLabel.ts";
import { gameButtonClass } from "../components/ui/buttonStyles.ts";

type ZoneRailStyle = CSSProperties & {
  "--card-w": string;
  "--card-h": string;
};

export function GamePage() {
  const navigate = useNavigate();
  const { id: gameId } = useParams<{ id: string }>();
  const [searchParams] = useSearchParams();
  const rawMode = searchParams.get("mode");
  const difficulty = searchParams.get("difficulty") ?? "Medium";
  const joinCode = searchParams.get("code") ?? "";
  const formatParam = searchParams.get("format") as GameFormat | null;
  const playersParam = searchParams.get("players");
  const matchParam = searchParams.get("match");
  const playerCount = playersParam ? Number(playersParam) : undefined;
  const formatConfig = formatParam ? FORMAT_DEFAULTS[formatParam] : undefined;
  const matchConfig = useMemo<MatchConfig>(
    () => ({
      match_type: matchParam?.toLowerCase() === "bo3" ? "Bo3" : "Bo1",
    }),
    [matchParam],
  );

  // Map URL modes to GameProvider modes
  const mode: "ai" | "online" | "local" | "p2p-host" | "p2p-join" =
    rawMode === "p2p-host"
      ? "p2p-host"
      : rawMode === "p2p-join"
        ? "p2p-join"
        : rawMode === "host" || rawMode === "join"
          ? "online"
          : rawMode === "ai"
            ? "ai"
            : "local";

  const [showCardDataMissing, setShowCardDataMissing] = useState(false);

  // Online multiplayer state
  const [hostGameCode, setHostGameCode] = useState<string | null>(null);
  const [waitingForOpponent, setWaitingForOpponent] = useState(false);
  const [opponentDisconnected, setOpponentDisconnected] = useState(false);
  const [disconnectGrace, setDisconnectGrace] = useState(0);
  const [reconnectState, setReconnectState] = useState<
    | { status: "idle" }
    | { status: "reconnecting"; attempt: number; maxAttempts: number }
    | { status: "failed" }
  >({ status: "idle" });

  // Multiplayer UX state
  const [showConcedeDialog, setShowConcedeDialog] = useState(false);
  const [receivedEmote, setReceivedEmote] = useState<string | null>(null);
  const receivedEmoteTimerRef = useRef<ReturnType<typeof setTimeout> | null>(
    null,
  );
  const [timerRemaining, setTimerRemaining] = useState<Record<number, number>>(
    {},
  );
  const [gameStartedAt, setGameStartedAt] = useState<number | null>(null);

  const handleWsEvent = useCallback((event: WsAdapterEvent) => {
    switch (event.type) {
      case "gameCreated":
        setHostGameCode(event.gameCode);
        break;
      case "waitingForOpponent":
        setWaitingForOpponent(true);
        break;
      case "opponentDisconnected":
        setOpponentDisconnected(true);
        setDisconnectGrace(event.graceSeconds);
        break;
      case "opponentReconnected":
        setOpponentDisconnected(false);
        break;
      case "reconnecting":
        setReconnectState({
          status: "reconnecting",
          attempt: event.attempt,
          maxAttempts: event.maxAttempts,
        });
        break;
      case "reconnected":
        setReconnectState({ status: "idle" });
        break;
      case "reconnectFailed":
        setReconnectState({ status: "failed" });
        break;
      case "stateChanged":
        // Record game start time on first state update
        setGameStartedAt((prev) => prev ?? Date.now());
        break;
      case "conceded":
        // Server will follow up with GameOver; nothing extra needed here
        break;
      case "emoteReceived":
        setReceivedEmote(event.emote);
        if (receivedEmoteTimerRef.current)
          clearTimeout(receivedEmoteTimerRef.current);
        receivedEmoteTimerRef.current = setTimeout(
          () => setReceivedEmote(null),
          3000,
        );
        break;
      case "timerUpdate":
        setTimerRemaining((prev) => ({
          ...prev,
          [event.player]: event.remainingSeconds,
        }));
        break;
    }
  }, []);

  const handleP2PEvent = useCallback((event: P2PAdapterEvent) => {
    switch (event.type) {
      case "roomCreated":
        setHostGameCode(event.roomCode);
        break;
      case "waitingForGuest":
        setWaitingForOpponent(true);
        break;
      case "guestConnected":
        // Guest connected, game init will follow
        break;
      case "opponentDisconnected":
        setOpponentDisconnected(true);
        break;
      case "error":
        setReconnectState({ status: "failed" });
        break;
    }
  }, []);

  const handleReady = useCallback(() => {
    setWaitingForOpponent(false);
  }, []);

  const handleNoDeck = useCallback(() => {
    navigate("/");
  }, [navigate]);

  const handleCardDataMissing = useCallback(() => {
    setShowCardDataMissing(true);
  }, []);

  if (!gameId) return null;

  return (
    <GameProvider
      gameId={gameId}
      mode={mode}
      difficulty={difficulty}
      joinCode={joinCode || undefined}
      formatConfig={formatConfig}
      playerCount={playerCount}
      matchConfig={matchConfig}
      onWsEvent={mode === "online" ? handleWsEvent : undefined}
      onP2PEvent={
        mode === "p2p-host" || mode === "p2p-join" ? handleP2PEvent : undefined
      }
      onReady={
        mode === "online" || mode === "p2p-host" || mode === "p2p-join"
          ? handleReady
          : undefined
      }
      onCardDataMissing={handleCardDataMissing}
      onNoDeck={handleNoDeck}
    >
      <GamePageContent
        gameId={gameId}
        mode={rawMode}
        isOnlineMode={mode === "online"}
        hostGameCode={hostGameCode}
        waitingForOpponent={waitingForOpponent}
        opponentDisconnected={opponentDisconnected}
        disconnectGrace={disconnectGrace}
        reconnectState={reconnectState}
        showCardDataMissing={showCardDataMissing}
        onDismissCardDataMissing={() => setShowCardDataMissing(false)}
        showConcedeDialog={showConcedeDialog}
        onShowConcedeDialog={() => setShowConcedeDialog(true)}
        onHideConcedeDialog={() => setShowConcedeDialog(false)}
        receivedEmote={receivedEmote}
        timerRemaining={timerRemaining}
        gameStartedAt={gameStartedAt}
      />
    </GameProvider>
  );
}

interface GamePageContentProps {
  gameId: string;
  mode: string | null;
  isOnlineMode: boolean;
  hostGameCode: string | null;
  waitingForOpponent: boolean;
  opponentDisconnected: boolean;
  disconnectGrace: number;
  reconnectState:
    | { status: "idle" }
    | { status: "reconnecting"; attempt: number; maxAttempts: number }
    | { status: "failed" };
  showCardDataMissing: boolean;
  onDismissCardDataMissing: () => void;
  showConcedeDialog: boolean;
  onShowConcedeDialog: () => void;
  onHideConcedeDialog: () => void;
  receivedEmote: string | null;
  timerRemaining: Record<number, number>;
  gameStartedAt: number | null;
}

function GamePageContent({
  gameId,
  mode,
  isOnlineMode,
  hostGameCode,
  waitingForOpponent,
  opponentDisconnected,
  disconnectGrace,
  reconnectState,
  showCardDataMissing,
  onDismissCardDataMissing,
  showConcedeDialog,
  onShowConcedeDialog,
  onHideConcedeDialog,
  receivedEmote,
  timerRemaining,
  gameStartedAt,
}: GamePageContentProps) {
  const navigate = useNavigate();
  const containerRef = useRef<HTMLDivElement>(null);

  const gameState = useGameStore((s) => s.gameState);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameDispatch();
  const inspectedObjectId = useUiStore((s) => s.inspectedObjectId);
  const objects = gameState?.objects;
  const [showAiHand, setShowAiHand] = useState(false);
  const [showDebugBounds, setShowDebugBounds] = useState(false);
  const [viewingZone, setViewingZone] = useState<{
    zone: "graveyard" | "exile";
    playerId: number;
  } | null>(null);
  const [showPreferences, setShowPreferences] = useState(false);

  const playerId = usePlayerId();
  const opponentDisplayName = useMultiplayerStore((s) => s.opponentDisplayName);
  const adapter = useGameStore((s) => s.adapter);
  const focusedOpponent = useUiStore((s) => s.focusedOpponent);
  const opponents = useMemo(() => {
    if (!gameState) return [];
    const seatOrder =
      gameState.seat_order ?? gameState.players.map((p) => p.id);
    const eliminated = gameState.eliminated_players ?? [];
    return seatOrder.filter(
      (id) => id !== playerId && !eliminated.includes(id),
    );
  }, [gameState, playerId]);
  const activeOpponentId =
    focusedOpponent ?? opponents[0] ?? (playerId === 0 ? 1 : 0);

  const handleConcede = useCallback(() => {
    if (adapter && adapter instanceof WebSocketAdapter) {
      adapter.sendConcede();
    }
    onHideConcedeDialog();
  }, [adapter, onHideConcedeDialog]);

  const handleSendEmote = useCallback(
    (emote: string) => {
      if (adapter && adapter instanceof WebSocketAdapter) {
        adapter.sendEmote(emote);
      }
    },
    [adapter],
  );

  const isDragging = useUiStore((s) => s.isDragging);
  const inspectedFaceIndex = useUiStore((s) => s.inspectedFaceIndex);
  const inspectedObj =
    !isDragging && inspectedObjectId != null && objects
      ? (objects[inspectedObjectId] ?? null)
      : null;
  const inspectedCardName = inspectedObj
    ? inspectedFaceIndex === 1 && inspectedObj.back_face
      ? inspectedObj.back_face.name
      : inspectedObj.name
    : null;

  useKeyboardShortcuts();

  // Toggle debug layout bounds with Ctrl+Shift+D
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.shiftKey && e.key === "D") {
        e.preventDefault();
        setShowDebugBounds((v) => !v);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  // Sync card size preference to CSS custom properties
  const cardSize = usePreferencesStore((s) => s.cardSize);
  useEffect(() => {
    const root = document.documentElement;
    const scale = cardSize === "small" ? 0.8 : cardSize === "large" ? 1.25 : 1;
    root.style.setProperty("--card-size-scale", String(scale));
  }, [cardSize]);

  // Auto-open graveyard/exile viewer when the engine is waiting for a target in that zone
  useEffect(() => {
    if (!gameState) return;
    const wf = gameState.waiting_for;
    if (
      (wf?.type !== "TargetSelection" && wf?.type !== "TriggerTargetSelection")
      || wf.data.player !== playerId
    ) return;

    const legalTargets = wf.data.selection.current_legal_targets;
    // Find the first legal target whose engine-provided zone is Graveyard or Exile
    for (const t of legalTargets) {
      if (!("Object" in t)) continue;
      const obj = gameState.objects[t.Object];
      if (!obj) continue;
      if (obj.zone === "Graveyard" || obj.zone === "Exile") {
        const zone = obj.zone === "Graveyard" ? "graveyard" : "exile";
        setViewingZone({ zone, playerId: obj.owner });
        return;
      }
    }
  }, [gameState, playerId]);

  const handleMulliganChoice = useCallback(
    (id: string) => {
      dispatch({
        type: "MulliganDecision",
        data: { keep: id === "keep" },
      });
    },
    [dispatch],
  );

  const handleBottomCards = useCallback(
    (id: string) => {
      const cards = id.split(",").map(Number).filter(Boolean);
      dispatch({ type: "SelectCards", data: { cards } });
    },
    [dispatch],
  );

  const handleSubmitSideboard = useCallback(() => {
    if (!gameState?.deck_pools) return;
    const pool = gameState.deck_pools.find(
      (deckPool) => deckPool.player === playerId,
    );
    if (!pool) return;
    const toSortedCounts = (
      entries: Array<{ card: { name: string }; count: number }>,
    ) => {
      const counts = new Map<string, number>();
      for (const entry of entries) {
        counts.set(
          entry.card.name,
          (counts.get(entry.card.name) ?? 0) + entry.count,
        );
      }
      return [...counts.entries()]
        .sort((a, b) => a[0].localeCompare(b[0]))
        .map(([name, count]) => ({ name, count }));
    };
    dispatch({
      type: "SubmitSideboard",
      data: {
        main: toSortedCounts(pool.current_main),
        sideboard: toSortedCounts(pool.current_sideboard),
      },
    });
  }, [dispatch, gameState, playerId]);

  const handleChoosePlayDraw = useCallback(
    (playFirst: boolean) => {
      dispatch({
        type: "ChoosePlayDraw",
        data: { play_first: playFirst },
      });
    },
    [dispatch],
  );

  const isReconnecting = reconnectState.status !== "idle";
  const topOverlayOffsetPx = reconnectState.status === "idle" ? 0 : 56;
  const gamePageStyle = {
    "--game-top-overlay-offset": `${topOverlayOffsetPx}px`,
  } as CSSProperties;
  const playerZoneRailStyle: ZoneRailStyle = {
    "--card-w": "clamp(60px, 7vw, 95px)",
    "--card-h": "clamp(84px, 9.8vw, 133px)",
  };

  return (
    <div
      ref={containerRef}
      className={`relative h-[100dvh] w-full overflow-hidden bg-gray-950${showDebugBounds ? " debug-bounds" : ""}`}
      style={gamePageStyle}
    >
      <BattlefieldBackground />
      <StackDisplay />

      {/* Reconnecting banner */}
      {reconnectState.status === "reconnecting" && (
        <div className="fixed left-0 right-0 top-0 z-40 bg-amber-600 px-4 py-2 text-center text-sm font-semibold text-white">
          Reconnecting... (attempt {reconnectState.attempt}/
          {reconnectState.maxAttempts})
        </div>
      )}

      {/* Connection lost banner */}
      {reconnectState.status === "failed" && (
        <div className="fixed left-0 right-0 top-0 z-40 flex items-center justify-center gap-4 bg-red-700 px-4 py-2 text-sm font-semibold text-white">
          <span>Connection lost</span>
          <button
            onClick={() => navigate("/")}
            className="rounded bg-white/20 px-3 py-1 text-xs font-semibold hover:bg-white/30"
          >
            Return to Menu
          </button>
        </div>
      )}

      {/* Full-screen board layout */}
      <div
        className={`relative z-10 flex h-full flex-col${isReconnecting ? " pointer-events-none" : ""}`}
        style={{ paddingTop: "var(--game-top-overlay-offset, 0px)" }}
      >
        {/* Opponent hand + zones at top */}
        <div className="relative z-20 w-full shrink-0" data-debug-label="Opp Top">
          <OpponentHand showCards={showAiHand} />
          <div
            className="pointer-events-none absolute right-0 top-0 z-10 flex w-fit flex-col items-end gap-1 px-2 py-1 [&>*]:pointer-events-auto [&>div>*]:pointer-events-auto"
            style={playerZoneRailStyle}
            data-debug-label="Opp Zones"
          >
            <div className="flex items-start gap-2">
              <LibraryPile playerId={activeOpponentId} />
              <GraveyardPile
                playerId={activeOpponentId}
                onClick={() =>
                  setViewingZone({ zone: "graveyard", playerId: activeOpponentId })
                }
              />
            </div>
            <ZoneIndicator
              zone="exile"
              playerId={activeOpponentId}
              onClick={() =>
                setViewingZone({ zone: "exile", playerId: activeOpponentId })
              }
            />
          </div>
        </div>

        {/* Opponent avatar centered below their hand */}
        <div className="relative z-20 shrink-0" data-debug-label="Opp HUD">
          <OpponentHud
            opponentName={isOnlineMode ? opponentDisplayName : undefined}
          />
        </div>

        {/* Battlefield */}
        <div className="relative z-10 flex min-h-0 flex-1 flex-col overflow-hidden" data-debug-label="Battlefield">
          <GameBoard />
        </div>

        {/* Player avatar centered with flanking phase indicators */}
        <div data-debug-label="Player HUD">
          <PlayerHud />
        </div>

        {/* Player hand + zones at bottom — negative margin pushes hand content
             below viewport edge so cards peek from the bottom (clipped by page root overflow-hidden).
             Zones are anchored to top-0 so they stay in the visible area. */}
        <div className="relative shrink-0 mb-[calc(var(--card-h)*-0.15)] sm:mb-[calc(var(--card-h)*-0.25)] md:mb-[calc(var(--card-h)*-0.35)]" data-debug-label="Player Bottom">
          <PlayerHand />
          <div
            className="pointer-events-none absolute left-0 top-0 bottom-[calc(var(--card-h)*0.15)] sm:bottom-[calc(var(--card-h)*0.25)] md:bottom-[calc(var(--card-h)*0.35)] z-10 flex w-fit flex-col items-start justify-end gap-1 p-3 [&>*]:pointer-events-auto [&>div>*]:pointer-events-auto"
            style={playerZoneRailStyle}
            data-debug-label="Player Zones"
          >
            <ZoneIndicator
              zone="exile"
              playerId={playerId}
              onClick={() => setViewingZone({ zone: "exile", playerId })}
            />
            <div className="flex items-end gap-2">
              <GraveyardPile
                playerId={playerId}
                onClick={() => setViewingZone({ zone: "graveyard", playerId })}
              />
              <LibraryPile playerId={playerId} />
            </div>
          </div>
        </div>
      </div>

      {/* Opponent zones are now inline in the Opp Top row above */}

      {/* Combat phase indicator — above action buttons to avoid overlap */}
      <div
        className="fixed z-30 bottom-[calc(env(safe-area-inset-bottom)+14rem)] sm:bottom-[calc(env(safe-area-inset-bottom)+11rem)]"
        style={{
          right:
            "calc(env(safe-area-inset-right) + 1rem + var(--game-right-rail-offset, 0px))",
        }}
      >
        <CombatPhaseIndicator />
      </div>

      {/* Game menu — top-left hamburger */}
      <GameMenu
        gameId={gameId}
        isAiMode={mode === "ai"}
        isOnlineMode={isOnlineMode}
        showAiHand={showAiHand}
        onToggleAiHand={() => setShowAiHand((v) => !v)}
        onSettingsClick={() => setShowPreferences(true)}
        onConcede={onShowConcedeDialog}
      />

      {/* Connection status dot — top-right, visible during multiplayer */}
      {isOnlineMode && <ConnectionDot />}

      {/* Connection failure toast */}
      {isOnlineMode && (
        <ConnectionToast
          onRetry={() => window.location.reload()}
          onSettings={() => setShowPreferences(true)}
        />
      )}

      {/* Host game: show game code while waiting */}
      {waitingForOpponent && hostGameCode && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div className="absolute inset-0 bg-black/70" />
          <div className="relative z-10 w-full max-w-md rounded-[24px] border border-white/10 bg-[#0b1020]/96 p-8 text-center shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md">
            <h2 className="mb-2 text-xl font-bold text-white">
              Waiting for Opponent
            </h2>
            <p className="mb-4 text-sm text-gray-400">
              Share this code with your opponent:
            </p>
            <p className="mb-4 font-mono text-4xl font-bold tracking-widest text-emerald-400">
              {hostGameCode}
            </p>
            <p className="text-xs text-gray-500">
              The game will start when your opponent joins.
            </p>
          </div>
        </div>
      )}

      {/* Join game: waiting overlay */}
      {waitingForOpponent && !hostGameCode && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div className="absolute inset-0 bg-black/70" />
          <div className="relative z-10 w-full max-w-sm rounded-[24px] border border-white/10 bg-[#0b1020]/96 p-6 text-center shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md">
            <h2 className="text-lg font-bold text-white">Joining Game...</h2>
            <p className="mt-2 text-sm text-gray-400">Connecting to game</p>
          </div>
        </div>
      )}

      {/* Opponent disconnected overlay */}
      {opponentDisconnected && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div className="absolute inset-0 bg-black/60" />
          <div className="relative z-10 w-full max-w-sm rounded-[24px] border border-yellow-400/30 bg-[#0b1020]/96 p-6 text-center shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md">
            <h2 className="mb-2 text-lg font-bold text-yellow-400">
              Opponent Disconnected
            </h2>
            <p className="text-sm text-gray-300">
              Waiting for opponent to reconnect...
            </p>
            <p className="mt-2 text-xs text-gray-500">
              Game will forfeit in {disconnectGrace}s
            </p>
          </div>
        </div>
      )}

      {/* Card data missing modal */}
      {showCardDataMissing && (
        <CardDataMissingModal onContinue={onDismissCardDataMissing} />
      )}

      {/* Overlay layers */}
      <GameLogPanel />

      {viewingZone && (
        <ZoneViewer
          zone={viewingZone.zone}
          playerId={viewingZone.playerId}
          onClose={() => setViewingZone(null)}
        />
      )}

      {showPreferences && (
        <PreferencesModal onClose={() => setShowPreferences(false)} />
      )}

      {/* Animation overlay (above board, below modals) */}
      <AnimationOverlay containerRef={containerRef} />
      <TurnBanner />

      {/* Block assignment lines (animated SVG overlay for combat) */}
      <BlockAssignmentLines />

      {/* Damage assignment review (read-only for v1.1) */}
      <DamageAssignmentModal />

      {/* Unified action button (combat + priority controls) */}
      <ActionButton />
      <div
        className="fixed z-30"
        style={{
          bottom: "calc(env(safe-area-inset-bottom) + 5rem)",
          right:
            "calc(env(safe-area-inset-right) + 1rem + var(--game-right-rail-offset, 0px))",
        }}
      >
        <FullControlToggle />
      </div>

      {/* Card preview overlay */}
      <CardPreview cardName={inspectedCardName} />

      {/* WaitingFor-driven prompt overlays (only for human player) */}
      {(waitingFor?.type === "TargetSelection" ||
        waitingFor?.type === "TriggerTargetSelection") &&
        waitingFor.data.player === playerId && <TargetingOverlay />}
      {waitingFor?.type === "ManaPayment" &&
        waitingFor.data.player === playerId && <ManaPaymentUI />}
      {waitingFor?.type === "ReplacementChoice" &&
        waitingFor.data.player === playerId && <ReplacementModal />}
      <ModeChoiceModal />
      <AdventureCastModal />

      {/* Scry/Dig/Surveil card choice modal */}
      <CardChoiceModal />

      {/* Ability choice picker (planeswalkers, multi-ability permanents) */}
      <AbilityChoiceModal />

      {/* Optional additional cost choice (kicker, blight, "or pay") */}
      {waitingFor?.type === "OptionalCostChoice" &&
        waitingFor.data.player === playerId && (
          <OptionalCostModal />
        )}

      {/* Optional effect choice ("You may X") */}
      {waitingFor?.type === "OptionalEffectChoice" &&
        waitingFor.data.player === playerId && (
          <OptionalEffectModal />
        )}

      {/* Unless payment choice ("Counter unless you pay {X}") */}
      {waitingFor?.type === "UnlessPayment" &&
        waitingFor.data.player === playerId && (
          <UnlessPaymentModal />
        )}

      {waitingFor?.type === "MulliganDecision" &&
        waitingFor.data.player === playerId && (
          <MulliganDecisionPrompt
            playerId={waitingFor.data.player}
            mulliganCount={waitingFor.data.mulligan_count}
            onChoose={handleMulliganChoice}
          />
        )}

      {waitingFor?.type === "MulliganBottomCards" &&
        waitingFor.data.player === playerId && (
          <MulliganBottomCardsPrompt
            playerId={waitingFor.data.player}
            count={waitingFor.data.count}
            onChoose={handleBottomCards}
          />
        )}

      {waitingFor?.type === "BetweenGamesSideboard" &&
        waitingFor.data.player === playerId && (
          <BetweenGamesSideboardPrompt
            gameNumber={waitingFor.data.game_number}
            score={waitingFor.data.score}
            onSubmit={handleSubmitSideboard}
          />
        )}

      {waitingFor?.type === "BetweenGamesChoosePlayDraw" &&
        waitingFor.data.player === playerId && (
          <ChoiceModal
            title={`Game ${waitingFor.data.game_number}: Choose Play or Draw`}
            subtitle={`Match score ${waitingFor.data.score.p0_wins}-${waitingFor.data.score.p1_wins}`}
            options={[
              {
                id: "play",
                label: "Play First",
                description: "Take the first turn",
              },
              {
                id: "draw",
                label: "Draw First",
                description: "Take the extra draw on your first turn",
              },
            ]}
            onChoose={(id) => handleChoosePlayDraw(id === "play")}
          />
        )}

      {/* Multiplayer UX overlays */}
      {isOnlineMode && (
        <>
          <ConcedeDialog
            isOpen={showConcedeDialog}
            onConfirm={handleConcede}
            onCancel={onHideConcedeDialog}
          />
          <EmoteOverlay
            onSendEmote={handleSendEmote}
            receivedEmote={receivedEmote}
          />
          {/* Per-player timer display */}
          {Object.entries(timerRemaining).map(([pid, secs]) =>
            secs > 0 ? (
              <div
                key={pid}
                className={`fixed z-30 text-xs font-mono font-bold ${
                  Number(pid) === playerId
                    ? "bottom-40 left-1/2 -translate-x-1/2 text-amber-400"
                    : "top-16 left-1/2 -translate-x-1/2 text-red-400"
                }`}
              >
                {Math.floor(secs / 60)}:{String(secs % 60).padStart(2, "0")}
              </div>
            ) : null,
          )}
        </>
      )}

      {waitingFor?.type === "GameOver" && (
        <GameOverScreen
          winner={waitingFor.data.winner}
          mode={mode}
          isOnlineMode={isOnlineMode}
          gameStartedAt={gameStartedAt}
        />
      )}
    </div>
  );
}

// ── Mulligan Bottom Cards ─────────────────────────────────────────────────

interface MulliganBottomCardsPromptProps {
  playerId: number;
  count: number;
  onChoose: (id: string) => void;
}

function BetweenGamesSideboardPrompt({
  gameNumber,
  score,
  onSubmit,
}: {
  gameNumber: number;
  score: { p0_wins: number; p1_wins: number; draws: number };
  onSubmit: () => void;
}) {
  return (
    <ChoiceModal
      title={`Game ${gameNumber}: Sideboarding`}
      subtitle={`Match score ${score.p0_wins}-${score.p1_wins}${score.draws > 0 ? ` (${score.draws} draw)` : ""}`}
      options={[
        {
          id: "submit",
          label: "Submit Deck",
          description: "Keep current main/sideboard configuration",
        },
      ]}
      onChoose={() => onSubmit()}
    />
  );
}

interface MulliganDecisionPromptProps {
  playerId: number;
  mulliganCount: number;
  onChoose: (id: string) => void;
}

interface MulliganPanelProps {
  eyebrow: string;
  title: string;
  subtitle: string;
  children: React.ReactNode;
  footer?: React.ReactNode;
}

function MulliganPanel({
  eyebrow,
  title,
  subtitle,
  children,
  footer,
}: MulliganPanelProps) {
  return (
    <div className="fixed inset-0 z-50 overflow-y-auto px-3 py-4 sm:px-4 sm:py-6">
      <div className="absolute inset-0 bg-[radial-gradient(circle_at_top,rgba(31,41,55,0.55),rgba(2,6,23,0.92)_58%,rgba(2,6,23,0.98))]" />
      <div className="relative flex min-h-full items-center justify-center pb-[env(safe-area-inset-bottom)] pt-[env(safe-area-inset-top)]">
        <motion.div
          className="relative z-10 flex w-full max-w-6xl flex-col overflow-hidden rounded-[28px] border border-white/10 bg-[#0b1020]/94 shadow-[0_32px_90px_rgba(0,0,0,0.48)] backdrop-blur-md"
          initial={{ opacity: 0, y: 18, scale: 0.98 }}
          animate={{ opacity: 1, y: 0, scale: 1 }}
          transition={{ duration: 0.24, ease: "easeOut" }}
        >
          <div className="border-b border-white/10 px-5 py-5 sm:px-7 sm:py-6">
            <div className="text-[0.68rem] uppercase tracking-[0.24em] text-slate-500">
              {eyebrow}
            </div>
            <h2 className="mt-2 text-2xl font-semibold text-white sm:text-3xl">
              {title}
            </h2>
            <p className="mt-2 max-w-2xl text-sm text-slate-400 sm:text-[0.95rem]">
              {subtitle}
            </p>
          </div>

          <div className="flex-1 px-3 py-4 sm:px-5 sm:py-5">{children}</div>

          {footer && (
            <div className="border-t border-white/10 bg-black/15 px-4 py-4 sm:px-6">
              {footer}
            </div>
          )}
        </motion.div>
      </div>
    </div>
  );
}

function MulliganDecisionPrompt({
  playerId,
  mulliganCount,
  onChoose,
}: MulliganDecisionPromptProps) {
  const player = useGameStore((s) => s.gameState?.players[playerId]);
  const objects = useGameStore((s) => s.gameState?.objects);
  const inspectObject = useUiStore((s) => s.inspectObject);
  const [buttonsVisible, setButtonsVisible] = useState(false);

  if (!player || !objects) {
    return (
      <ChoiceModal
        title={`Mulligan (${mulliganCount} cards)`}
        options={[
          { id: "keep", label: "Keep Hand" },
          {
            id: "mulligan",
            label: "Mulligan",
            description: `Draw ${7 - mulliganCount - 1} cards`,
          },
        ]}
        onChoose={onChoose}
      />
    );
  }

  const handObjects = player.hand.map((id) => objects[id]).filter(Boolean);
  const nextHandSize = 7 - mulliganCount - 1;
  return (
    <MulliganPanel
      eyebrow={mulliganCount > 0 ? `Mulligan ${mulliganCount}` : "Opening Hand"}
      title="Review your opening hand"
      subtitle={
        mulliganCount > 0
          ? `Choose whether to keep this ${handObjects.length}-card hand or mulligan down to ${nextHandSize}.`
          : "Take a final look before the game starts."
      }
      footer={
        <AnimatePresence>
          {buttonsVisible && (
            <motion.div
              className="flex w-full flex-col gap-3 sm:flex-row sm:items-center sm:justify-end"
              initial={{ opacity: 0, y: 20 }}
              animate={{ opacity: 1, y: 0 }}
              transition={{ duration: 0.22 }}
            >
              <button
                onClick={() => onChoose("mulligan")}
                className="min-h-11 rounded-[16px] border border-white/12 bg-white/5 px-5 py-3 text-sm font-semibold text-slate-200 transition hover:bg-white/8 hover:text-white sm:text-base"
              >
                Mulligan to {nextHandSize}
              </button>
              <button
                onClick={() => onChoose("keep")}
                className="min-h-11 rounded-[16px] bg-cyan-500 px-5 py-3 text-sm font-semibold text-slate-950 shadow-[0_14px_34px_rgba(6,182,212,0.28)] transition hover:bg-cyan-400 sm:text-base"
              >
                Keep Hand
              </button>
            </motion.div>
          )}
        </AnimatePresence>
      }
    >
      <div
        className="rounded-[22px] border border-white/8 bg-black/18 px-2 py-4 sm:px-4"
        style={
          {
            "--card-w": "clamp(100px, 14vw, 180px)",
            "--card-h": "clamp(140px, 19.6vw, 252px)",
          } as React.CSSProperties
        }
      >
        <div className="overflow-x-auto pb-3">
          <div className="mx-auto flex w-max min-w-full items-center justify-center px-2 sm:px-4">
            {handObjects.map((obj, index) => (
              <motion.div
                key={obj.id}
                className="cursor-pointer flex-shrink-0 rounded-[18px] transition-shadow duration-200 hover:z-50 hover:shadow-[0_0_24px_rgba(56,189,248,0.22)]"
                style={{
                  marginLeft: index === 0 ? 0 : "clamp(-26px, -3vw, -16px)",
                }}
                initial={{ opacity: 0, y: 80, scale: 0.8 }}
                animate={{ opacity: 1, y: 0, scale: 1 }}
                transition={{
                  delay: 0.1 + index * 0.08,
                  duration: 0.4,
                  ease: "easeOut",
                }}
                whileHover={{ scale: 1.06, y: -12 }}
                onAnimationComplete={() => {
                  if (index === handObjects.length - 1) setButtonsVisible(true);
                }}
                onMouseEnter={() => inspectObject(obj.id)}
                onMouseLeave={() => inspectObject(null)}
              >
                <CardImage
                  cardName={obj.name}
                  size="normal"
                  className="h-[clamp(160px,28vh,252px)] w-[clamp(114px,20vh,180px)]"
                />
              </motion.div>
            ))}
          </div>
        </div>
      </div>
    </MulliganPanel>
  );
}

function MulliganBottomCardsPrompt({
  playerId,
  count,
  onChoose,
}: MulliganBottomCardsPromptProps) {
  const player = useGameStore((s) => s.gameState?.players[playerId]);
  const objects = useGameStore((s) => s.gameState?.objects);
  const selectedCardIds = useUiStore((s) => s.selectedCardIds);
  const addSelectedCard = useUiStore((s) => s.addSelectedCard);
  const inspectObject = useUiStore((s) => s.inspectObject);

  if (!player || !objects) return null;

  const handObjects = player.hand.map((id) => objects[id]).filter(Boolean);
  const isReady = selectedCardIds.length === count;

  const handleConfirm = () => {
    onChoose(selectedCardIds.join(","));
  };

  return (
    <MulliganPanel
      eyebrow="London Mulligan"
      title={`Put ${count} card${count > 1 ? "s" : ""} on the bottom`}
      subtitle={`Select ${count} card${count > 1 ? "s" : ""} from your hand. They will be returned to the bottom of your library in the order you choose here.`}
      footer={
        <motion.div
          className="flex w-full flex-col gap-3 sm:flex-row sm:items-center sm:justify-between"
          initial={{ opacity: 0, y: 20 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ delay: 0.12, duration: 0.22 }}
        >
          <div className="text-sm text-slate-400">
            Selected {selectedCardIds.length} of {count}
          </div>
          <button
            onClick={handleConfirm}
            disabled={!isReady}
            className={`min-h-11 rounded-[16px] px-5 py-3 text-sm font-semibold transition sm:text-base ${
              isReady
                ? "bg-cyan-500 text-slate-950 shadow-[0_14px_34px_rgba(6,182,212,0.28)] hover:bg-cyan-400"
                : "cursor-not-allowed border border-white/8 bg-white/5 text-slate-500"
            }`}
          >
            Confirm Selection
          </button>
        </motion.div>
      }
    >
      <div
        className="rounded-[22px] border border-white/8 bg-black/18 px-2 py-4 sm:px-4"
        style={
          {
            "--card-w": "clamp(100px, 14vw, 180px)",
            "--card-h": "clamp(140px, 19.6vw, 252px)",
          } as React.CSSProperties
        }
      >
        <div className="overflow-x-auto pb-3">
          <div className="mx-auto flex w-max min-w-full items-center justify-center px-2 sm:px-4">
            {handObjects.map((obj, index) => {
              const isSelected = selectedCardIds.includes(obj.id);
              return (
                <motion.button
                  key={obj.id}
                  onClick={() => {
                    if (!isSelected && selectedCardIds.length < count) {
                      addSelectedCard(obj.id);
                    }
                  }}
                  className={`flex-shrink-0 rounded-[18px] p-1 transition hover:z-50 ${
                    isSelected
                      ? "z-40 ring-2 ring-cyan-300 shadow-[0_0_0_1px_rgba(103,232,249,0.55)] opacity-75"
                      : "hover:shadow-[0_0_24px_rgba(56,189,248,0.22)]"
                  }`}
                  style={{
                    marginLeft: index === 0 ? 0 : "clamp(-26px, -3vw, -16px)",
                  }}
                  initial={{ opacity: 0, y: 80, scale: 0.8 }}
                  animate={{ opacity: isSelected ? 0.75 : 1, y: 0, scale: 1 }}
                  transition={{
                    delay: 0.1 + index * 0.08,
                    duration: 0.4,
                    ease: "easeOut",
                  }}
                  whileHover={{ scale: 1.06, y: -12 }}
                  onMouseEnter={() => inspectObject(obj.id)}
                  onMouseLeave={() => inspectObject(null)}
                >
                  <CardImage
                    cardName={obj.name}
                    size="normal"
                    className="h-[clamp(160px,28vh,252px)] w-[clamp(114px,20vh,180px)]"
                  />
                </motion.button>
              );
            })}
          </div>
        </div>
      </div>
    </MulliganPanel>
  );
}

// ── Game Over Screen ──────────────────────────────────────────────────────

// Golden floating particles for victory screen
function VictoryParticles() {
  const particles = Array.from({ length: 24 }, (_, i) => ({
    id: i,
    left: `${5 + Math.random() * 90}%`,
    size: 2 + Math.random() * 4,
    delay: Math.random() * 3,
    duration: 3 + Math.random() * 4,
    opacity: 0.3 + Math.random() * 0.5,
  }));

  return (
    <div className="pointer-events-none absolute inset-0 overflow-hidden">
      {particles.map((p) => (
        <motion.div
          key={p.id}
          className="absolute rounded-full"
          style={{
            left: p.left,
            bottom: "-10px",
            width: p.size,
            height: p.size,
            backgroundColor: "#C9B037",
          }}
          animate={{
            y: [0, -window.innerHeight - 20],
            opacity: [0, p.opacity, p.opacity, 0],
          }}
          transition={{
            duration: p.duration,
            delay: p.delay,
            repeat: Infinity,
            ease: "linear",
          }}
        />
      ))}
    </div>
  );
}

function GameOverScreen({
  winner,
  mode,
  isOnlineMode = false,
  gameStartedAt,
}: {
  winner: number | null;
  mode: string | null;
  isOnlineMode?: boolean;
  gameStartedAt?: number | null;
}) {
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const difficulty = searchParams.get("difficulty") ?? "Medium";
  const gameState = useGameStore((s) => s.gameState);
  const players = gameState?.players;
  const [buttonsVisible, setButtonsVisible] = useState(false);

  const activePlayerId = useMultiplayerStore((s) => s.activePlayerId) ?? 0;

  const playerLife = players?.[activePlayerId]?.life ?? 0;
  const opponentLife = players
    ? (players.find((p) => p.id !== activePlayerId)?.life ?? 0)
    : 0;

  const isVictory = winner === activePlayerId;
  const isDraw = winner == null;

  const turnCount = gameState?.turn_number ?? 0;
  const gameDuration = gameStartedAt
    ? Math.floor((Date.now() - gameStartedAt) / 1000)
    : null;

  const titleText = isDraw ? "DRAW" : isVictory ? "VICTORY" : "DEFEAT";
  const titleColor = isDraw ? "#B0B0B0" : isVictory ? "#C9B037" : "#991B1B";

  const glowColor = isDraw
    ? "rgba(176,176,176,0.5)"
    : isVictory
      ? "rgba(201,176,55,0.8)"
      : "rgba(153,27,27,0.8)";

  const textShadow = `0 0 20px ${glowColor}, 0 0 40px ${glowColor.replace(/[\d.]+\)$/, "0.5)")}, 0 0 80px ${glowColor.replace(/[\d.]+\)$/, "0.3)")}`;

  const bgGradient = isDraw
    ? "radial-gradient(ellipse at center, rgba(50,50,50,0.6) 0%, rgba(0,0,0,0.95) 70%)"
    : isVictory
      ? "radial-gradient(ellipse at center, rgba(60,50,10,0.6) 0%, rgba(0,0,0,0.95) 70%)"
      : "radial-gradient(ellipse at center, rgba(60,10,10,0.5) 0%, rgba(0,0,0,0.95) 70%)";

  const handleRematch = () => {
    const newId = crypto.randomUUID();
    const params = new URLSearchParams();
    if (mode) params.set("mode", mode);
    params.set("difficulty", difficulty);
    navigate(`/game/${newId}?${params.toString()}`);
  };

  return (
    <div
      className="fixed inset-0 z-50 flex flex-col items-center justify-center px-4"
      style={{ background: bgGradient }}
    >
      {/* Victory particles */}
      {isVictory && <VictoryParticles />}

      {/* Title text */}
      <motion.h2
        className="relative z-10 text-4xl font-black tracking-[0.24em] text-center sm:text-6xl sm:tracking-widest"
        style={{ color: titleColor, textShadow }}
        initial={{ scale: 0.5, opacity: 0 }}
        animate={{ scale: 1, opacity: 1 }}
        transition={{
          type: "spring",
          stiffness: 200,
          damping: 12,
          duration: 0.6,
        }}
        onAnimationComplete={() => setButtonsVisible(true)}
      >
        {titleText}
      </motion.h2>

      {/* Life totals and game stats */}
      <AnimatePresence>
        {buttonsVisible && (
          <motion.div
            className="relative z-10 mt-6 rounded-[20px] border border-white/10 bg-black/18 px-5 py-4 text-center backdrop-blur-md"
            initial={{ opacity: 0, y: 10 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ duration: 0.4 }}
          >
            <p className="text-base text-gray-200 sm:text-lg">
              You: <span className="font-bold text-white">{playerLife}</span>
              <span className="mx-3 text-gray-500">/</span>
              Opponent:{" "}
              <span className="font-bold text-white">{opponentLife}</span>
            </p>
            {(turnCount > 0 || gameDuration !== null) && (
              <p className="mt-2 text-xs text-gray-400 sm:text-sm">
                {turnCount > 0 && <span>Turns: {turnCount}</span>}
                {turnCount > 0 && gameDuration !== null && (
                  <span className="mx-2 text-gray-600">|</span>
                )}
                {gameDuration !== null && (
                  <span>
                    Duration: {Math.floor(gameDuration / 60)}:
                    {String(gameDuration % 60).padStart(2, "0")}
                  </span>
                )}
              </p>
            )}
          </motion.div>
        )}
      </AnimatePresence>

      {/* Buttons */}
      <AnimatePresence>
        {buttonsVisible && (
          <motion.div
            className="relative z-10 mt-8 flex w-full max-w-[min(28rem,calc(100vw-2rem))] flex-col gap-3 rounded-[22px] border border-white/10 bg-[#0b1020]/82 p-2 shadow-[0_20px_48px_rgba(0,0,0,0.38)] backdrop-blur-md sm:w-auto sm:max-w-fit sm:flex-row sm:items-center sm:justify-center"
            initial={{ opacity: 0, y: 20 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ delay: 0.15, duration: 0.3 }}
          >
            {isOnlineMode ? (
              <button
                onClick={() => navigate("/?view=lobby")}
                className={gameButtonClass({
                  tone: isVictory ? "amber" : "slate",
                  size: "lg",
                  className: "w-full justify-center sm:w-auto sm:min-w-[12rem]",
                })}
              >
                Back to Lobby
              </button>
            ) : (
              <button
                onClick={() => navigate("/")}
                className={gameButtonClass({
                  tone: isVictory ? "amber" : "slate",
                  size: "lg",
                  className: "w-full justify-center sm:w-auto sm:min-w-[12rem]",
                })}
              >
                Return to Menu
              </button>
            )}
            <button
              onClick={handleRematch}
              className={gameButtonClass({
                tone: isVictory ? "emerald" : "neutral",
                size: "lg",
                className: "w-full justify-center sm:w-auto sm:min-w-[12rem]",
              })}
            >
              Rematch
            </button>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

// ── Ability Choice Modal ──────────────────────────────────────────────────

function AbilityChoiceModal() {
  const dispatch = useGameDispatch();
  const pending = useUiStore((s) => s.pendingAbilityChoice);
  const setPending = useUiStore((s) => s.setPendingAbilityChoice);
  const obj = useGameStore((s) =>
    pending ? s.gameState?.objects[pending.objectId] : undefined,
  );

  if (!pending || !obj) return null;

  return (
    <ChoiceModal
      title={obj.name}
      subtitle="Choose an ability to activate"
      options={pending.actions.map((action, i) => {
        const { label, description } = abilityChoiceLabel(
          action,
          obj.abilities,
        );
        return { id: String(i), label, description };
      })}
      onChoose={(id) => {
        dispatch(pending.actions[Number(id)]);
        setPending(null);
      }}
      onClose={() => setPending(null)}
    />
  );
}

// ── Optional Cost Choice Modal ──────────────────────────────────────────

function OptionalCostModal() {
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);

  if (waitingFor?.type !== "OptionalCostChoice") return null;

  const { cost } = waitingFor.data;
  const { title, payLabel, skipLabel } = additionalCostChoices(cost);
  // Mandatory Choice costs (e.g. "discard a card or pay 3 life") require picking one —
  // no cancel/close allowed. Optional costs allow canceling the cast.
  const isMandatoryChoice = cost.type === "Choice";

  return (
    <ChoiceModal
      title={title}
      options={[
        { id: "pay", label: payLabel },
        { id: "skip", label: skipLabel },
      ]}
      onChoose={(id) =>
        dispatch({ type: "DecideOptionalCost", data: { pay: id === "pay" } })
      }
      onClose={isMandatoryChoice ? undefined : () => dispatch({ type: "CancelCast" })}
    />
  );
}

// ── Optional Effect Choice Modal ────────────────────────────────────────

function OptionalEffectModal() {
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);
  const objects = useGameStore((s) => s.gameState?.objects ?? {});

  if (waitingFor?.type !== "OptionalEffectChoice") return null;

  const sourceObj = objects[waitingFor.data.source_id];
  const sourceName = sourceObj?.name ?? "Effect";

  return (
    <ChoiceModal
      title={`${sourceName} — Optional Effect`}
      options={[
        { id: "accept", label: "Yes" },
        { id: "decline", label: "No" },
      ]}
      onChoose={(id) =>
        dispatch({ type: "DecideOptionalEffect", data: { accept: id === "accept" } })
      }
    />
  );
}

// ── Unless Payment Modal (CR 118.12) ────────────────────────────────────

function formatManaCost(cost: { type: string; shards?: string[]; generic?: number }): string {
  if (cost.type === "NoCost") return "0";
  const parts: string[] = [];
  if (cost.generic && cost.generic > 0) parts.push(`{${cost.generic}}`);
  for (const shard of cost.shards ?? []) {
    parts.push(`{${shard}}`);
  }
  return parts.join("") || "0";
}

function UnlessPaymentModal() {
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);

  if (waitingFor?.type !== "UnlessPayment") return null;

  const costDisplay = formatManaCost(waitingFor.data.cost);

  return (
    <ChoiceModal
      title="Counter Unless You Pay"
      options={[
        { id: "pay", label: `Pay ${costDisplay}` },
        { id: "decline", label: "Don\u2019t Pay" },
      ]}
      onChoose={(id) =>
        dispatch({ type: "PayUnlessCost", data: { pay: id === "pay" } })
      }
    />
  );
}
