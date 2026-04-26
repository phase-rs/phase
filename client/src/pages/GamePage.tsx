import {
  type CSSProperties,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useLocation, useNavigate, useParams, useSearchParams } from "react-router";
import { AnimatePresence, motion } from "framer-motion";

import type { DeckCardCount, GameFormat, MatchConfig } from "../adapter/types";
import { BetweenGamesSideboardModal } from "../components/multiplayer/BetweenGamesSideboardModal.tsx";
import { audioManager } from "../audio/AudioManager.ts";
import { useAudioContext } from "../audio/useAudioContext.ts";
import { AnimationOverlay } from "../components/animation/AnimationOverlay.tsx";
import { TurnBanner } from "../components/animation/TurnBanner.tsx";
import { BattlefieldBackground } from "../components/board/BattlefieldBackground.tsx";
import { BoardContextMenu } from "../components/board/BoardContextMenu.tsx";
import { AttackTargetLines } from "../components/board/AttackTargetLines.tsx";
import { BlockAssignmentLines } from "../components/board/BlockAssignmentLines.tsx";
import { GameBoard } from "../components/board/GameBoard.tsx";
import { CardImage } from "../components/card/CardImage.tsx";
import { CardPreview } from "../components/card/CardPreview.tsx";
import { ActionButton } from "../components/board/ActionButton.tsx";
import { FullControlToggle } from "../components/controls/FullControlToggle.tsx";
import { CombatPhaseIndicator } from "../components/controls/PhaseStopBar.tsx";
import { OpponentHand } from "../components/hand/OpponentHand.tsx";
import { MobileHandDrawer } from "../components/hand/MobileHandDrawer.tsx";
import { PlayerHand } from "../components/hand/PlayerHand.tsx";
import { GameLogPanel } from "../components/log/GameLogPanel.tsx";
import { ChooseXValueUI } from "../components/mana/ChooseXValueUI.tsx";
import { ManaPaymentUI } from "../components/mana/ManaPaymentUI.tsx";
import { CardDataMissingModal } from "../components/modal/CardDataMissingModal.tsx";
import { AdventureCastModal } from "../components/modal/AdventureCastModal.tsx";
import { CascadeChoiceModal } from "../components/modal/CascadeChoiceModal.tsx";
import { ModalFaceModal } from "../components/modal/ModalFaceModal.tsx";
import { WarpCostModal } from "../components/modal/WarpCostModal.tsx";
import { MiracleRevealModal } from "../components/modal/MiracleRevealModal.tsx";
import { CardChoiceModal } from "../components/modal/CardChoiceModal.tsx";
import { ChoiceModal } from "../components/modal/ChoiceModal.tsx";
import { ModeChoiceModal } from "../components/modal/ModeChoiceModal.tsx";
import { ReplacementModal } from "../components/modal/ReplacementModal.tsx";
import { BattleProtectorModal } from "../components/modal/BattleProtectorModal.tsx";
import { TributeModal } from "../components/modal/TributeModal.tsx";
import { CombatTaxModal } from "../components/modal/CombatTaxModal.tsx";
import { DialogHost } from "../components/modal/DialogHost.tsx";
import { StackDisplay } from "../components/stack/StackDisplay.tsx";
import { TargetingOverlay } from "../components/targeting/TargetingOverlay.tsx";
import { PlayerHud } from "../components/hud/PlayerHud.tsx";
import { OpponentHud } from "../components/hud/OpponentHud.tsx";
import { GraveyardPile } from "../components/zone/GraveyardPile.tsx";
import { LibraryPile } from "../components/zone/LibraryPile.tsx";
import { ZoneIndicator } from "../components/zone/ZoneIndicator.tsx";
import { CompanionZone } from "../components/zone/CompanionZone.tsx";
import { ZoneHand } from "../components/hand/ZoneHand.tsx";
import { ZoneViewer } from "../components/zone/ZoneViewer.tsx";
import {
  PreferencesModal,
  type SettingsHighlight,
  type SettingsTabId,
} from "../components/settings/PreferencesModal.tsx";
import { DebugPanel } from "../components/chrome/DebugPanel.tsx";
import { GameMenu } from "../components/chrome/GameMenu.tsx";
import { ConcedeDialog } from "../components/multiplayer/ConcedeDialog.tsx";
import { ConnectionToast } from "../components/multiplayer/ConnectionToast.tsx";
import { EmoteOverlay } from "../components/multiplayer/EmoteOverlay.tsx";
import { LobbyProgress } from "../components/multiplayer/LobbyProgress.tsx";
import { DisconnectChoiceDialog } from "../components/hud/DisconnectChoiceDialog.tsx";
import { PausedBanner } from "../components/chrome/PausedBanner.tsx";
import type { P2PAdapterEvent } from "../adapter/p2p-adapter.ts";
import { WebSocketAdapter } from "../adapter/ws-adapter.ts";
import type { WsAdapterEvent } from "../adapter/ws-adapter.ts";
import { useGameDispatch } from "../hooks/useGameDispatch.ts";
import { useInspectHoverProps } from "../hooks/useInspectHoverProps.ts";
import { useKeyboardShortcuts } from "../hooks/useKeyboardShortcuts.ts";
import { usePreviewDismiss } from "../hooks/usePreviewDismiss.ts";
import { clearGame, useGameStore } from "../stores/gameStore.ts";
import { useUiStore } from "../stores/uiStore.ts";
import { usePreferencesStore } from "../stores/preferencesStore.ts";
import {
  FORMAT_DEFAULTS,
  getOpponentDisplayName,
  getPlayerDisplayName,
  playerToastKey,
  useMultiplayerStore,
  type PlayerSlot,
} from "../stores/multiplayerStore.ts";
import { GameProvider } from "../providers/GameProvider.tsx";
import { useCanActForWaitingState, usePerspectivePlayerId, usePlayerId } from "../hooks/usePlayerId.ts";
import { abilityChoiceLabel, additionalCostChoices } from "../viewmodel/costLabel.ts";
import { gameButtonClass } from "../components/ui/buttonStyles.ts";
import { cardImageLookup } from "../services/cardImageLookup.ts";

type ZoneRailStyle = CSSProperties & {
  "--card-w": string;
  "--card-h": string;
};

/**
 * User-facing messages keyed by `P2PAdapterEvent.hostingFailed.reason`.
 * Typed as `Record<ReasonUnion, string>` so adding a new reason to the
 * adapter event union without adding a message here is a compile error —
 * the idiomatic TS replacement for a `switch`-with-`never`-default when
 * the union has a single arm today but will grow.
 */
const HOSTING_FAILURE_MESSAGES: Record<
  Extract<P2PAdapterEvent, { type: "hostingFailed" }>["reason"],
  string
> = {
  room_still_claimed:
    "Your previous room is still winding down on the signaling server. Wait about a minute, then try Resume again.",
};

export function GamePage() {
  const navigate = useNavigate();
  const { id: gameId } = useParams<{ id: string }>();
  const [searchParams] = useSearchParams();
  const location = useLocation();
  // `useBroker` is threaded through React Router's location state from
  // `MultiplayerPage` — intentionally not a URL param, so a hard refresh
  // re-evaluates broker reachability instead of pinning the "no lobby"
  // choice silently. On hard-refresh the location state is absent; fall
  // back to the store's cached `serverInfo.mode` so the user only gets
  // broker registration when the reachable server is actually `LobbyOnly`.
  // Without this gate, refreshing `/game/<id>?mode=p2p-host` against a
  // Full-mode server would attempt `openBrokerClient` and surface an
  // "Expected LobbyOnly server, got Full" error to the user.
  const locationState = location.state as { useBroker?: boolean } | null;
  const cachedServerMode = useMultiplayerStore((s) => s.serverInfo?.mode);
  const useBroker = locationState?.useBroker ?? (cachedServerMode === "LobbyOnly");
  const rawMode = searchParams.get("mode");
  const difficulty = searchParams.get("difficulty") ?? "Medium";
  const joinCode = searchParams.get("code") ?? "";
  const formatParam = searchParams.get("format") as GameFormat | null;
  const playersParam = searchParams.get("players");
  const matchParam = searchParams.get("match");
  const firstParam = searchParams.get("first");
  const roomNameParam = searchParams.get("roomName");
  const playerCount = playersParam ? Number(playersParam) : undefined;
  // Memoize so the `GameProvider` `useEffect` dep array doesn't
  // tear-down/rebuild the P2P session on every parent re-render. Without
  // `useMemo`, each render constructs a fresh object reference from
  // `FORMAT_DEFAULTS[formatParam]` (its lookup returns a stable reference,
  // but TypeScript's narrowing produces a fresh binding that the linter
  // treats as new). The explicit memo makes the stability guarantee
  // self-documenting.
  const formatConfig = useMemo(
    () => (formatParam ? FORMAT_DEFAULTS[formatParam] : undefined),
    [formatParam],
  );
  // CR 103.1: 0 = play first, 1 = draw first, undefined = random
  const firstPlayer = firstParam === "play" ? 0 : firstParam === "draw" ? 1 : undefined;
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
  const [reconnectState, setReconnectState] = useState<
    | { status: "idle" }
    | { status: "reconnecting"; attempt: number; maxAttempts: number }
    | { status: "failed" }
  >({ status: "idle" });

  // P2P 3-4p multiplayer additions
  const [disconnectChoice, setDisconnectChoice] = useState<
    { playerId: number; gracePeriodMs: number } | null
  >(null);
  const [pauseReason, setPauseReason] = useState<string | null>(null);

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
  const hasConcededRef = useRef(false);

  const handleWsEvent = useCallback((event: WsAdapterEvent) => {
    switch (event.type) {
      case "gameCreated":
        setHostGameCode(event.gameCode);
        break;
      case "waitingForOpponent":
        setWaitingForOpponent(true);
        break;
      case "opponentDisconnected": {
        setOpponentDisconnected(true);
        // 2-player: mark the single opponent as disconnected
        const myId = useMultiplayerStore.getState().activePlayerId ?? 0;
        const oppId = myId === 0 ? 1 : 0;
        useMultiplayerStore.getState().setPlayerDisconnected(oppId);
        useMultiplayerStore.getState().showToast(`${getOpponentDisplayName(oppId)} disconnected`, {
          countdownSeconds: event.graceSeconds,
          key: playerToastKey(oppId),
        });
        break;
      }
      case "opponentReconnected": {
        setOpponentDisconnected(false);
        // 2-player: clear disconnected status
        const myId = useMultiplayerStore.getState().activePlayerId ?? 0;
        const oppId = myId === 0 ? 1 : 0;
        useMultiplayerStore.getState().setPlayerReconnected(oppId);
        useMultiplayerStore.getState().clearToast(playerToastKey(oppId));
        break;
      }
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
        // If WE conceded, navigate to menu immediately
        if (event.player === useMultiplayerStore.getState().activePlayerId) {
          hasConcededRef.current = true;
          if (gameId) clearGame(gameId);
          navigate("/");
        }
        break;
      case "gameOver":
        // Skip if we already navigated away from a self-concede — the server sends
        // both Conceded and GameOver to all players, so this would race with navigate.
        if (hasConcededRef.current) break;
        // Server-initiated game end (concede, disconnect timeout, etc.)
        // Map the server's authoritative winner into the store so GameOverScreen renders.
        if (gameId) clearGame(gameId);
        useGameStore.setState({
          waitingFor: { type: "GameOver", data: { winner: event.winner } },
        });
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
      case "playerDisconnected":
        // Multiplayer (3+ players): a specific player disconnected
        setOpponentDisconnected(true);
        useMultiplayerStore.getState().setPlayerDisconnected(event.playerId);
        useMultiplayerStore.getState().showToast(
          `${getPlayerDisplayName(event.playerId)} disconnected`,
          {
            countdownSeconds: event.graceSeconds,
            key: playerToastKey(event.playerId),
          },
        );
        break;
      case "playerReconnected":
        useMultiplayerStore.getState().setPlayerReconnected(event.playerId);
        useMultiplayerStore.getState().clearToast(playerToastKey(event.playerId));
        if (useMultiplayerStore.getState().disconnectedPlayers.size === 0) {
          setOpponentDisconnected(false);
        }
        break;
      case "gamePaused":
        setOpponentDisconnected(true);
        useMultiplayerStore.getState().setPlayerDisconnected(event.disconnectedPlayer);
        useMultiplayerStore.getState().showToast(
          `Game paused — ${getPlayerDisplayName(event.disconnectedPlayer)} disconnected`,
          {
            countdownSeconds: event.timeoutSeconds,
            key: playerToastKey(event.disconnectedPlayer),
          },
        );
        break;
      case "gameResumed":
        setOpponentDisconnected(false);
        // Clear per-player disconnect toasts only. Generic toasts (errors,
        // connection warnings) are independent of the pause/resume cycle.
        useMultiplayerStore.getState().clearPlayerToasts();
        break;
      case "playerEliminated":
        // Store-level side effects (isSpectator, toast) already handled in ws-adapter
        break;
      case "spectatorJoined":
        // Could show a toast, but not critical — no UI for this yet
        break;
      case "error":
        useMultiplayerStore.getState().showToast(event.message);
        break;
      case "deckRejected":
        navigate("/multiplayer", {
          state: {
            deckRejected: true,
            reason: event.reason,
            joinCode,
          },
        });
        break;
    }
  }, [gameId, navigate, joinCode]);

  const handleP2PEvent = useCallback((event: P2PAdapterEvent) => {
    switch (event.type) {
      case "roomCreated": {
        setHostGameCode(event.roomCode);
        const effectivePlayerCount = playerCount ?? formatConfig?.max_players ?? 2;
        const slots: PlayerSlot[] = [
          {
            playerId: 0,
            name: useMultiplayerStore.getState().displayName || "Host",
            kind: { type: "HostHuman" },
          },
          ...Array.from({ length: effectivePlayerCount - 1 }, (_, i) => ({
            playerId: i + 1,
            name: "",
            kind: { type: "WaitingHuman" as const },
          })),
        ];
        useMultiplayerStore.setState({
          hostGameCode: event.roomCode,
          hostingStatus: "waiting",
          hostSession: formatConfig
            ? {
                formatConfig,
                timerSeconds: null,
                matchType: matchConfig?.match_type === "Bo3" ? "Bo3" : "Bo1",
              }
            : null,
          playerSlots: slots,
        });
        break;
      }
      case "waitingForGuest":
        setWaitingForOpponent(true);
        break;
      case "guestConnected":
        break;
      case "roomFull":
        useMultiplayerStore.getState().showToast("Room full — ready to start!");
        break;
      case "opponentDisconnected":
        setOpponentDisconnected(true);
        break;
      case "opponentDisconnectedWithChoice":
        setDisconnectChoice({
          playerId: event.playerId,
          gracePeriodMs: event.gracePeriodMs,
        });
        setPauseReason(`${getPlayerDisplayName(event.playerId)} disconnected`);
        break;
      case "playerReconnected":
        // Dismiss the disconnect modal if it was waiting on this player.
        setDisconnectChoice((cur) => (cur?.playerId === event.playerId ? null : cur));
        break;
      case "gamePaused":
        setPauseReason(event.reason);
        break;
      case "gameResumed":
        setPauseReason(null);
        setDisconnectChoice(null);
        setOpponentDisconnected(false);
        break;
      case "playerKicked":
        // If this was the player whose disconnect was prompting the dialog,
        // dismiss it now that they're conceded.
        setDisconnectChoice((cur) => (cur?.playerId === event.playerId ? null : cur));
        break;
      case "lobbyProgress": {
        const { setLobbyProgress } = useGameStore.getState();
        setLobbyProgress({ joined: event.joined, total: event.total });
        // When all seats arrive, clear lobby UI — game_setup is about to fire.
        if (event.joined >= event.total) {
          setLobbyProgress(null);
          setWaitingForOpponent(false);
          useMultiplayerStore.setState({
            hostGameCode: null,
            hostingStatus: "idle",
          });
        }
        break;
      }
      case "playerConceded":
        // Treat conceded players the same as kicked for dialog dismissal.
        setDisconnectChoice((cur) => (cur?.playerId === event.playerId ? null : cur));
        break;
      case "playerIdentity":
        setReconnectState({ status: "idle" });
        useMultiplayerStore.getState().clearToast();
        break;
      case "reconnecting":
        setReconnectState({
          status: "reconnecting",
          attempt: event.attempt,
          maxAttempts: 0,
        });
        useMultiplayerStore.getState().showToast(
          `Host disconnected — reconnecting (attempt ${event.attempt})…`,
        );
        break;
      case "reconnectFailed":
        setReconnectState({ status: "failed" });
        break;
      case "playerSlotsUpdated":
        useMultiplayerStore.setState({ playerSlots: event.slots });
        break;
      case "gameOver":
        if (gameId) clearGame(gameId);
        useGameStore.setState({
          waitingFor: { type: "GameOver", data: { winner: event.winner } },
        });
        break;
      case "deckRejected":
        navigate("/multiplayer", {
          state: {
            deckRejected: true,
            reason: event.reason,
            format: event.format,
            joinCode,
          },
        });
        break;
      case "error":
        useMultiplayerStore.getState().showToast(event.message);
        setReconnectState({ status: "failed" });
        break;
      case "hostingFailed": {
        // Pre-game setup failure — distinct from `error` (catch-all for
        // connection drops mid-game) because we haven't entered a game
        // yet and `setReconnectState` would be semantically wrong (no
        // connection to reconnect). Show the user what happened and
        // send them back to the menu; the Resume button remains because
        // `clearP2PHostSession` was NOT called — the persisted state is
        // still valid, the signaling server just needs a moment.
        //
        // `HOSTING_FAILURE_MESSAGES` is typed as `Record<ReasonUnion, string>`
        // so adding a new `reason` to the P2PAdapterEvent union without
        // adding a message here is a compile error.
        useMultiplayerStore
          .getState()
          .showToast(HOSTING_FAILURE_MESSAGES[event.reason]);
        navigate("/");
        break;
      }
    }
  }, [navigate, formatConfig, matchConfig, playerCount, gameId, joinCode]);

  const handleReady = useCallback(() => {
    setWaitingForOpponent(false);
  }, []);

  const handleNoDeck = useCallback(() => {
    navigate("/");
  }, [navigate]);

  const handleCardDataMissing = useCallback(() => {
    setShowCardDataMissing(true);
  }, []);

  const [resumeResetReason, setResumeResetReason] = useState<string | null>(null);
  const handleResumeReset = useCallback((reason: string) => {
    setResumeResetReason(reason);
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
      firstPlayer={firstPlayer}
      useBroker={useBroker}
      roomName={roomNameParam ?? undefined}
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
      onResumeReset={handleResumeReset}
    >
      <GamePageContent
        gameId={gameId}
        mode={rawMode}
        isOnlineMode={mode === "online"}
        hostGameCode={hostGameCode}
        waitingForOpponent={waitingForOpponent}
        opponentDisconnected={opponentDisconnected}
        reconnectState={reconnectState}
        showCardDataMissing={showCardDataMissing}
        onDismissCardDataMissing={() => setShowCardDataMissing(false)}
        resumeResetReason={resumeResetReason}
        onDismissResumeReset={() => setResumeResetReason(null)}
        showConcedeDialog={showConcedeDialog}
        onShowConcedeDialog={() => setShowConcedeDialog(true)}
        onHideConcedeDialog={() => setShowConcedeDialog(false)}
        receivedEmote={receivedEmote}
        timerRemaining={timerRemaining}
        gameStartedAt={gameStartedAt}
        disconnectChoice={disconnectChoice}
        onDismissDisconnectChoice={() => setDisconnectChoice(null)}
        pauseReason={pauseReason}
        isP2PHost={mode === "p2p-host"}
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
  reconnectState:
    | { status: "idle" }
    | { status: "reconnecting"; attempt: number; maxAttempts: number }
    | { status: "failed" };
  showCardDataMissing: boolean;
  onDismissCardDataMissing: () => void;
  resumeResetReason: string | null;
  onDismissResumeReset: () => void;
  showConcedeDialog: boolean;
  onShowConcedeDialog: () => void;
  onHideConcedeDialog: () => void;
  receivedEmote: string | null;
  timerRemaining: Record<number, number>;
  gameStartedAt: number | null;
  // 3-4p P2P additions
  disconnectChoice: { playerId: number; gracePeriodMs: number } | null;
  onDismissDisconnectChoice: () => void;
  pauseReason: string | null;
  isP2PHost: boolean;
}

function GamePageContent({
  gameId,
  mode,
  isOnlineMode,
  hostGameCode,
  waitingForOpponent: _waitingForOpponent,
  opponentDisconnected,
  reconnectState,
  showCardDataMissing,
  onDismissCardDataMissing,
  resumeResetReason,
  onDismissResumeReset,
  showConcedeDialog,
  onShowConcedeDialog,
  onHideConcedeDialog,
  receivedEmote,
  timerRemaining,
  gameStartedAt,
  disconnectChoice,
  onDismissDisconnectChoice,
  pauseReason,
  isP2PHost,
}: GamePageContentProps) {
  const navigate = useNavigate();
  const containerRef = useRef<HTMLDivElement>(null);

  const waitingFor = useGameStore((s) => s.waitingFor);
  const lobbyProgress = useGameStore((s) => s.lobbyProgress);
  const dispatch = useGameDispatch();
  const inspectedObjectId = useUiStore((s) => s.inspectedObjectId);
  const objects = useGameStore((s) => s.gameState?.objects);
  const seatOrder = useGameStore((s) => s.gameState?.seat_order);
  const players = useGameStore((s) => s.gameState?.players);
  const eliminatedPlayers = useGameStore((s) => s.gameState?.eliminated_players);
  const turnNumber = useGameStore((s) => s.gameState?.turn_number);
  const engineWaitingFor = useGameStore((s) => s.gameState?.waiting_for);
  const deckPools = useGameStore((s) => s.gameState?.deck_pools);
  const [showAiHand, setShowAiHand] = useState(false);
  const [showDebugBounds, setShowDebugBounds] = useState(false);
  const [viewingZone, setViewingZone] = useState<{
    zone: "graveyard" | "exile";
    playerId: number;
  } | null>(null);
  const [preferencesOpen, setPreferencesOpen] = useState<
    null | { tab?: SettingsTabId; highlight?: SettingsHighlight }
  >(null);
  const [boardContextMenu, setBoardContextMenu] = useState<{ x: number; y: number } | null>(null);

  const playerId = usePlayerId();
  const perspectivePlayerId = usePerspectivePlayerId();
  const canActForWaitingState = useCanActForWaitingState();
  const opponentDisplayName = useMultiplayerStore((s) => s.opponentDisplayName);
  const adapter = useGameStore((s) => s.adapter);
  const focusedOpponent = useUiStore((s) => s.focusedOpponent);
  const opponents = useMemo(() => {
    const orderedPlayers = seatOrder ?? players?.map((player) => player.id) ?? [];
    const eliminated = new Set(eliminatedPlayers ?? []);
    return orderedPlayers.filter((id) => id !== perspectivePlayerId && !eliminated.has(id));
  }, [eliminatedPlayers, perspectivePlayerId, players, seatOrder]);
  const activeOpponentId =
    focusedOpponent ?? opponents[0] ?? (perspectivePlayerId === 0 ? 1 : 0);

  useAudioContext("battlefield");

  // Update battlefield music phase based on turn progression
  useEffect(() => {
    if (!turnNumber) return;
    const turn = turnNumber;
    const bp = audioManager.getPhaseBreakpoints();
    const phase = turn >= bp.late ? "late" : turn >= bp.mid ? "mid" : "early";
    audioManager.setBattlefieldPhase(phase);
  }, [turnNumber]);

  const handleConcede = useCallback(() => {
    if (adapter) {
      if (adapter instanceof WebSocketAdapter) {
        adapter.sendConcede();
      } else if ("sendConcede" in adapter && typeof adapter.sendConcede === "function") {
        void (adapter.sendConcede as () => void | Promise<void>)();
      }
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
  // Scryfall lookups must use the front-face name (scryfall-data.json indexes
  // only front faces). When a permanent has transformed, the engine swaps
  // obj.name to the back-face name — cardImageLookup recovers the front name
  // from obj.back_face. See services/cardImageLookup.ts (issue #90).
  const inspectedLookup = inspectedObj ? cardImageLookup(inspectedObj) : null;
  const inspectedCardName = inspectedObj
    ? inspectedFaceIndex === 1 && inspectedObj.back_face
      ? inspectedObj.back_face.name
      : inspectedLookup?.name ?? inspectedObj.name
    : null;
  // The "other" face: when viewing front, this is back_face; when viewing back, this is the front
  const inspectedOtherFaceName = inspectedObj?.back_face
    ? inspectedFaceIndex === 1 ? inspectedObj.name : inspectedObj.back_face.name
    : null;

  useKeyboardShortcuts();
  usePreviewDismiss();

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

  // Register dev-mode console helpers (tree-shaken in production)
  useEffect(() => {
    if (import.meta.env.DEV) {
      import("../dev/devTools.ts");
    }
  }, []);

  // Auto-open graveyard/exile viewer when the engine is waiting for a target in that zone
  useEffect(() => {
    if (!objects) return;
    const wf = engineWaitingFor;
    if (
      (wf?.type !== "TargetSelection" && wf?.type !== "TriggerTargetSelection")
      || !canActForWaitingState
    ) return;

    const legalTargets = wf.data.selection.current_legal_targets;
    // Collect distinct (zone, owner) groupings so we don't trap the user in one
    // graveyard when the effect can target either player's graveyard (e.g. Soul-Guide Lantern).
    const groups = new Set<string>();
    let firstHit: { zone: "graveyard" | "exile"; playerId: number } | null = null;
    for (const t of legalTargets) {
      if (!("Object" in t)) continue;
      const obj = objects[t.Object];
      if (!obj) continue;
      if (obj.zone !== "Graveyard" && obj.zone !== "Exile") continue;
      const zone: "graveyard" | "exile" = obj.zone === "Graveyard" ? "graveyard" : "exile";
      groups.add(`${zone}:${obj.owner}`);
      if (!firstHit) firstHit = { zone, playerId: obj.owner };
    }
    // Only auto-open when there's a single zone+owner to open. Otherwise the
    // zone pile glow (GraveyardPile hasTargetableCards) prompts the user to pick.
    if (groups.size === 1 && firstHit) {
      setViewingZone(firstHit);
    }
  }, [canActForWaitingState, engineWaitingFor, objects]);

  const handleDeclareCompanion = useCallback(
    (cardIndex: number | null) => {
      dispatch({ type: "DeclareCompanion", data: { card_index: cardIndex } });
    },
    [dispatch],
  );

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

  const handleSubmitSideboard = useCallback(
    (main: DeckCardCount[], sideboard: DeckCardCount[]) => {
      dispatch({
        type: "SubmitSideboard",
        data: { main, sideboard },
      });
    },
    [dispatch],
  );

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
      className={`game-no-select relative h-[100dvh] w-full overflow-hidden bg-gray-950${showDebugBounds ? " debug-bounds" : ""}`}
      style={gamePageStyle}
      onContextMenu={(e) => {
        e.preventDefault();
        const target = e.target as HTMLElement | null;
        // Cards, buttons, HUD, and the menu itself "own" their right-clicks.
        // Anything else is considered the board background.
        if (
          target?.closest(
            "button, a, input, select, textarea, [role='menuitem'], [role='menu'], [data-card-hover], [data-card-preview], [data-context-menu-ignore]",
          )
        ) {
          return;
        }
        setBoardContextMenu({ x: e.clientX, y: e.clientY });
      }}
    >
      <BattlefieldBackground />
      <StackDisplay />

      {/* Reconnecting banner */}
      {reconnectState.status === "reconnecting" && (
        <div className="fixed left-0 right-0 top-0 z-40 bg-amber-600 px-4 py-2 text-center text-sm font-semibold text-white">
          Reconnecting…{" "}
          {reconnectState.maxAttempts > 0
            ? `(attempt ${reconnectState.attempt}/${reconnectState.maxAttempts})`
            : `(attempt ${reconnectState.attempt})`}
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
          {/* Opponent HUD — bottom-center of opp container */}
          <div className="pointer-events-none absolute bottom-0 left-0 right-0 z-20 flex justify-center" data-debug-label="Opp HUD">
            <div className="pointer-events-auto">
              <OpponentHud
                opponentName={isOnlineMode ? opponentDisplayName : undefined}
                onKickPlayer={
                  isP2PHost
                    ? (pid) => {
                        // The host adapter is exposed via the game store.
                        // We type-cast to the host-specific surface; if the
                        // game isn't actually a P2P host (mode mismatch), the
                        // optional method call is a no-op.
                        const adapter = useGameStore.getState().adapter as
                          | { kickPlayer?: (pid: number) => Promise<void> }
                          | null;
                        void adapter?.kickPlayer?.(pid);
                      }
                    : undefined
                }
              />
            </div>
          </div>
          <div
            className="pointer-events-none absolute right-0 top-0 z-10 flex w-fit flex-col items-end gap-2 px-2 py-1 [&>*]:pointer-events-auto [&>div>*]:pointer-events-auto"
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
            <div className="relative z-10">
              <ZoneIndicator
                zone="exile"
                playerId={activeOpponentId}
                onClick={() =>
                  setViewingZone({ zone: "exile", playerId: activeOpponentId })
                }
              />
            </div>
          </div>
        </div>

        {/* Battlefield */}
        <div className="relative z-10 flex min-h-0 flex-1 flex-col" data-debug-label="Battlefield">
          <GameBoard />
        </div>

        {/* Player hand + zones at bottom — negative margin pushes hand content
             below viewport edge so cards peek from the bottom (clipped by page root overflow-hidden).
             Zones are anchored to top-0 so they stay in the visible area. */}
        <div className="relative shrink-0 pt-4 mb-[calc(var(--card-h)*-0.25)] sm:mb-[calc(var(--card-h)*-0.25)] md:mb-[calc(var(--card-h)*-0.35)] [@media(max-height:500px)]:!mb-0 [@media(max-height:500px)]:!pt-1" data-debug-label="Player Bottom">
          {/* Player HUD — top-center of player bottom container */}
          <div className="pointer-events-none absolute top-0 left-0 right-0 z-20 flex justify-center" data-debug-label="Player HUD">
            <div className="pointer-events-auto">
              <PlayerHud />
            </div>
          </div>
          <div className="flex items-end justify-center">
            <ZoneHand zone="exile" />
            <PlayerHand />
            <ZoneHand zone="graveyard" />
          </div>
          <div
            className="pointer-events-none absolute left-0 top-0 bottom-[calc(var(--card-h)*0.25)] sm:bottom-[calc(var(--card-h)*0.25)] md:bottom-[calc(var(--card-h)*0.35)] [@media(max-height:500px)]:!bottom-0 z-10 flex w-fit flex-col items-start justify-end gap-0.5 p-1 lg:gap-1 lg:p-3 [&>*]:pointer-events-auto [&>div>*]:pointer-events-auto"
            style={playerZoneRailStyle}
            data-debug-label="Player Zones"
          >
            <div className="relative z-10">
              <ZoneIndicator
                zone="exile"
                playerId={perspectivePlayerId}
                onClick={() => setViewingZone({ zone: "exile", playerId: perspectivePlayerId })}
              />
            </div>
            <div className="flex items-end gap-2">
              <GraveyardPile
                playerId={perspectivePlayerId}
                onClick={() => setViewingZone({ zone: "graveyard", playerId: perspectivePlayerId })}
              />
              <LibraryPile playerId={perspectivePlayerId} />
            </div>
          </div>
          {/* Companion zone — right side, Arena-style */}
          <div
            className="pointer-events-none absolute right-0 top-0 bottom-[calc(var(--card-h)*0.15)] sm:bottom-[calc(var(--card-h)*0.25)] md:bottom-[calc(var(--card-h)*0.35)] [@media(max-height:500px)]:!bottom-0 z-10 flex w-fit flex-col items-end justify-end gap-0.5 p-1 lg:gap-1 lg:p-3 [&>*]:pointer-events-auto"
            style={playerZoneRailStyle}
          >
            <CompanionZone playerId={perspectivePlayerId} />
          </div>
        </div>
      </div>

      {/* Opponent zones are now inline in the Opp Top row above */}

      {/* Right-side fixed UI stack: combat phases → full control → action buttons → log */}
      <div
        className="fixed z-30 flex flex-col items-end gap-1.5"
        style={{
          bottom: "calc(env(safe-area-inset-bottom) + var(--action-btn-bottom))",
          right: "calc(env(safe-area-inset-right) + var(--game-edge-right) + var(--game-right-rail-offset, 0px))",
        }}
      >
        <CombatPhaseIndicator />
        <FullControlToggle />
        <ActionButton />
      </div>

      <GameLogPanel />
      <MobileHandDrawer />

      {/* Game menu — top-left hamburger */}
      <GameMenu
        gameId={gameId}
        isAiMode={mode === "ai"}
        isOnlineMode={isOnlineMode}
        showAiHand={showAiHand}
        onToggleAiHand={() => setShowAiHand((v) => !v)}
        onSettingsClick={() => setPreferencesOpen({})}
        onConcede={onShowConcedeDialog}
      />

      {/* Connection failure toast */}
      {isOnlineMode && (
        <ConnectionToast
          onRetry={() => window.location.reload()}
          onSettings={() => setPreferencesOpen({})}
        />
      )}


      {/*
        Opponent-disconnected overlay for server (WS) games. The live
        "N seconds to forfeit" countdown lives on `ConnectionToast`, keyed
        by player — this modal just communicates the blocking/paused state
        of the game screen. P2P games use `DisconnectChoiceDialog` +
        `PausedBanner` instead (see adapter §4).
      */}
      {opponentDisconnected && !pauseReason && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div className="absolute inset-0 bg-black/60" />
          <div className="relative z-10 w-full max-w-sm rounded-[24px] border border-yellow-400/30 bg-[#0b1020]/96 p-6 text-center shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md">
            <h2 className="mb-2 text-lg font-bold text-yellow-400">
              Opponent Disconnected
            </h2>
            <p className="text-sm text-gray-300">
              Waiting for opponent to reconnect...
            </p>
          </div>
        </div>
      )}

      {/* P2P pause banner — visible to everyone while paused. */}
      <PausedBanner isVisible={pauseReason !== null} reason={pauseReason ?? ""} />

      {/* P2P host-only disconnect decision modal. */}
      {isP2PHost && disconnectChoice !== null && (
        <DisconnectChoiceDialog
          isOpen
          playerLabel={getOpponentDisplayName(disconnectChoice.playerId)}
          gracePeriodMs={disconnectChoice.gracePeriodMs}
          onPauseAndWait={() => {
            const adapter = useGameStore.getState().adapter as
              | { holdForReconnect?: (pid: number) => void }
              | null;
            adapter?.holdForReconnect?.(disconnectChoice.playerId);
          }}
          onContinueWithout={() => {
            const adapter = useGameStore.getState().adapter as
              | { concedeDisconnected?: (pid: number) => Promise<void> }
              | null;
            void adapter?.concedeDisconnected?.(disconnectChoice.playerId);
          }}
          onDismiss={onDismissDisconnectChoice}
        />
      )}

      {/* Pre-game lobby progress (3-4p P2P only). */}
      {lobbyProgress !== null && (
        <LobbyProgress
          joined={lobbyProgress.joined}
          total={lobbyProgress.total}
          roomCode={hostGameCode ?? undefined}
        />
      )}

      {/* Card data missing modal */}
      {showCardDataMissing && (
        <CardDataMissingModal onContinue={onDismissCardDataMissing} />
      )}

      {/* Resume-failed banner */}
      <AnimatePresence>
        {resumeResetReason && (
          <motion.div
            className="fixed top-4 left-1/2 z-50 flex -translate-x-1/2 items-center gap-3 rounded-lg bg-amber-950 px-4 py-3 shadow-2xl ring-1 ring-amber-700/50"
            initial={{ opacity: 0, y: -20 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0, y: -20 }}
            transition={{ duration: 0.25 }}
          >
            <span className="text-sm text-amber-200">{resumeResetReason} A new game was started.</span>
            <button
              onClick={onDismissResumeReset}
              className="rounded bg-amber-800 px-2.5 py-1 text-xs font-semibold text-amber-100 transition hover:bg-amber-700"
            >
              OK
            </button>
          </motion.div>
        )}
      </AnimatePresence>

      {/* Overlay layers */}
      <DebugPanel />

      {viewingZone && (
        <ZoneViewer
          zone={viewingZone.zone}
          playerId={viewingZone.playerId}
          onClose={() => setViewingZone(null)}
        />
      )}

      {preferencesOpen && (
        <PreferencesModal
          onClose={() => setPreferencesOpen(null)}
          initialTab={preferencesOpen.tab}
          highlight={preferencesOpen.highlight}
        />
      )}

      {boardContextMenu && (
        <BoardContextMenu
          x={boardContextMenu.x}
          y={boardContextMenu.y}
          onClose={() => setBoardContextMenu(null)}
          onChangeBackground={() =>
            setPreferencesOpen({ tab: "gameplay", highlight: "board-background" })
          }
          onToggleGameLog={() => useUiStore.getState().toggleLogPanel()}
          onToggleDebugLog={() => useUiStore.getState().toggleDebugPanel()}
        />
      )}

      {/* Animation overlay (above board, below modals) */}
      <AnimationOverlay containerRef={containerRef} />
      <TurnBanner />

      {/* Combat SVG overlays: blocker assignments + attack target arrows */}
      <BlockAssignmentLines />
      <AttackTargetLines />

      {/* Card preview overlay */}
      <CardPreview cardName={inspectedCardName} backFaceName={inspectedOtherFaceName} />

      {/* WaitingFor-driven prompt overlays (only for human player).
          Wrapped in DialogHost so any active dialog can be peeked away to
          reveal the battlefield underneath; peek state resets on every
          new WaitingFor so a fresh prompt is always visible. */}
      <DialogHost>
        {waitingFor != null &&
          (["TargetSelection", "TriggerTargetSelection", "CopyTargetChoice", "ExploreChoice", "TapCreaturesForManaAbility", "TapCreaturesForSpellCost"] as const).includes(waitingFor.type as never) &&
          canActForWaitingState && <TargetingOverlay />}
        {waitingFor?.type === "ManaPayment" &&
          canActForWaitingState && <ManaPaymentUI />}
        {waitingFor?.type === "ChooseXValue" &&
          canActForWaitingState && <ChooseXValueUI />}
        {waitingFor?.type === "ReplacementChoice" &&
          canActForWaitingState && <ReplacementModal />}
        <BattleProtectorModal />
        <TributeModal />
        <CombatTaxModal />
        <ModeChoiceModal />
        <AdventureCastModal />
        <CascadeChoiceModal />
        <ModalFaceModal />
        <WarpCostModal />
        <MiracleRevealModal />

        {/* Scry/Dig/Surveil card choice modal */}
        <CardChoiceModal />

        {/* Ability choice picker (planeswalkers, multi-ability permanents) */}
        <AbilityChoiceModal />

        {/* Optional additional cost choice (kicker, blight, "or pay") */}
        {waitingFor?.type === "OptionalCostChoice" &&
          canActForWaitingState && (
            <OptionalCostModal />
          )}

        {/* Defiler cycle — optional life payment for mana reduction */}
        {waitingFor?.type === "DefilerPayment" &&
          canActForWaitingState && (
            <DefilerPaymentModal />
          )}

        {/* Optional effect choice ("You may X") / Opponent may choice */}
        {(waitingFor?.type === "OptionalEffectChoice" || waitingFor?.type === "OpponentMayChoice") &&
          canActForWaitingState && (
            <OptionalEffectModal />
          )}

        {/* Unless payment choice ("Counter unless you pay {X}") */}
        {waitingFor?.type === "UnlessPayment" &&
          canActForWaitingState && (
            <UnlessPaymentModal />
          )}
      </DialogHost>

      {waitingFor?.type === "CompanionReveal" &&
        waitingFor.data.player === playerId && (
          <CompanionRevealPrompt
            eligibleCompanions={waitingFor.data.eligible_companions}
            onChoose={handleDeclareCompanion}
          />
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
        waitingFor.data.player === playerId &&
        (() => {
          const pool = deckPools?.find((p) => p.player === playerId);
          if (!pool) return null;
          return (
            <BetweenGamesSideboardModal
              pool={pool}
              gameNumber={waitingFor.data.game_number}
              score={waitingFor.data.score}
              onSubmit={handleSubmitSideboard}
            />
          );
        })()}

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
    <div className="fixed inset-0 z-50 overflow-y-auto px-2 py-2 lg:px-4 lg:py-6">
      <div className="absolute inset-0 bg-[radial-gradient(circle_at_top,rgba(31,41,55,0.55),rgba(2,6,23,0.92)_58%,rgba(2,6,23,0.98))]" />
      <div className="relative flex min-h-full items-center justify-center pb-[env(safe-area-inset-bottom)] pt-[env(safe-area-inset-top)]">
        <motion.div
          className="card-scale-reset relative z-10 flex w-full max-w-6xl flex-col overflow-hidden rounded-[14px] lg:rounded-[28px] border border-white/10 bg-[#0b1020]/94 shadow-[0_32px_90px_rgba(0,0,0,0.48)] backdrop-blur-md"
          initial={{ opacity: 0, y: 18, scale: 0.98 }}
          animate={{ opacity: 1, y: 0, scale: 1 }}
          transition={{ duration: 0.24, ease: "easeOut" }}
        >
          <div className="modal-header-compact border-b border-white/10">
            <div className="modal-eyebrow uppercase tracking-[0.24em] text-slate-500">
              {eyebrow}
            </div>
            <h2 className="font-semibold text-white">
              {title}
            </h2>
            <p className="modal-subtitle max-w-2xl text-slate-400">
              {subtitle}
            </p>
          </div>

          <div className="flex flex-1 flex-col px-2 py-2 lg:px-5 lg:py-5">{children}</div>

          {footer && (
            <div className="border-t border-white/10 bg-black/15 px-3 py-2 lg:px-6 lg:py-4">
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
  const hoverProps = useInspectHoverProps();
  const [buttonsVisible, setButtonsVisible] = useState(false);

  if (!player || !objects) {
    return (
      <ChoiceModal
        title={`London Mulligan (${mulliganCount} taken)`}
        options={[
          {
            id: "keep",
            label: "Keep Hand",
            description:
              mulliganCount > 0
                ? `Put ${mulliganCount} on the bottom`
                : "No cards to the bottom",
          },
          {
            id: "mulligan",
            label: "Mulligan",
            description: "Shuffle and draw 7 again",
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
      eyebrow={mulliganCount > 0 ? `Mulligan ${mulliganCount} · London` : "Opening Hand · London Mulligan"}
      title="Review your opening hand"
      subtitle={
        mulliganCount > 0
          ? `Keep this hand (you'll put ${mulliganCount} on the bottom) or mulligan again for a fresh 7.`
          : "Keep this hand or mulligan for a fresh 7 (you'll put 1 on the bottom when you keep)."
      }
      footer={
        <AnimatePresence>
          {buttonsVisible && (
            <motion.div
              className="flex w-full flex-row items-center justify-end gap-2 lg:gap-3"
              initial={{ opacity: 0, y: 20 }}
              animate={{ opacity: 1, y: 0 }}
              transition={{ duration: 0.22 }}
            >
              <button
                onClick={() => onChoose("mulligan")}
                className="rounded-[10px] border border-white/12 bg-white/5 px-3 py-1.5 text-xs font-semibold text-slate-200 transition hover:bg-white/8 hover:text-white lg:min-h-11 lg:rounded-[16px] lg:px-5 lg:py-3 lg:text-base"
              >
                Mulligan to {nextHandSize}
              </button>
              <button
                onClick={() => onChoose("keep")}
                className="rounded-[10px] bg-cyan-500 px-3 py-1.5 text-xs font-semibold text-slate-950 shadow-[0_14px_34px_rgba(6,182,212,0.28)] transition hover:bg-cyan-400 lg:min-h-11 lg:rounded-[16px] lg:px-5 lg:py-3 lg:text-base"
              >
                Keep Hand
              </button>
            </motion.div>
          )}
        </AnimatePresence>
      }
    >
      <div
        className="modal-card-area flex min-h-0 flex-1 items-center justify-center"
        style={
          {
            "--card-w": "clamp(100px, 14vw, 180px)",
            "--card-h": "clamp(140px, 19.6vw, 252px)",
          } as React.CSSProperties
        }
      >
        <div className="w-full overflow-x-auto">
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
                {...hoverProps(obj.id)}
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

interface CompanionRevealPromptProps {
  eligibleCompanions: [string, number][];
  onChoose: (cardIndex: number | null) => void;
}

function CompanionRevealPrompt({
  eligibleCompanions,
  onChoose,
}: CompanionRevealPromptProps) {
  const [buttonsVisible, setButtonsVisible] = useState(
    eligibleCompanions.length === 0,
  );

  return (
    <MulliganPanel
      eyebrow="Pre-Game"
      title="Reveal Companion?"
      subtitle="You may reveal a companion from your sideboard. It will be placed in the companion zone and can be put into your hand once during the game by paying {3}."
      footer={
        <AnimatePresence>
          {buttonsVisible && (
            <motion.div
              className="flex w-full flex-row items-center justify-end gap-2 lg:gap-3"
              initial={{ opacity: 0, y: 20 }}
              animate={{ opacity: 1, y: 0 }}
              transition={{ duration: 0.22 }}
            >
              <button
                onClick={() => onChoose(null)}
                className="rounded-[10px] border border-white/12 bg-white/5 px-3 py-1.5 text-xs font-semibold text-slate-200 transition hover:bg-white/8 hover:text-white lg:min-h-11 lg:rounded-[16px] lg:px-5 lg:py-3 lg:text-base"
              >
                Decline
              </button>
              {eligibleCompanions.map(([name], i) => (
                <button
                  key={name}
                  onClick={() => onChoose(i)}
                  className="min-h-11 rounded-[16px] bg-amber-500 px-5 py-3 text-sm font-semibold text-slate-950 shadow-[0_14px_34px_rgba(245,158,11,0.28)] transition hover:bg-amber-400 sm:text-base"
                >
                  Reveal {name}
                </button>
              ))}
            </motion.div>
          )}
        </AnimatePresence>
      }
    >
      <div
        className="modal-card-area flex min-h-0 flex-1 items-center justify-center"
        style={
          {
            "--card-w": "clamp(100px, 14vw, 180px)",
            "--card-h": "clamp(140px, 19.6vw, 252px)",
          } as React.CSSProperties
        }
      >
        <div className="w-full overflow-x-auto">
          <div className="mx-auto flex w-max min-w-full items-center justify-center px-2 sm:px-4">
            {eligibleCompanions.map(([name], index) => (
              <motion.div
                key={name}
                className="flex-shrink-0 rounded-[18px] transition-shadow duration-200 hover:z-50 hover:shadow-[0_0_24px_rgba(245,158,11,0.22)]"
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
                  if (index === eligibleCompanions.length - 1)
                    setButtonsVisible(true);
                }}
              >
                <CardImage
                  cardName={name}
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
  const hoverProps = useInspectHoverProps();

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
        className="modal-card-area flex min-h-0 flex-1 items-center justify-center"
        style={
          {
            "--card-w": "clamp(100px, 14vw, 180px)",
            "--card-h": "clamp(140px, 19.6vw, 252px)",
          } as React.CSSProperties
        }
      >
        <div className="w-full overflow-x-auto">
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
                  {...hoverProps(obj.id)}
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
  const objects = useGameStore((s) => s.gameState?.objects);

  if (!pending || !obj) return null;

  // CR 702.190a: When every pending action is a Sneak cast, reframe the
  // modal's subtitle — the user is choosing which attacker to return as the
  // cost-payment creature, not activating an ability.
  const allSneak = pending.actions.every((a) => a.type === "CastSpellAsSneak");
  const subtitle = allSneak
    ? "Choose which attacker to return (Sneak cost)"
    : "Choose an ability to activate";

  return (
    <ChoiceModal
      title={obj.name}
      subtitle={subtitle}
      previewCardName={obj.name}
      options={pending.actions.map((action, i) => {
        const { label, description } = abilityChoiceLabel(
          action,
          obj,
          objects,
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

// ── Defiler Payment Modal ────────────────────────────────────────────

function DefilerPaymentModal() {
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);

  if (waitingFor?.type !== "DefilerPayment") return null;

  const { life_cost } = waitingFor.data;

  return (
    <ChoiceModal
      title="Defiler Cost Reduction"
      subtitle={`Pay ${life_cost} life to reduce the mana cost?`}
      options={[
        { id: "pay", label: `Pay ${life_cost} life` },
        { id: "skip", label: "Decline" },
      ]}
      onChoose={(id) =>
        dispatch({ type: "DecideOptionalCost", data: { pay: id === "pay" } })
      }
      onClose={() => dispatch({ type: "CancelCast" })}
    />
  );
}

// ── Optional Effect Choice Modal ────────────────────────────────────────

function OptionalEffectModal() {
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);
  const objects = useGameStore((s) => s.gameState?.objects);

  if (waitingFor?.type !== "OptionalEffectChoice" && waitingFor?.type !== "OpponentMayChoice") return null;

  const sourceObj = objects?.[waitingFor.data.source_id];
  const sourceName = sourceObj?.name ?? "Effect";
  const description = waitingFor.data.description as string | undefined;

  return (
    <ChoiceModal
      title={`${sourceName} — Optional Effect`}
      subtitle={description}
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

function formatUnlessCost(cost: { type: string; cost?: { type: string; shards?: string[]; generic?: number }; amount?: number }): string {
  switch (cost.type) {
    case "Fixed":
      return cost.cost ? formatManaCost(cost.cost) : "0";
    case "PayLife":
      return `${cost.amount ?? 0} life`;
    case "DiscardCard":
      return "discard a card";
    case "Sacrifice": {
      const n = (cost as { count?: number }).count ?? 1;
      return n > 1 ? `sacrifice ${n} permanents` : "sacrifice a permanent";
    }
    default:
      return "a cost";
  }
}

function UnlessPaymentModal() {
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);

  if (waitingFor?.type !== "UnlessPayment") return null;

  const costDisplay = formatUnlessCost(waitingFor.data.cost);
  const description = waitingFor.data.effect_description ?? "Counter Unless You Pay";
  const title = description.charAt(0).toUpperCase() + description.slice(1);

  return (
    <ChoiceModal
      title={`${title} Unless You Pay`}
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
