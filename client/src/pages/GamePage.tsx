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
import { Trans, useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

import type { DeckCardCount, GameFormat, MatchConfig, ObjectId, SerializedAbilityCost } from "../adapter/types";
import { useDraftStore } from "../stores/draftStore";
import { loadActiveQuickDraft } from "../services/quickDraftPersistence";
import type { DraftMatchResult } from "../services/quickDraftPersistence";
import { useResolvedGridRows, useResolvedSplitGridRows } from "../hooks/useResolvedGridRows.ts";
import { useIsMobile } from "../hooks/useIsMobile.ts";
import { FlexEditOverlay } from "../components/flexlayout/FlexEditOverlay.tsx";
import { DraggableWidget } from "../components/flexlayout/DraggableWidget.tsx";
import { BetweenGamesSideboardModal } from "../components/multiplayer/BetweenGamesSideboardModal.tsx";
import { audioManager } from "../audio/AudioManager.ts";
import { useAudioContext } from "../audio/useAudioContext.ts";
import { AnimationOverlay } from "../components/animation/AnimationOverlay.tsx";
import { RevealOverlay } from "../components/animation/RevealOverlay.tsx";
import { TurnBanner } from "../components/animation/TurnBanner.tsx";
import { DiceRollOverlay } from "../components/animation/DiceRollOverlay.tsx";
import { flashStartingPlayerContest } from "../game/diceContest.ts";
import { BattlefieldBackground } from "../components/board/BattlefieldBackground.tsx";
import { BoardContextMenu } from "../components/board/BoardContextMenu.tsx";
import { DebugCardContextMenu } from "../components/chrome/DebugCardContextMenu.tsx";
import { DebugLibraryViewer } from "../components/chrome/DebugLibraryViewer.tsx";
import { AttackTargetLines } from "../components/board/AttackTargetLines.tsx";
import { BlockAssignmentLines } from "../components/board/BlockAssignmentLines.tsx";
import { BlockRequirementBadges } from "../components/combat/BlockRequirementBadges.tsx";
import { GameBoard } from "../components/board/GameBoard.tsx";
import { CardImage } from "../components/card/CardImage.tsx";
import { GameCardPreview } from "../components/card/GameCardPreview.tsx";
import { CardReportDialog } from "../components/card/CardReportDialog.tsx";
import { ActionButton } from "../components/board/ActionButton.tsx";
import { FullControlToggle } from "../components/controls/FullControlToggle.tsx";
import { CombatPhaseIndicator } from "../components/controls/PhaseStopBar.tsx";
import { PriorityYieldList } from "../components/board/PriorityYieldList.tsx";
import { OpponentHand } from "../components/hand/OpponentHand.tsx";
import { MobileHandDrawer } from "../components/hand/MobileHandDrawer.tsx";
import { HandBadge } from "../components/hand/HandBadge.tsx";
import { PlayerHand } from "../components/hand/PlayerHand.tsx";
import { FlowHelpNudge } from "../components/help/FlowHelpNudge.tsx";
import { ReportCardNudge } from "../components/help/ReportCardNudge.tsx";
import { SandboxToolsNudge } from "../components/help/SandboxToolsNudge.tsx";
import { HelpSheet } from "../components/help/HelpSheet.tsx";
import { GameLogPanel } from "../components/log/GameLogPanel.tsx";
import { ChooseXValueUI } from "../components/mana/ChooseXValueUI.tsx";
import { AssistPaymentUI } from "../components/mana/AssistPaymentUI.tsx";
import { ManaPaymentUI } from "../components/mana/ManaPaymentUI.tsx";
import { PayAmountChoiceUI } from "../components/mana/PayAmountChoiceUI.tsx";
import { RichLabel } from "../components/mana/RichLabel.tsx";
import { CardDataMissingModal } from "../components/modal/CardDataMissingModal.tsx";
import { UnhandledWaitingForModal } from "../components/modal/UnhandledWaitingForModal.tsx";
import { AdventureCastModal } from "../components/modal/AdventureCastModal.tsx";
import { CascadeChoiceModal } from "../components/modal/CascadeChoiceModal.tsx";
import { FreeCastWindowModal } from "../components/modal/FreeCastWindowModal.tsx";
import { ModalFaceModal } from "../components/modal/ModalFaceModal.tsx";
import { AlternativeCostModal } from "../components/modal/AlternativeCostModal.tsx";
import { CastingVariantModal } from "../components/modal/CastingVariantModal.tsx";
import { MiracleRevealModal } from "../components/modal/MiracleRevealModal.tsx";
import { SpliceOfferModal } from "../components/modal/SpliceOfferModal.tsx";
import { CardChoiceModal } from "../components/modal/CardChoiceModal.tsx";
import { ChoiceModal } from "../components/modal/ChoiceModal.tsx";
import { OptionalEffectModalContent } from "../components/modal/OptionalEffectModal.tsx";
import { OptionalCostModalContent } from "../components/modal/OptionalCostModal.tsx";
import { ChooseOneOfBranchModal } from "../components/modal/ChooseOneOfBranchModal.tsx";
import { LifeRedistributionModal } from "../components/modal/LifeRedistributionModal.tsx";
import { ModeChoiceModal } from "../components/modal/ModeChoiceModal.tsx";
import { ReplacementModal } from "../components/modal/ReplacementModal.tsx";
import { TriggerOrderModal } from "../components/modal/TriggerOrderModal.tsx";
import { PeekTab } from "../components/modal/DialogShell.tsx";
import { PeekRestoreTab } from "../components/modal/DialogHost.tsx";
import { useModalPeek } from "../components/modal/useModalPeek.ts";
import { BattleProtectorModal } from "../components/modal/BattleProtectorModal.tsx";
import { AssistChoosePlayerModal } from "../components/modal/AssistChoosePlayerModal.tsx";
import { ClashOpponentModal } from "../components/modal/ClashOpponentModal.tsx";
import { TributeModal } from "../components/modal/TributeModal.tsx";
import { CombatTaxModal } from "../components/modal/CombatTaxModal.tsx";
import { TopOrBottomChoiceModalContent } from "../components/modal/TopOrBottomChoiceModal.tsx";
import { MutateMergeChoiceModalContent } from "../components/modal/MutateMergeChoiceModal.tsx";
import { CipherEncodeChoiceModalContent } from "../components/modal/CipherEncodeChoiceModal.tsx";
import { DialogHost, isClickThroughWaitingFor } from "../components/modal/DialogHost.tsx";
import { PermanentTypeSlotModal } from "../components/modal/PermanentTypeSlotModal.tsx";
import { StackDisplay } from "../components/stack/StackDisplay.tsx";
import { TargetingOverlay } from "../components/targeting/TargetingOverlay.tsx";
import { PlayerHud } from "../components/hud/PlayerHud.tsx";
import { OpponentHud } from "../components/hud/OpponentHud.tsx";
import { TurnStatusLine } from "../components/hud/TurnStatusLine.tsx";
import { GraveyardPile } from "../components/zone/GraveyardPile.tsx";
import { LibraryPile } from "../components/zone/LibraryPile.tsx";
import { ExilePile } from "../components/zone/ExilePile.tsx";
import { CompanionZone } from "../components/zone/CompanionZone.tsx";
import { ZoneViewer } from "../components/zone/ZoneViewer.tsx";
import {
  PreferencesModal,
  type SettingsHighlight,
  type SettingsTabId,
} from "../components/settings/PreferencesModal.tsx";
import { DebugPanel } from "../components/chrome/DebugPanel.tsx";
import { GameMenu } from "../components/chrome/GameMenu.tsx";
import { ConcedeDialog } from "../components/multiplayer/ConcedeDialog.tsx";
import { TakebackRequestDialog } from "../components/multiplayer/TakebackRequestDialog.tsx";
import { ConnectionToast } from "../components/multiplayer/ConnectionToast.tsx";
import { EmoteOverlay } from "../components/multiplayer/EmoteOverlay.tsx";
import { ResolutionProgressOverlay } from "../components/board/ResolutionProgressOverlay.tsx";
import { LobbyProgress } from "../components/multiplayer/LobbyProgress.tsx";
import { DisconnectChoiceDialog } from "../components/hud/DisconnectChoiceDialog.tsx";
import { PlayerEnchantmentsDialog } from "../components/hud/PlayerEnchantmentsDialog.tsx";
import { AttachmentFan } from "../components/board/AttachmentFan.tsx";
import { PausedBanner } from "../components/chrome/PausedBanner.tsx";
import type { P2PAdapterEvent } from "../adapter/p2p-adapter.ts";
import { WebSocketAdapter } from "../adapter/ws-adapter.ts";
import type { WsAdapterEvent } from "../adapter/ws-adapter.ts";
import { MANA_PAYMENT_WAITING_FOR_TYPES } from "../game/waitingForRegistry.ts";
import { useGameDispatch } from "../hooks/useGameDispatch.ts";
import { useInspectHoverProps } from "../hooks/useInspectHoverProps.ts";
import { useKeyboardShortcuts } from "../hooks/useKeyboardShortcuts.ts";
import { clearPromptOverlayState } from "../game/sessionCleanup.ts";
import { clearGame, loadActiveGame, useGameStore } from "../stores/gameStore.ts";
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
import { formatMetadata, isSetupFormat } from "../data/formatRegistry.ts";
import { useMultiplayerDraftStore } from "../stores/multiplayerDraftStore.ts";
import { SpectatorChrome } from "../components/spectator/SpectatorChrome.tsx";
import { useSpectatorMode } from "../hooks/useSpectatorMode.ts";
import { GameProvider } from "../providers/GameProvider.tsx";
import { useCanActForWaitingState, usePerspectivePlayerId, usePlayerId } from "../hooks/usePlayerId.ts";
import {
  abilityChoiceLabel,
  formatAbilityCost,
  loyaltyBadge,
  stripLoyaltyCostPrefix,
} from "../viewmodel/costLabel.ts";
import { ManaFontIcon } from "../components/icons/ManaFontIcon.tsx";
import {
  getCastableZoneViewerTarget,
  getBoardChoiceView,
  getOpponentIds,
  getSeatCount,
  getWaitingForObjectChoiceIds,
  isSplitBoardActive,
  resolveFocusedOpponent,
  shouldRenderFocusedOpponentTopRow,
  type ZoneViewerTarget,
} from "../viewmodel/gameStateView.ts";
import { gameButtonClass } from "../components/ui/buttonStyles.ts";
import { GAME_Z_LAYER } from "../constants/ui.ts";

type ZoneRailStyle = CSSProperties & {
  "--card-w": string;
  "--card-h": string;
};

function castableZoneViewerAutoOpenKey(target: ZoneViewerTarget): string {
  return `${target.zone}:${target.playerId}:${target.objectIds.join(",")}`;
}

function isDirectSoloRouteMode(rawMode: string | null): boolean {
  return ![
    "p2p-host",
    "p2p-join",
    "draft-match",
    "spectate",
    "host",
    "join",
  ].includes(rawMode ?? "");
}

function isDirectSetupFormat(format: GameFormat): boolean {
  const metadata = formatMetadata(format);
  return Boolean(metadata && isSetupFormat(metadata));
}

function directSetupFormatConfig(format: GameFormat | null) {
  if (!format) return undefined;
  if (!isDirectSetupFormat(format)) return undefined;
  return FORMAT_DEFAULTS[format];
}

/**
 * i18n keys for user-facing messages keyed by
 * `P2PAdapterEvent.hostingFailed.reason`. Typed as `Record<ReasonUnion, string>`
 * so adding a new reason to the adapter event union without adding a key here is
 * a compile error — the idiomatic TS replacement for a `switch`-with-`never`-default
 * when the union has a single arm today but will grow.
 */
const HOSTING_FAILURE_MESSAGE_KEYS: Record<
  Extract<P2PAdapterEvent, { type: "hostingFailed" }>["reason"],
  string
> = {
  room_still_claimed: "gamePage.toasts.roomStillClaimed",
};

export function GamePage() {
  const { t } = useTranslation("game");
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
  const loopParam = searchParams.get("loop");
  const firstParam = searchParams.get("first");
  const roomNameParam = searchParams.get("roomName");
  const sourceParam = searchParams.get("source") ?? undefined;
  const draftIdParam = searchParams.get("draftId") ?? undefined;
  const playerCount = playersParam ? Number(playersParam) : undefined;
  const activeGameMeta = useMemo(
    () => (gameId ? loadActiveGame() : null),
    [gameId],
  );
  const savedFormatConfig =
    activeGameMeta && activeGameMeta.id === gameId
      ? activeGameMeta.formatConfig
      : undefined;
  // Memoize so the `GameProvider` `useEffect` dep array doesn't
  // tear-down/rebuild the P2P session on every parent re-render. Without
  // `useMemo`, each render constructs a fresh object reference from
  // `FORMAT_DEFAULTS[formatParam]` (its lookup returns a stable reference,
  // but TypeScript's narrowing produces a fresh binding that the linter
  // treats as new). The explicit memo makes the stability guarantee
  // self-documenting.
  const formatConfig = useMemo(() => {
    if (isDirectSoloRouteMode(rawMode)) {
      if (savedFormatConfig && isDirectSetupFormat(savedFormatConfig.format)) {
        return savedFormatConfig;
      }
      return directSetupFormatConfig(formatParam);
    }
    return savedFormatConfig ?? (formatParam ? FORMAT_DEFAULTS[formatParam] : undefined);
  }, [formatParam, rawMode, savedFormatConfig]);
  // CR 103.1: 0 = play first, 1 = draw first, undefined = random
  const firstPlayer = firstParam === "play" ? 0 : firstParam === "draw" ? 1 : undefined;
  const matchConfig = useMemo<MatchConfig>(
    () => ({
      match_type: matchParam?.toLowerCase() === "bo3" ? "Bo3" : "Bo1",
      // CR 732.2a: combo (infinite-loop) detector opt-in carried from the local
      // game-setup screen; immutable once the game starts (engine default Off).
      loop_detection: loopParam?.toLowerCase() === "on" ? { type: "On" } : { type: "Off" },
    }),
    [matchParam, loopParam],
  );

  // Map URL modes to GameProvider modes
  const mode: "ai" | "online" | "local" | "p2p-host" | "p2p-join" | "draft-match" | "spectate" =
    rawMode === "p2p-host"
      ? "p2p-host"
      : rawMode === "p2p-join"
        ? "p2p-join"
        : rawMode === "draft-match"
          ? "draft-match"
          : rawMode === "spectate"
            ? "spectate"
            : rawMode === "host" || rawMode === "join"
              ? "online"
              : rawMode === "ai"
                ? "ai"
                : "local";

  const [showCardDataMissing, setShowCardDataMissing] = useState(false);

  // cEDH bracket-violation blocking modal: set when the engine rejects a game
  // init because one or more decks are not declared cEDH at a cEDH table.
  const [bracketViolationError, setBracketViolationError] = useState<string | null>(null);

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
  // GH #1507: "request takeback" — the table-wide pending request, if any.
  const [pendingTakeback, setPendingTakeback] = useState<
    { requester: number; requesterName: string } | null
  >(null);

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
        useMultiplayerStore.getState().showToast(
          t("gamePage.toasts.playerDisconnected", { name: getOpponentDisplayName(oppId) }),
          {
            countdownSeconds: event.graceSeconds,
            key: playerToastKey(oppId),
          },
        );
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
          clearPromptOverlayState();
          if (gameId) void clearGame(gameId);
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
      case "takebackRequested":
        setPendingTakeback({
          requester: event.requester,
          requesterName: event.requesterName,
        });
        break;
      case "takebackResolved":
        setPendingTakeback(null);
        if (!event.approved) {
          useMultiplayerStore.getState().showToast(t("multiplayer:takebackDialog.declinedToast"));
        }
        break;
      case "playerDisconnected":
        // Multiplayer (3+ players): a specific player disconnected
        setOpponentDisconnected(true);
        useMultiplayerStore.getState().setPlayerDisconnected(event.playerId);
        useMultiplayerStore.getState().showToast(
          t("gamePage.toasts.playerDisconnected", { name: getPlayerDisplayName(event.playerId) }),
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
          t("gamePage.toasts.gamePausedPlayerDisconnected", {
            name: getPlayerDisplayName(event.disconnectedPlayer),
          }),
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
        useMultiplayerStore.setState((s) => ({
          spectators: s.spectators.includes(event.name)
            ? s.spectators
            : [...s.spectators, event.name],
        }));
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
  }, [gameId, navigate, joinCode, t]);

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
        useMultiplayerStore.getState().showToast(t("gamePage.toasts.roomFull"));
        break;
      case "opponentDisconnected":
        setOpponentDisconnected(true);
        break;
      case "opponentDisconnectedWithChoice":
        setDisconnectChoice({
          playerId: event.playerId,
          gracePeriodMs: event.gracePeriodMs,
        });
        setPauseReason(
          t("gamePage.toasts.playerDisconnected", { name: getPlayerDisplayName(event.playerId) }),
        );
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
          t("gamePage.toasts.hostReconnecting", { attempt: event.attempt }),
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
        // `HOSTING_FAILURE_MESSAGE_KEYS` is typed as `Record<ReasonUnion, string>`
        // so adding a new `reason` to the P2PAdapterEvent union without
        // adding a key here is a compile error.
        useMultiplayerStore
          .getState()
          .showToast(t(HOSTING_FAILURE_MESSAGE_KEYS[event.reason]));
        navigate("/");
        break;
      }
    }
  }, [navigate, formatConfig, matchConfig, playerCount, gameId, joinCode, t]);

  const handleReady = useCallback(() => {
    setWaitingForOpponent(false);
  }, []);

  const handleNoDeck = useCallback((reason?: string, bracketViolation?: boolean) => {
    if (reason) {
      // cEDH bracket lock: surface as a blocking modal rather than navigating
      // away, so the user can read the explanation before going back to setup.
      // Match by the typed flag from GameProvider — not by string substring —
      // so a reformatted error message can never silently break this modal.
      if (bracketViolation) {
        setBracketViolationError(reason);
        return;
      }
      navigate("/setup", { state: { setupError: reason } });
      return;
    }
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
      source={sourceParam}
      draftId={draftIdParam}
      onWsEvent={mode === "online" || mode === "spectate" ? handleWsEvent : undefined}
      onP2PEvent={
        mode === "p2p-host" || mode === "p2p-join" ? handleP2PEvent : undefined
      }
      onReady={
        mode === "online" || mode === "spectate" || mode === "p2p-host" || mode === "p2p-join"
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
        isOnlineMode={mode === "online" || mode === "spectate"}
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
        pendingTakeback={pendingTakeback}
        onCloseTakebackDialog={() => setPendingTakeback(null)}
        disconnectChoice={disconnectChoice}
        onDismissDisconnectChoice={() => setDisconnectChoice(null)}
        pauseReason={pauseReason}
        isP2PHost={mode === "p2p-host"}
        bracketViolationError={bracketViolationError}
        onDismissBracketViolation={() => {
          setBracketViolationError(null);
          navigate("/setup");
        }}
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
  pendingTakeback: { requester: number; requesterName: string } | null;
  onCloseTakebackDialog: () => void;
  // 3-4p P2P additions
  disconnectChoice: { playerId: number; gracePeriodMs: number } | null;
  onDismissDisconnectChoice: () => void;
  pauseReason: string | null;
  isP2PHost: boolean;
  /** Set when the engine rejected game init because a deck is not declared cEDH at a cEDH table. */
  bracketViolationError: string | null;
  /** Navigate back to setup and clear the bracket-violation modal. */
  onDismissBracketViolation: () => void;
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
  pendingTakeback,
  onCloseTakebackDialog,
  disconnectChoice,
  onDismissDisconnectChoice,
  pauseReason,
  isP2PHost,
  bracketViolationError,
  onDismissBracketViolation,
}: GamePageContentProps) {
  const { t } = useTranslation("game");
  const navigate = useNavigate();
  const containerRef = useRef<HTMLDivElement>(null);

  const waitingFor = useGameStore((s) => s.waitingFor);
  const lobbyProgress = useGameStore((s) => s.lobbyProgress);
  const dispatch = useGameDispatch();
  const isMobile = useIsMobile();
  const focusedGridTemplateRows = useResolvedGridRows();
  const splitGridTemplateRows = useResolvedSplitGridRows();
  const gameState = useGameStore((s) => s.gameState);
  const objects = useGameStore((s) => s.gameState?.objects);
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
  const turnNumber = useGameStore((s) => s.gameState?.turn_number);
  const engineWaitingFor = useGameStore((s) => s.gameState?.waiting_for);
  const deckPools = useGameStore((s) => s.gameState?.deck_pools);
  const stackLength = useGameStore((s) => s.gameState?.stack.length ?? 0);
  const isSandboxGame = useGameStore(
    (s) => s.gameState?.format_config?.allow_debug_actions === true,
  );

  // CR 103.1: present the starting-player d20 contest once on game load. The
  // store holds it as pure data; this presentation-layer effect drives the dice
  // overlay with the engine's authoritative winner and clears the carrier. The
  // identity-ref latch makes the consume idempotent under React StrictMode's
  // double-invoke and after the clear (the re-run sees `null`).
  const startingContest = useGameStore((s) => s.startingContest);
  const consumedContestRef = useRef<typeof startingContest>(null);
  useEffect(() => {
    if (!startingContest || consumedContestRef.current === startingContest) return;
    consumedContestRef.current = startingContest;
    flashStartingPlayerContest(startingContest.events, startingContest.startingPlayer);
    useGameStore.getState().clearStartingContest();
  }, [startingContest]);
  // CR 103.1 before CR 103.5: the starting-player contest must finish before the
  // mulligan UI appears (the roll determines who's on the play, which precedes
  // drawing opening hands). True from `initGame` setting the carrier through the
  // dice overlay's full life — the store hands `startingContest` off to
  // `uiStore.diceRoll` atomically, so there's no gap. Degrades to `false`
  // immediately for instant speed and explicit play/draw (no contest).
  const startingContestDiceActive = useUiStore(
    (s) => s.diceRoll?.context === "startingPlayer",
  );
  const startingContestActive = startingContest !== null || startingContestDiceActive;
  const [showAiHand, setShowAiHand] = useState(false);
  const [showDebugBounds, setShowDebugBounds] = useState(false);
  const [viewingZone, setViewingZone] = useState<{
    zone: "graveyard" | "exile" | "library";
    playerId: number;
    autoOpenKey?: string;
  } | null>(null);
  const dismissedCastableZoneViewerKeyRef = useRef<string | null>(null);
  const [preferencesOpen, setPreferencesOpen] = useState<
    null | { tab?: SettingsTabId; highlight?: SettingsHighlight }
  >(null);
  const [boardContextMenu, setBoardContextMenu] = useState<{ x: number; y: number } | null>(null);

  const playerId = usePlayerId();
  const perspectivePlayerId = usePerspectivePlayerId();
  const isSpectatorMode = useSpectatorMode();
  // Card-report picker is valid in a live, participating game (never spectate).
  const canReportCard = gameState != null && !isSpectatorMode;
  const canActForWaitingState = useCanActForWaitingState();
  const boardChoiceLayerActive = useMemo(() => {
    const choice = getBoardChoiceView(waitingFor, objects);
    return canActForWaitingState && choice?.player === playerId;
  }, [canActForWaitingState, objects, playerId, waitingFor]);
  const helpSheetOpen = useUiStore((s) => s.helpSheetOpen);
  const setHelpSheetOpen = useUiStore((s) => s.setHelpSheetOpen);
  const dismissedFlowHelpNudge = usePreferencesStore((s) => s.dismissedFlowHelpNudge);
  const dismissedSandboxToolsNudge = usePreferencesStore((s) => s.dismissedSandboxToolsNudge);
  const dismissedReportCardNudge = usePreferencesStore((s) => s.dismissedReportCardNudge);
  const cardReportDialogOpen = useUiStore((s) => s.cardReportDialogOpen);
  const multiplayerBoardLayout = usePreferencesStore((s) => s.multiplayerBoardLayout);
  const setMultiplayerBoardLayout = usePreferencesStore((s) => s.setMultiplayerBoardLayout);
  const debugPanelOpen = useUiStore((s) => s.debugPanelOpen);
  const debugClickModeButtonVisible = useUiStore((s) => s.debugClickModeButtonVisible);
  const toggleDebugClickModeButtonVisible = useUiStore(
    (s) => s.toggleDebugClickModeButtonVisible,
  );
  const opponentDisplayName = useMultiplayerStore((s) => s.opponentDisplayName);
  const adapter = useGameStore((s) => s.adapter);
  const focusedOpponent = useUiStore((s) => s.focusedOpponent);
  const opponents = useMemo(() => {
    return getOpponentIds(gameState, perspectivePlayerId);
  }, [gameState, perspectivePlayerId]);
  const activeOpponentId =
    resolveFocusedOpponent(focusedOpponent, opponents) ?? opponents[0] ?? null;
  const seatCount = getSeatCount(gameState);
  const splitBoardActive = isSplitBoardActive(multiplayerBoardLayout, seatCount);
  const renderFocusedOpponentTopRow = shouldRenderFocusedOpponentTopRow(
    multiplayerBoardLayout,
    seatCount,
  );
  const handleToggleMultiplayerBoardLayout = useCallback(() => {
    setMultiplayerBoardLayout(multiplayerBoardLayout === "split" ? "focused" : "split");
  }, [multiplayerBoardLayout, setMultiplayerBoardLayout]);
  const gridTemplateRows = splitBoardActive ? splitGridTemplateRows : focusedGridTemplateRows;
  const handleKickPlayer = useCallback((pid: number) => {
    const adapter = useGameStore.getState().adapter as
      | { kickPlayer?: (pid: number) => Promise<void> }
      | null;
    void adapter?.kickPlayer?.(pid);
  }, []);

  // Memoize the HUD elements passed to GameBoard. GameBoard is wrapped in
  // React.memo, which shallow-compares props; without stable element
  // references these inline JSX nodes would be new on every GamePageContent
  // render, defeating the memo. Stable refs let GameBoard skip re-rendering
  // when GamePageContent re-renders for reasons that don't touch these props.
  const oppHud = useMemo(
    () => (
      <OpponentHud
        opponentName={isOnlineMode ? opponentDisplayName : undefined}
        splitOverview={splitBoardActive}
        onKickPlayer={isP2PHost ? handleKickPlayer : undefined}
      />
    ),
    [handleKickPlayer, isOnlineMode, opponentDisplayName, isP2PHost, splitBoardActive],
  );
  const playerHud = useMemo(() => <PlayerHud />, []);

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

  // GH #1507: "request takeback" — ask every other human player to approve
  // rolling the game back to before this player's last action.
  const handleRequestTakeback = useCallback(() => {
    if (adapter && adapter instanceof WebSocketAdapter) {
      adapter.sendRequestTakeback();
    }
  }, [adapter]);

  const handleRespondTakeback = useCallback(
    (approve: boolean) => {
      if (adapter && adapter instanceof WebSocketAdapter) {
        adapter.sendRespondTakeback(approve);
      }
      onCloseTakebackDialog();
    },
    [adapter, onCloseTakebackDialog],
  );

  const handleCancelTakeback = useCallback(() => {
    if (adapter && adapter instanceof WebSocketAdapter) {
      adapter.sendCancelTakeback();
    }
    onCloseTakebackDialog();
  }, [adapter, onCloseTakebackDialog]);

  // Issue #311 safety net: when the engine emits a WaitingFor variant the
  // frontend has no UI for, this handler is the user's escape hatch.
  // - Online: concede (server forfeits the seat, opponents see GameOver).
  // - AI / local: clear local state + navigate home (no opponent to notify).
  const handleUnhandledExit = useCallback(() => {
    if (isOnlineMode) {
      handleConcede();
      return;
    }
    clearPromptOverlayState();
    if (gameId) {
      void clearGame(gameId);
    }
    navigate("/");
  }, [isOnlineMode, gameId, handleConcede, navigate]);

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
  // Keys BattlefieldBackground so a mode/seat change remounts it with a fresh
  // lock (see BattlefieldBackground) instead of resetting refs during render.
  const boardBackground = usePreferencesStore((s) => s.boardBackground);
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

  // Auto-open graveyard/exile viewer when the engine is waiting for an object
  // choice in that zone, or when Priority surfaces cast/play actions on cards
  // in a single graveyard/exile pile (Retrace, Flashback, etc.).
  useEffect(() => {
    if (!objects) {
      dismissedCastableZoneViewerKeyRef.current = null;
      return;
    }
    const wf = engineWaitingFor;
    if (!canActForWaitingState) {
      return;
    }

    // Collect distinct (zone, owner) groupings so we don't trap the user in one
    // graveyard when the effect can target either player's graveyard (e.g. Soul-Guide Lantern).
    const groups = new Set<string>();
    let firstHit: { zone: "graveyard" | "exile"; playerId: number } | null = null;
    for (const objectId of getWaitingForObjectChoiceIds(wf)) {
      const obj = objects[objectId];
      if (!obj) continue;
      if (obj.zone !== "Graveyard" && obj.zone !== "Exile") continue;
      const zone: "graveyard" | "exile" = obj.zone === "Graveyard" ? "graveyard" : "exile";
      groups.add(`${zone}:${obj.owner}`);
      if (!firstHit) firstHit = { zone, playerId: obj.owner };
    }
    // Only auto-open when there's a single zone+owner to open. Otherwise the
    // zone control glow prompts the user to pick.
    if (groups.size === 1 && firstHit) {
      dismissedCastableZoneViewerKeyRef.current = null;
      setViewingZone(firstHit);
      return;
    }

    const castableTarget = getCastableZoneViewerTarget(
      wf,
      objects,
      legalActionsByObject,
    );
    if (castableTarget) {
      const autoOpenKey = castableZoneViewerAutoOpenKey(castableTarget);
      if (dismissedCastableZoneViewerKeyRef.current !== autoOpenKey) {
        setViewingZone({
          zone: castableTarget.zone,
          playerId: castableTarget.playerId,
          autoOpenKey,
        });
      }
      return;
    }

    dismissedCastableZoneViewerKeyRef.current = null;
  }, [canActForWaitingState, engineWaitingFor, legalActionsByObject, objects]);

  const handleZoneViewerClose = useCallback(() => {
    if (viewingZone?.autoOpenKey) {
      dismissedCastableZoneViewerKeyRef.current = viewingZone.autoOpenKey;
    }
    setViewingZone(null);
  }, [viewingZone]);

  const handleDeclareCompanion = useCallback(
    (cardIndex: number | null) => {
      dispatch({ type: "DeclareCompanion", data: { card_index: cardIndex } });
    },
    [dispatch],
  );

  // CR 103.5 + 103.5b: `id` encodes the three branches of MulliganChoice.
  // "keep"           → MulliganChoice::Keep
  // "mulligan"       → MulliganChoice::Mulligan
  // "powder:<oid>"   → MulliganChoice::UseSerumPowder { object_id: <oid> }
  const handleMulliganChoice = useCallback(
    (id: string) => {
      if (id.startsWith("powder:")) {
        const objectId = Number(id.slice("powder:".length));
        dispatch({
          type: "MulliganDecision",
          data: { choice: { type: "UseSerumPowder", data: { object_id: objectId } } },
        });
        return;
      }
      dispatch({
        type: "MulliganDecision",
        data: { choice: { type: id === "keep" ? "Keep" : "Mulligan" } },
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
    "--game-split-safe-top": "0px",
    "--game-targeting-prompt-top": splitBoardActive
      ? isMobile ? "4.25rem" : "4.75rem"
      : "0.25rem",
  } as CSSProperties;
  const playerZoneRailStyle: ZoneRailStyle = isMobile
    ? { "--card-w": "28px", "--card-h": "39px" }
    : { "--card-w": "clamp(45px, 4.5vw, 70px)", "--card-h": "clamp(63px, 6.3vw, 98px)" };
  const pileSize = isMobile
    ? { width: "38px", height: "53px" }
    : { width: "clamp(45px, 4.5vw, 70px)", height: "clamp(63px, 6.3vw, 98px)" };
  const handleViewZone = useCallback(
    (zone: "graveyard" | "exile" | "library", zonePlayerId: number) => {
      setViewingZone({ zone, playerId: zonePlayerId });
    },
    [],
  );
  const showFlowHelpNudge =
    !dismissedFlowHelpNudge &&
    !helpSheetOpen &&
    (mode === "ai" || mode === "local") &&
    viewingZone == null &&
    preferencesOpen == null &&
    boardContextMenu == null &&
    !showCardDataMissing &&
    resumeResetReason == null &&
    !showConcedeDialog &&
    disconnectChoice == null &&
    pauseReason == null &&
    reconnectState.status === "idle" &&
    waitingFor?.type === "Priority" &&
    waitingFor.data.player === playerId &&
    canActForWaitingState &&
    stackLength === 0;

  // Sequenced after the flow nudge (requires it dismissed first) so the two
  // first-run hints never stack. Same calm-moment guards as the flow nudge,
  // plus: hidden once the panel is already open (nothing left to advertise).
  const showSandboxToolsNudge =
    !dismissedSandboxToolsNudge &&
    dismissedFlowHelpNudge &&
    !debugPanelOpen &&
    !helpSheetOpen &&
    (mode === "ai" || mode === "local") &&
    viewingZone == null &&
    preferencesOpen == null &&
    boardContextMenu == null &&
    !showCardDataMissing &&
    resumeResetReason == null &&
    !showConcedeDialog &&
    disconnectChoice == null &&
    pauseReason == null &&
    reconnectState.status === "idle" &&
    waitingFor?.type === "Priority" &&
    waitingFor.data.player === playerId &&
    canActForWaitingState &&
    stackLength === 0;

  // Last in the first-run hint chain (requires the sandbox nudge dismissed first)
  // so the three hints never stack. Same calm-moment guards, plus: gated on the
  // report affordance being available and hidden once the dialog is already open.
  const showReportCardNudge =
    !dismissedReportCardNudge &&
    dismissedSandboxToolsNudge &&
    canReportCard &&
    !cardReportDialogOpen &&
    !debugPanelOpen &&
    !helpSheetOpen &&
    (mode === "ai" || mode === "local") &&
    viewingZone == null &&
    preferencesOpen == null &&
    boardContextMenu == null &&
    !showCardDataMissing &&
    resumeResetReason == null &&
    !showConcedeDialog &&
    disconnectChoice == null &&
    pauseReason == null &&
    reconnectState.status === "idle" &&
    waitingFor?.type === "Priority" &&
    waitingFor.data.player === playerId &&
    canActForWaitingState &&
    stackLength === 0;

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
      <SpectatorChrome />
      <BattlefieldBackground key={`${boardBackground}-${playerId}`} />
      <StackDisplay />

      {/* Persistent Sandbox banner — visible to all players whenever the
          game's format_config has debug actions enabled. Not dismissible. */}
      {isSandboxGame && (
        <div
          className="pointer-events-none fixed left-0 right-0 top-0 z-30 select-none bg-amber-600 px-4 py-1 text-center text-xs font-bold uppercase tracking-wider text-white shadow-md"
          role="status"
          aria-label={t("gamePage.sandbox.bannerAria")}
        >
          {t("gamePage.sandbox.banner")}
        </div>
      )}

      {/* Reconnecting banner */}
      {reconnectState.status === "reconnecting" && (
        <div className="fixed left-0 right-0 top-0 z-40 bg-amber-600 px-4 py-2 text-center text-sm font-semibold text-white">
          {reconnectState.maxAttempts > 0
            ? t("gamePage.reconnect.bannerWithMax", {
                attempt: reconnectState.attempt,
                maxAttempts: reconnectState.maxAttempts,
              })
            : t("gamePage.reconnect.banner", { attempt: reconnectState.attempt })}
        </div>
      )}

      {/* Connection lost banner */}
      {reconnectState.status === "failed" && (
        <div className="fixed left-0 right-0 top-0 z-40 flex items-center justify-center gap-4 bg-red-700 px-4 py-2 text-sm font-semibold text-white">
          <span>{t("gamePage.reconnect.connectionLost")}</span>
          <button
            onClick={() => navigate("/")}
            className="rounded bg-white/20 px-3 py-1 text-xs font-semibold hover:bg-white/30"
          >
            {t("gamePage.actions.returnToMenu")}
          </button>
        </div>
      )}

      <DebugModeBanner />

      {/* Full-screen board layout — CSS Grid with 3 rows: opp hand, battlefield, player hand.
          Board choices lift the grid above normal HUD rails, but must stay below
          DialogHost/TargetingOverlay so confirm controls are not hidden behind
          the player hand. Keep this ordering in GAME_Z_LAYER. */}
      <div
        className={`relative ${boardChoiceLayerActive && !isReconnecting ? GAME_Z_LAYER.boardChoiceGrid : GAME_Z_LAYER.board} grid min-w-0 h-full${isReconnecting ? " pointer-events-none" : ""}`}
        style={{
          paddingTop: "var(--game-top-overlay-offset, 0px)",
          gridTemplateRows,
          gridTemplateColumns: "1fr",
        }}
      >
        {/* Row 1: Opponent hand + zone piles (flow layout — piles take real space) */}
        <div
          className={`relative z-20 min-w-0 flex w-full ${splitBoardActive ? "overflow-hidden" : "overflow-visible"}`}
          data-flex-zone="opp-row"
        >
          {renderFocusedOpponentTopRow && (
            <>
              <div className="min-w-0 flex-1">
                <OpponentHand showCards={showAiHand} />
              </div>
              <DraggableWidget
                target={{ kind: "widget", key: "opponentPiles" }}
                flexZone="opponentPiles"
                className="flex shrink-0 items-start gap-1.5 px-1 py-1"
                style={playerZoneRailStyle}
              >
                {activeOpponentId != null ? (
                  <>
                    <ExilePile
                      playerId={activeOpponentId}
                      size={pileSize}
                      onClick={() => handleViewZone("exile", activeOpponentId)}
                    />
                    <LibraryPile
                      playerId={activeOpponentId}
                      size={pileSize}
                      onView={() => handleViewZone("library", activeOpponentId)}
                    />
                    <GraveyardPile
                      playerId={activeOpponentId}
                      size={pileSize}
                      onClick={() => handleViewZone("graveyard", activeOpponentId)}
                    />
                  </>
                ) : null}
              </DraggableWidget>
            </>
          )}
        </div>

        {/* Row 2: Battlefield — takes remaining space; HUDs passed inline to PlayerAreas */}
        <div className="relative z-30 flex min-h-0 min-w-0 flex-col">
          <GameBoard
            oppHud={oppHud}
            playerHud={playerHud}
            showOpponentCards={showAiHand}
            onKickPlayer={isP2PHost ? handleKickPlayer : undefined}
            onViewZone={handleViewZone}
          />
        </div>

        {/* Row 3: Player hand + zones. The hand is top-anchored in this row, so
            if the row stretched with its (resizable) band track, resizing the
            band would drag the hand vertically. Instead we give the row a
            CONSTANT height equal to the DEFAULT band and pin it to the track's
            bottom (`self-end`, the viewport edge, which never moves). The height
            mirrors the resolver's default track exactly — `min(18%, 150px)` of
            the grid's CONTENT box (`100dvh` minus the top-overlay padding) — but
            computed in viewport units so it ignores the LIVE (resized) track,
            which a plain `18%` on a grid item would track instead. The hand thus
            keeps its default resting position and stays put on resize; a grown
            band opens empty space ABOVE the row (trading with the battlefield)
            rather than shoving the hand up. */}
        <div
          className="relative min-w-0 self-end overflow-visible"
          style={{ height: "min(calc(0.18 * (100dvh - var(--game-top-overlay-offset, 0px))), 150px)" }}
          data-flex-zone="player-row"
        >
          <div className="flex items-end justify-center" data-flex-zone="playerHandRow">
            {/* Castable graveyard/exile cards now render as colored wings inside
                PlayerHand's own fan (see ZoneFanCard), so the row is just the hand.
                The `playerHandRow` flex-zone hook drives the mobile hand-lift
                transform in index.css. */}
            <PlayerHand />
          </div>
          <DraggableWidget
            target={{ kind: "widget", key: "playerPiles" }}
            flexZone="playerPiles"
            scaleKey="playerPiles"
            className="pointer-events-none absolute left-0 top-0 bottom-0 z-10 flex w-fit flex-col items-start justify-end gap-0.5 p-1 lg:gap-1 lg:p-3 [&>*]:pointer-events-auto [&>div>*]:pointer-events-auto"
            // Anchor box-scale to the bottom-left dock corner.
            style={{ ...playerZoneRailStyle, transformOrigin: "bottom left" }}
          >
            <div className="flex items-end gap-2">
              <ExilePile
                playerId={perspectivePlayerId}
                size={pileSize}
                onClick={() => handleViewZone("exile", perspectivePlayerId)}
              />
              <GraveyardPile
                playerId={perspectivePlayerId}
                size={pileSize}
                onClick={() => handleViewZone("graveyard", perspectivePlayerId)}
              />
              <LibraryPile
                playerId={perspectivePlayerId}
                size={pileSize}
                onView={() => handleViewZone("library", perspectivePlayerId)}
              />
            </div>
          </DraggableWidget>
          <div
            className="pointer-events-none absolute right-0 top-0 bottom-0 z-10 flex w-fit flex-col items-end justify-end gap-0.5 p-1 lg:gap-1 lg:p-3 [&>*]:pointer-events-auto"
            style={playerZoneRailStyle}
          >
            <CompanionZone playerId={perspectivePlayerId} />
          </div>
        </div>
      </div>

      {/* Bottom UI: mobile splits hand/full-control (left) from phases + pass (right). */}
      <DraggableWidget
        target={{ kind: "widget", key: "actionRail" }}
        flexZone="actionRail"
        scaleKey="actionRail"
        resizeCorner="bl"
        className="fixed z-30 flex flex-col items-end gap-1.5 max-lg:portrait:w-full max-lg:portrait:flex-row max-lg:portrait:items-end max-lg:portrait:justify-between max-lg:portrait:gap-2"
        style={{
          bottom: "calc(env(safe-area-inset-bottom) + var(--action-btn-bottom))",
          right: "calc(env(safe-area-inset-right) + var(--game-edge-right) + var(--game-right-rail-offset, 0px))",
          // Anchor box-scale to the docked corner so it grows inward, not off-screen.
          transformOrigin: "bottom right",
        }}
      >
        {!isSpectatorMode && (
          <div
            data-mobile-action-left
            className="hidden flex-col gap-1 max-lg:portrait:flex max-lg:portrait:min-w-0"
          >
            <div className="flex flex-col gap-1 max-lg:gap-1">
              <CombatPhaseIndicator />
              <HandBadge className="w-full" />
            </div>
            <div className="flex items-center gap-1.5">
              <PriorityYieldList />
              <FullControlToggle className="w-full" />
            </div>
          </div>
        )}
        <div
          data-mobile-action-right
          className="flex flex-col items-end gap-1.5 max-lg:min-w-0 max-lg:portrait:items-stretch lg:items-end"
        >
          {showFlowHelpNudge && <FlowHelpNudge />}
          {showSandboxToolsNudge && <SandboxToolsNudge />}
          {showReportCardNudge && <ReportCardNudge />}
          <div className="hidden max-lg:landscape:block lg:block">
            <CombatPhaseIndicator />
          </div>
          {isSpectatorMode ? (
            <TurnStatusLine />
          ) : (
            <>
              <div className="hidden max-lg:portrait:block max-lg:portrait:w-full">
                <TurnStatusLine />
              </div>
              <div className="hidden flex-row items-center gap-1.5 max-lg:landscape:flex lg:flex">
                <TurnStatusLine />
                <HandBadge />
                {/* CR 117.3d: standing priority-yield summary chip, beside the
                    Full Control toggle (self-hides when no yields stand). */}
                <PriorityYieldList />
                <FullControlToggle />
              </div>
              <ActionButton />
            </>
          )}
        </div>
      </DraggableWidget>

      <GameLogPanel />
      <MobileHandDrawer />
      <FlexEditOverlay />

      {/* Game menu — top-left hamburger */}
      <GameMenu
        gameId={gameId}
        isAiMode={mode === "ai"}
        isOnlineMode={isOnlineMode}
        showAiHand={showAiHand}
        onToggleAiHand={() => setShowAiHand((v) => !v)}
        multiplayerBoardLayout={seatCount > 2 ? multiplayerBoardLayout : undefined}
        onToggleMultiplayerBoardLayout={seatCount > 2 ? handleToggleMultiplayerBoardLayout : undefined}
        onSettingsClick={() => setPreferencesOpen({})}
        onHelpClick={() => setHelpSheetOpen(true)}
        onConcede={onShowConcedeDialog}
        onRequestTakeback={isOnlineMode ? handleRequestTakeback : undefined}
        showSandboxTools={mode === "ai" || mode === "local" || isSandboxGame}
        onSandboxToolsClick={() => useUiStore.getState().openSandboxTools()}
        debugClickModeButtonVisible={debugClickModeButtonVisible}
        onToggleDebugClickModeButtonVisible={toggleDebugClickModeButtonVisible}
        showReportCard={canReportCard}
        onReportCardClick={() => useUiStore.getState().openCardReportDialog()}
      />
      <HelpSheet />
      <CardReportDialog />

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
          <div className="relative z-10 w-full max-w-sm rounded-[12px] border border-yellow-400/30 bg-[#0b1020] p-6 text-center shadow-[0_18px_48px_rgba(0,0,0,0.48)]">
            <h2 className="mb-2 text-lg font-bold text-yellow-400">
              {t("gamePage.opponentDisconnected.title")}
            </h2>
            <p className="text-sm text-gray-300">
              {t("gamePage.opponentDisconnected.body")}
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

      {/* cEDH bracket-violation blocking modal.
          Shown when the engine refuses game init because one or more decks
          are not declared cEDH (bracket 5) at a cEDH table.
          Covers the entire page — no game state is accessible behind it
          because the engine never initialised. */}
      {bracketViolationError && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/80"
          role="dialog"
          aria-modal="true"
          aria-label={t("gameSetup.bracketViolation.title")}
          data-testid="bracket-violation-modal"
        >
          <div className="mx-4 max-w-md rounded-xl bg-gray-900 p-6 shadow-2xl ring-1 ring-rose-700/60">
            <h2 className="mb-2 text-lg font-bold text-rose-400">
              {t("gameSetup.bracketViolation.title")}
            </h2>
            <p className="mb-4 text-sm text-gray-300">{bracketViolationError}</p>
            <p className="mb-6 text-xs text-gray-500">
              {t("gameSetup.bracketViolation.body")}
            </p>
            <button
              onClick={onDismissBracketViolation}
              className="w-full rounded-lg bg-rose-700 py-2 text-sm font-semibold text-white transition hover:bg-rose-600"
            >
              {t("gameSetup.bracketViolation.returnToSetup")}
            </button>
          </div>
        </div>
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
            <span className="text-sm text-amber-200">
              {t("gamePage.resumeReset.message", { reason: resumeResetReason })}
            </span>
            <button
              onClick={onDismissResumeReset}
              className="rounded bg-amber-800 px-2.5 py-1 text-xs font-semibold text-amber-100 transition hover:bg-amber-700"
            >
              {t("gamePage.actions.ok")}
            </button>
          </motion.div>
        )}
      </AnimatePresence>

      {/* Overlay layers */}
      <DebugPanel />
      <ResolutionProgressOverlay />

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
          onCustomizeLayout={() => useUiStore.getState().setFlexEditMode(true)}
          onToggleGameLog={() => useUiStore.getState().toggleLogPanel()}
          onToggleDebugLog={() => useUiStore.getState().toggleDebugPanel()}
          onReportCard={
            canReportCard ? () => useUiStore.getState().openCardReportDialog() : undefined
          }
        />
      )}

      <DebugCardContextMenu />
      <DebugLibraryViewer />

      {/* Animation overlay (above board, below modals) */}
      <AnimationOverlay containerRef={containerRef} />
      {/* Multi-card top-of-library reveal (CR 701.20b), e.g. Lead the Stampede */}
      <RevealOverlay />
      <TurnBanner />
      <DiceRollOverlay />

      {/* Combat SVG overlays: blocker assignments + attack target arrows */}
      <BlockAssignmentLines />
      <AttackTargetLines />
      {/* Per-attacker "needs N blockers" badges (menace / "blocked by N or more").
          Self-gates: renders nothing unless the local player is assigning blockers
          to attackers that carry a minimum-blocker requirement. */}
      <BlockRequirementBadges />

      {/* Card preview overlay. Owns its own inspect-state subscriptions so a
          hover doesn't re-render GamePageContent (and the whole battlefield). */}
      <GameCardPreview />

      {/* WaitingFor-driven prompt overlays (only for human player).
          Wrapped in DialogHost so any active dialog can be peeked away to
          reveal the battlefield underneath; peek state resets on every
          new WaitingFor so a fresh prompt is always visible. */}
      <DialogHost>
        {waitingFor != null &&
          isClickThroughWaitingFor(waitingFor, objects) &&
          canActForWaitingState && <TargetingOverlay />}
        {waitingFor != null &&
          MANA_PAYMENT_WAITING_FOR_TYPES.has(waitingFor.type) &&
          canActForWaitingState && <ManaPaymentUI />}
        {waitingFor?.type === "ChooseXValue" &&
          canActForWaitingState && <ChooseXValueUI />}
        {waitingFor?.type === "PayAmountChoice" &&
          canActForWaitingState && <PayAmountChoiceUI />}
        {waitingFor?.type === "AssistPayment" &&
          canActForWaitingState && <AssistPaymentUI />}
        {waitingFor?.type === "ReplacementChoice" &&
          canActForWaitingState && <ReplacementModal />}
        {waitingFor?.type === "OrderTriggers" &&
          canActForWaitingState && <TriggerOrderModal />}
        <BattleProtectorModal />
        <AssistChoosePlayerModal />
        <ClashOpponentModal />
        <TributeModal />
        <CombatTaxModal />
        <AlternativeCostModal />
        <CastingVariantModal />
        <PermanentTypeSlotModal />
        <ModeChoiceModal />
        <ChooseOneOfBranchModal />
        <LifeRedistributionModal />
        <AdventureCastModal />
        <CascadeChoiceModal />
        <SpellbookDraftModal />
        <FreeCastWindowModal />
        <ModalFaceModal />
        <MiracleRevealModal />
        {waitingFor?.type === "SpliceOffer" &&
          canActForWaitingState && (
            <SpliceOfferModal />
          )}

        {/* Scry/Dig/Surveil card choice modal */}
        <CardChoiceModal />

        {/* Ability choice picker (planeswalkers, multi-ability permanents) */}
        <AbilityChoiceModal />

        {/* Player-attached Aura viewer (Curse cycle, Faith's Fetters, etc.).
            Mounted here — not from inside HudPlate where the badge lives —
            so the dialog's `fixed inset-0` shell anchors to the viewport
            instead of HudPlate's transform-CB bounding box. */}
        <PlayerEnchantmentsDialog />

        {/* Permanent-attachment fan (Equipment / Aura / Fortification on a
            battlefield object): a centered spread of the host + attachments,
            each with its live selection affordance. Opened by clicking a
            permanent-with-attachments during a target/board-choice prompt or by
            the host's ⧉ badge. Self-portals to document.body, so its mount point
            here is incidental. */}
        <AttachmentFan />

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

        {/* CR 401.4: Owner puts permanent on top or bottom of library */}
        {(waitingFor?.type === "TopOrBottomChoice" || waitingFor?.type === "ClashCardPlacement") &&
          canActForWaitingState && (
            <TopOrBottomModal />
          )}

        {/* CR 702.140c + CR 730.2a: mutate spell controller chooses top/bottom */}
        {waitingFor?.type === "MutateMergeChoice" &&
          canActForWaitingState && (
            <MutateMergeModal />
          )}

        {/* CR 702.99a: cipher spell controller chooses a creature to encode on */}
        {waitingFor?.type === "CipherEncodeChoice" &&
          canActForWaitingState && (
            <CipherEncodeModal />
          )}

        {waitingFor?.type === "UntapChoice" &&
          canActForWaitingState && (
            <UntapChoiceModal />
          )}

        {/* CR 701.43d: Optional "exert as it attacks" choice (Combat Celebrant). */}
        {waitingFor?.type === "ExertChoice" &&
          canActForWaitingState && (
            <ExertChoiceModal />
          )}

        {/* CR 702.154a: Optional Enlist tap choice during declare attackers. */}
        {waitingFor?.type === "EnlistChoice" &&
          canActForWaitingState && (
            <EnlistChoiceModal />
          )}

        {/* Unless payment choice ("Counter unless you pay {X}") */}
        {waitingFor?.type === "UnlessPayment" &&
          canActForWaitingState && (
            <UnlessPaymentPanel />
          )}

        {/* CR 118.12a: Disjunctive unless-cost choice (Tergrid's Lantern). */}
        {waitingFor?.type === "UnlessPaymentChooseCost" &&
          canActForWaitingState && (
            <UnlessPaymentChooseCostModal />
          )}
        {waitingFor?.type === "ActivationCostOneOfChoice" &&
          canActForWaitingState && (
            <ActivationCostOneOfChoiceModal />
          )}
      </DialogHost>

      {/* Graveyard/exile viewer mounts after DialogHost so its z-[60] shell
          paints above prompt overlays (issue #2387: retrace / graveyard cast). */}
      {viewingZone && (
        <ZoneViewer
          zone={viewingZone.zone}
          playerId={viewingZone.playerId}
          onClose={handleZoneViewerClose}
        />
      )}

      {waitingFor?.type === "CompanionReveal" &&
        waitingFor.data.player === playerId && (
          <CompanionRevealPrompt
            eligibleCompanions={waitingFor.data.eligible_companions}
            onChoose={handleDeclareCompanion}
          />
        )}

      {/* CR 103.5: Simultaneous mulligan — render this player's modal iff
          they are in the pending set. Each player decides independently.
          Held back until the CR 103.1 starting-player contest finishes so the
          dice aren't hidden behind this modal. */}
      {waitingFor?.type === "MulliganDecision" &&
        !startingContestActive &&
        (() => {
          const entry = waitingFor.data.pending.find(
            (e) => e.player === playerId,
          );
          if (!entry) return null;
          // CR 103.5b: bottoming is folded into the MulliganDecision variant as
          // a per-entry BottomCards sub-phase resolved at this player's own
          // declare point.
          if (entry.phase.type === "BottomCards") {
            return (
              <MulliganBottomCardsPrompt
                playerId={entry.player}
                count={entry.phase.count}
                openingHandBottom={false}
                excludedCardId={
                  entry.phase.then.type === "UseSerumPowder"
                    ? entry.phase.then.object_id
                    : undefined
                }
                onChoose={handleBottomCards}
              />
            );
          }
          return (
            <MulliganDecisionPrompt
              playerId={entry.player}
              mulliganCount={entry.mulligan_count}
              freeFirstMulligan={waitingFor.data.free_first_mulligan}
              onChoose={handleMulliganChoice}
            />
          );
        })()}

      {waitingFor?.type === "MulliganDecision" &&
        !startingContestActive &&
        !waitingFor.data.pending.some((e) => e.player === playerId) && (
          <div className="fixed inset-0 z-50 flex items-center justify-center">
            <div className="absolute inset-0 bg-[radial-gradient(circle_at_top,rgba(31,41,55,0.55),rgba(2,6,23,0.92)_58%,rgba(2,6,23,0.98))]" />
            <div className="relative text-center">
              <p className="text-base font-semibold text-white">
                {t("gamePage.mulligan.opponentDeciding")}
              </p>
            </div>
          </div>
        )}

      {waitingFor?.type === "OpeningHandBottomCards" &&
        (() => {
          const entry = waitingFor.data.pending.find(
            (e) => e.player === playerId,
          );
          if (!entry) return null;
          return (
            <MulliganBottomCardsPrompt
              playerId={entry.player}
              count={entry.count}
              openingHandBottom
              onChoose={handleBottomCards}
            />
          );
        })()}

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
            title={t("gamePage.playDraw.title", { gameNumber: waitingFor.data.game_number })}
            subtitle={t("gamePage.playDraw.matchScore", {
              p0Wins: waitingFor.data.score.p0_wins,
              p1Wins: waitingFor.data.score.p1_wins,
            })}
            options={[
              {
                id: "play",
                label: t("gamePage.playDraw.playFirst"),
                description: t("gamePage.playDraw.playFirstDescription"),
              },
              {
                id: "draw",
                label: t("gamePage.playDraw.drawFirst"),
                description: t("gamePage.playDraw.drawFirstDescription"),
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
          <TakebackRequestDialog
            isOpen={pendingTakeback !== null}
            requesterName={pendingTakeback?.requesterName ?? ""}
            isOwnRequest={pendingTakeback?.requester === playerId}
            onApprove={() => handleRespondTakeback(true)}
            onDecline={() => handleRespondTakeback(false)}
            onCancel={handleCancelTakeback}
          />
          {!isSpectatorMode && (
            <EmoteOverlay
              onSendEmote={handleSendEmote}
              receivedEmote={receivedEmote}
            />
          )}
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

      {/* Issue #311: Fail-loud safety net for orphan WaitingFor states.
          Renders only when (a) the engine is waiting on the local player
          and (b) the WaitingFor type has no UI handler in the frontend.
          Without this, an unknown WaitingFor would silently hang the game
          with no way to escape — see UnhandledWaitingForModal for details. */}
      <UnhandledWaitingForModal
        onExit={handleUnhandledExit}
        exitLabel={isOnlineMode ? t("gamePage.actions.concedeGame") : t("gamePage.actions.returnToMenuLower")}
      />
    </div>
  );
}

// ── Mulligan Bottom Cards ─────────────────────────────────────────────────

interface MulliganBottomCardsPromptProps {
  playerId: number;
  count: number;
  openingHandBottom?: boolean;
  // CR 103.5b: when this bottom obligation completes into UseSerumPowder, the
  // earmarked Powder object must stay in hand to be exiled by its own effect,
  // so it is not selectable as a bottomed card (the engine rejects it too).
  excludedCardId?: ObjectId;
  onChoose: (id: string) => void;
}

interface MulliganDecisionPromptProps {
  playerId: number;
  mulliganCount: number;
  freeFirstMulligan: boolean;
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
  // Reuse the DialogHost peek affordance so the player can slide the (large)
  // mulligan modal out of the way to see the table — identical collapse
  // muscle-memory to engine dialogs, via the shared slide math + tab components.
  const { peeked, togglePeek, setPeeked, isNarrow, slideTransform } = useModalPeek();

  return (
    <>
    <div
      className="fixed inset-0 z-50 overflow-x-hidden overflow-y-auto px-2 py-2 lg:px-4 lg:py-6"
      style={{ pointerEvents: peeked ? "none" : undefined }}
    >
      <motion.div
        className="absolute inset-0 bg-[radial-gradient(circle_at_top,rgba(31,41,55,0.55),rgba(2,6,23,0.92)_58%,rgba(2,6,23,0.98))]"
        animate={{ opacity: peeked ? 0 : 1 }}
        transition={{ duration: 0.24, ease: "easeOut" }}
      />
      <div className="relative flex min-h-full items-center justify-center pb-[env(safe-area-inset-bottom)] pt-[env(safe-area-inset-top)]">
        <motion.div
          className="card-scale-reset pointer-events-auto relative z-10 w-full max-w-6xl"
          initial={{ opacity: 0, y: 18, scale: 0.98 }}
          animate={{ opacity: 1, scale: 1, ...slideTransform }}
          transition={{ duration: 0.24, ease: "easeOut" }}
        >
          <div className="flex w-full flex-col overflow-hidden rounded-[12px] border border-white/10 bg-[#0b1020] shadow-[0_18px_48px_rgba(0,0,0,0.48)]">
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
          </div>
          <PeekTab
            direction={isNarrow ? "bottom" : "right"}
            onClick={togglePeek}
          />
        </motion.div>
      </div>
    </div>
    {/* Sibling of the (pointer-events:none while peeked) overlay so the restore
        tab itself stays clickable and board taps pass through behind it. */}
    {peeked && (
      <PeekRestoreTab
        direction={isNarrow ? "bottom" : "right"}
        onClick={() => setPeeked(false)}
      />
    )}
    </>
  );
}

function MulliganDecisionPrompt({
  playerId,
  mulliganCount,
  freeFirstMulligan,
  onChoose,
}: MulliganDecisionPromptProps) {
  const { t } = useTranslation("game");
  const player = useGameStore((s) => s.gameState?.players[playerId]);
  const objects = useGameStore((s) => s.gameState?.objects);
  const legalActions = useGameStore((s) => s.legalActions);
  const hoverProps = useInspectHoverProps();
  // `animationDone` is the raw signal from Framer's `onAnimationComplete`
  // on the last card. `buttonsVisible` is the derived predicate — buttons
  // show once card-deal animations finish *or* immediately when the hand
  // is empty and there's nothing to animate (mulligan-to-zero path), so the
  // last-card callback never fires.
  const [animationDone, setAnimationDone] = useState(false);
  const handCount = player?.hand.length ?? 0;
  const buttonsVisible = animationDone || handCount === 0;

  // Engine rule (CR 103.5 + 103.5c): bottom_count_on_keep = mulligan_count - (free_first ? 1 : 0).
  // The *next* mulligan is "free" iff applying that formula at mulligan_count + 1 yields 0.
  const bottomOnKeep = Math.max(0, mulliganCount - (freeFirstMulligan ? 1 : 0));
  const nextMulliganFree = freeFirstMulligan && mulliganCount === 0;
  const nextHandSize = 7 - Math.max(0, mulliganCount + 1 - (freeFirstMulligan ? 1 : 0));

  // CR 103.5b + Serum Powder Oracle text: surface one button per legal
  // `UseSerumPowder` action the engine has already enumerated. The engine
  // (`ai_support::candidates::serum_powders_in_hand`) is the single authority
  // for which hand object qualifies — the FE must not duplicate the
  // name-match check. Each candidate carries an `object_id` whose display
  // name comes from `objects[id]?.name`.
  const serumPowderIds: number[] = legalActions
    .map((a) =>
      a.type === "MulliganDecision" && a.data.choice.type === "UseSerumPowder"
        ? a.data.choice.data.object_id
        : null,
    )
    .filter((oid): oid is number => oid !== null);

  if (!player || !objects) {
    const fallbackOptions = [
      {
        id: "keep",
        label: t("gamePage.mulligan.keepHand"),
        description:
          bottomOnKeep > 0
            ? t("gamePage.mulligan.putOnBottom", { count: bottomOnKeep })
            : t("gamePage.mulligan.noCardsToBottom"),
      },
      {
        id: "mulligan",
        label: nextMulliganFree
          ? t("gamePage.mulligan.freeMulligan")
          : t("gamePage.mulligan.mulligan"),
        description: nextMulliganFree
          ? t("gamePage.mulligan.shuffleDrawSevenFree")
          : t("gamePage.mulligan.shuffleDrawSevenAgain"),
      },
      // CR 103.5b: A Powder option per legal `UseSerumPowder` candidate the
      // engine emitted. The button label uses the object's engine-provided
      // name so the FE never re-evaluates which hand objects qualify.
      ...serumPowderIds.map((oid) => ({
        id: `powder:${oid}`,
        label: t("gamePage.mulligan.usePowder", {
          name: objects?.[oid]?.name ?? "Serum Powder",
        }),
        description: t("gamePage.mulligan.powderDescription"),
      })),
    ];
    return (
      <ChoiceModal
        title={t("gamePage.mulligan.londonTitle", { count: mulliganCount })}
        options={fallbackOptions}
        onChoose={onChoose}
      />
    );
  }

  const handObjects = player.hand.map((id) => objects[id]).filter(Boolean);
  return (
    <MulliganPanel
      eyebrow={
        mulliganCount > 0
          ? t("gamePage.mulligan.eyebrowMulligan", { count: mulliganCount })
          : t("gamePage.mulligan.eyebrowOpening")
      }
      title={t("gamePage.mulligan.reviewTitle")}
      subtitle={
        mulliganCount > 0
          ? bottomOnKeep > 0
            ? t("gamePage.mulligan.subtitleKeepWithBottom", { count: bottomOnKeep })
            : t("gamePage.mulligan.subtitleKeepFree")
          : nextMulliganFree
            ? t("gamePage.mulligan.subtitleOpeningFree")
            : t("gamePage.mulligan.subtitleOpeningOne")
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
                {nextMulliganFree
                  ? t("gamePage.mulligan.freeMulligan")
                  : t("gamePage.mulligan.mulliganTo", { count: nextHandSize })}
              </button>
              {/* CR 103.5b: One button per legal `UseSerumPowder` candidate
                  the engine surfaced. Name comes from engine-provided state. */}
              {serumPowderIds.map((oid) => (
                <button
                  key={oid}
                  onClick={() => onChoose(`powder:${oid}`)}
                  className="rounded-[10px] border border-amber-500/40 bg-amber-500/10 px-3 py-1.5 text-xs font-semibold text-amber-200 transition hover:bg-amber-500/20 hover:text-amber-100 lg:min-h-11 lg:rounded-[16px] lg:px-5 lg:py-3 lg:text-base"
                  title={t("gamePage.mulligan.powderTooltip")}
                >
                  {t("gamePage.mulligan.usePowder", {
                    name: objects?.[oid]?.name ?? "Serum Powder",
                  })}
                </button>
              ))}
              <button
                onClick={() => onChoose("keep")}
                className="rounded-[10px] bg-cyan-500 px-3 py-1.5 text-xs font-semibold text-slate-950 shadow-[0_14px_34px_rgba(6,182,212,0.28)] transition hover:bg-cyan-400 lg:min-h-11 lg:rounded-[16px] lg:px-5 lg:py-3 lg:text-base"
              >
                {t("gamePage.mulligan.keepHand")}
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
            "--card-w": "clamp(140px, 18vw, 257px)",
            "--card-h": "clamp(196px, 25.2vw, 360px)",
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
                  if (index === handObjects.length - 1) setAnimationDone(true);
                }}
                {...hoverProps(obj.id)}
              >
                <CardImage
                  cardName={obj.name}
                  size="normal"
                  className="h-[clamp(200px,40vh,360px)] w-[clamp(143px,28.6vh,257px)]"
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
  const { t } = useTranslation("game");
  const [buttonsVisible, setButtonsVisible] = useState(
    eligibleCompanions.length === 0,
  );

  return (
    <MulliganPanel
      eyebrow={t("gamePage.companion.eyebrow")}
      title={t("gamePage.companion.title")}
      subtitle={t("gamePage.companion.subtitle")}
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
                {t("gamePage.companion.decline")}
              </button>
              {eligibleCompanions.map(([name], i) => (
                <button
                  key={name}
                  onClick={() => onChoose(i)}
                  className="min-h-11 rounded-[16px] bg-amber-500 px-5 py-3 text-sm font-semibold text-slate-950 shadow-[0_14px_34px_rgba(245,158,11,0.28)] transition hover:bg-amber-400 sm:text-base"
                >
                  {t("gamePage.companion.reveal", { name })}
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
            "--card-w": "clamp(140px, 18vw, 257px)",
            "--card-h": "clamp(196px, 25.2vw, 360px)",
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
                  className="h-[clamp(200px,40vh,360px)] w-[clamp(143px,28.6vh,257px)]"
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
  openingHandBottom = false,
  excludedCardId,
  onChoose,
}: MulliganBottomCardsPromptProps) {
  const { t } = useTranslation("game");
  const player = useGameStore((s) => s.gameState?.players[playerId]);
  const objects = useGameStore((s) => s.gameState?.objects);
  const selectedCardIds = useUiStore((s) => s.selectedCardIds);
  const cycleSelectedCard = useUiStore((s) => s.cycleSelectedCard);
  const clearSelectedCards = useUiStore((s) => s.clearSelectedCards);
  const hoverProps = useInspectHoverProps();

  // Issue #1546: `selectedCardIds` is a single store array shared with targeting,
  // convoke, and tap-for-mana overlays. If a prior overlay left a stale selection
  // (e.g. an Opening-Hand bottom prompt immediately followed by a Mulligan bottom
  // prompt, or game 2+ of a match), the bottoming selection starts already at the
  // cap and clicks appear unresponsive. Clear the shared selection on mount and
  // unmount, mirroring `TargetingOverlay`, so bottoming always begins empty.
  useEffect(() => {
    clearSelectedCards();
    return () => clearSelectedCards();
  }, [clearSelectedCards]);

  if (!player || !objects) return null;

  // CR 103.5b: the earmarked Serum Powder object stays in hand to be exiled by
  // its own effect, so it is excluded from the selectable bottom-cards set (the
  // engine rejects any selection containing it).
  const handObjects = player.hand
    .filter((id) => id !== excludedCardId)
    .map((id) => objects[id])
    .filter(Boolean);
  const isReady = selectedCardIds.length === count;

  const handleConfirm = () => {
    onChoose(selectedCardIds.join(","));
  };

  return (
    <MulliganPanel
      eyebrow={
        openingHandBottom
          ? t("gamePage.bottomCards.eyebrowTinyLeaders")
          : t("gamePage.bottomCards.eyebrowLondon")
      }
      title={t("gamePage.bottomCards.title", { count })}
      subtitle={
        openingHandBottom
          ? t("gamePage.bottomCards.subtitleOpening", { count })
          : t("gamePage.bottomCards.subtitleMulligan", { count })
      }
      footer={
        <motion.div
          className="flex w-full flex-col gap-3 sm:flex-row sm:items-center sm:justify-between"
          initial={{ opacity: 0, y: 20 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ delay: 0.12, duration: 0.22 }}
        >
          <div className="text-sm text-slate-400">
            {t("gamePage.bottomCards.selectedOf", {
              selected: selectedCardIds.length,
              count,
            })}
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
            {t("gamePage.bottomCards.confirmSelection")}
          </button>
        </motion.div>
      }
    >
      <div
        className="modal-card-area flex min-h-0 flex-1 items-center justify-center"
        style={
          {
            "--card-w": "clamp(140px, 18vw, 257px)",
            "--card-h": "clamp(196px, 25.2vw, 360px)",
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
                  onClick={() => cycleSelectedCard(obj.id, count)}
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
                    className="h-[clamp(200px,40vh,360px)] w-[clamp(143px,28.6vh,257px)]"
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
  const { t } = useTranslation("game");
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

  const titleText = isDraw
    ? t("gamePage.gameOver.draw")
    : isVictory
      ? t("gamePage.gameOver.victory")
      : t("gamePage.gameOver.defeat");
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

  const source = searchParams.get("source");
  const draftId = searchParams.get("draftId");
  const isDraft = source === "draft" && !!draftId;
  const isDraftPodMatch = mode === "draft-match";
  const gameId = useGameStore((s) => s.gameId);

  const [resultRecorded, setResultRecorded] = useState(false);
  const [runOver, setRunOver] = useState(false);

  useEffect(() => {
    if (!isDraft || !gameId || resultRecorded) return;
    const result: DraftMatchResult = isDraw ? "draw" : isVictory ? "win" : "loss";
    useDraftStore.getState().recordMatchResult(gameId, result).then(() => {
      setResultRecorded(true);
      const meta = loadActiveQuickDraft();
      if (meta?.phase === "complete") setRunOver(true);
    });
  }, [isDraft, gameId, isDraw, isVictory, resultRecorded]);

  useEffect(() => {
    if (!isDraftPodMatch || resultRecorded) return;
    void useMultiplayerDraftStore
      .getState()
      .reportActiveMatchGameResult(winner)
      .then(() => setResultRecorded(true))
      .catch((err) => {
        console.error("[GameOverScreen] failed to report draft pod match result:", err);
      });
  }, [isDraftPodMatch, resultRecorded, winner]);

  const handleRematch = () => {
    const newId = crypto.randomUUID();
    // Preserve the original launch configuration (format, players, match,
    // first, source, draftId, …) by copying the current URL's searchParams.
    // Only drop params that are bound to this specific game instance — the
    // P2P join code and per-room name don't apply to a fresh game.
    const params = new URLSearchParams(searchParams);
    params.delete("code");
    params.delete("roomName");
    if (mode) params.set("mode", mode);
    params.set("difficulty", difficulty);
    navigate(`/game/${newId}?${params.toString()}`);
  };

  const handleBackToDraft = () => {
    navigate("/draft/quick?resume=1");
  };

  return (
    <div
      className="fixed inset-0 z-50 flex flex-col items-center justify-center px-4"
      style={{ background: bgGradient }}
    >
      {isVictory && <VictoryParticles />}

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

      <AnimatePresence>
        {buttonsVisible && (
          <motion.div
            className="relative z-10 mt-6 rounded-[10px] border border-white/10 bg-slate-950/82 px-5 py-4 text-center"
            initial={{ opacity: 0, y: 10 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ duration: 0.4 }}
          >
            <p className="text-base text-gray-200 sm:text-lg">
              <Trans
                i18nKey="gamePage.gameOver.lifeSummary"
                t={t}
                values={{ playerLife, opponentLife }}
                components={{
                  player: <span className="font-bold text-white" />,
                  sep: <span className="mx-3 text-gray-500" />,
                  opponent: <span className="font-bold text-white" />,
                }}
              />
            </p>
            {(turnCount > 0 || gameDuration !== null) && (
              <p className="mt-2 text-xs text-gray-400 sm:text-sm">
                {turnCount > 0 && <span>{t("gamePage.gameOver.turns", { count: turnCount })}</span>}
                {turnCount > 0 && gameDuration !== null && (
                  <span className="mx-2 text-gray-600">|</span>
                )}
                {gameDuration !== null && (
                  <span>
                    {t("gamePage.gameOver.duration", {
                      time: `${Math.floor(gameDuration / 60)}:${String(gameDuration % 60).padStart(2, "0")}`,
                    })}
                  </span>
                )}
              </p>
            )}
          </motion.div>
        )}
      </AnimatePresence>

      <AnimatePresence>
        {buttonsVisible && (
          <motion.div
            className="relative z-10 mt-8 flex w-full max-w-[min(28rem,calc(100vw-2rem))] flex-col gap-3 rounded-[10px] border border-white/10 bg-[#0b1020] p-2 shadow-[0_12px_32px_rgba(0,0,0,0.38)] sm:w-auto sm:max-w-fit sm:flex-row sm:items-center sm:justify-center"
            initial={{ opacity: 0, y: 20 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ delay: 0.15, duration: 0.3 }}
          >
            {isDraft ? (
              <button
                disabled={!resultRecorded}
                onClick={handleBackToDraft}
                className={gameButtonClass({
                  tone: isVictory ? "emerald" : "slate",
                  size: "lg",
                  disabled: !resultRecorded,
                  className: "w-full justify-center sm:w-auto sm:min-w-[12rem]",
                })}
              >
                {runOver
                  ? t("gamePage.gameOver.backToDraft")
                  : t("gamePage.gameOver.continueRun")}
              </button>
            ) : isDraftPodMatch ? (
              <button
                disabled={!resultRecorded}
                onClick={() => navigate("/draft-pod")}
                className={gameButtonClass({
                  tone: isVictory ? "amber" : "slate",
                  size: "lg",
                  disabled: !resultRecorded,
                  className: "w-full justify-center sm:w-auto sm:min-w-[12rem]",
                })}
              >
                {t("gamePage.gameOver.backToDraft")}
              </button>
            ) : isOnlineMode ? (
              <button
                onClick={() => navigate("/?view=lobby")}
                className={gameButtonClass({
                  tone: isVictory ? "amber" : "slate",
                  size: "lg",
                  className: "w-full justify-center sm:w-auto sm:min-w-[12rem]",
                })}
              >
                {t("gamePage.gameOver.backToLobby")}
              </button>
            ) : (
              <>
                <button
                  onClick={() => navigate("/")}
                  className={gameButtonClass({
                    tone: isVictory ? "amber" : "slate",
                    size: "lg",
                    className: "w-full justify-center sm:w-auto sm:min-w-[12rem]",
                  })}
                >
                  {t("gamePage.actions.returnToMenu")}
                </button>
                <button
                  onClick={handleRematch}
                  className={gameButtonClass({
                    tone: isVictory ? "emerald" : "neutral",
                    size: "lg",
                    className: "w-full justify-center sm:w-auto sm:min-w-[12rem]",
                  })}
                >
                  {t("gamePage.gameOver.rematch")}
                </button>
              </>
            )}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

// ── Ability Choice Modal ──────────────────────────────────────────────────

function AbilityChoiceModal() {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const pending = useUiStore((s) => s.pendingAbilityChoice);
  const setPending = useUiStore((s) => s.setPendingAbilityChoice);
  const obj = useGameStore((s) =>
    pending ? s.gameState?.objects[pending.objectId] : undefined,
  );
  const objects = useGameStore((s) => s.gameState?.objects);
  const webSlingingCosts = useGameStore(
    (s) => s.gameState?.derived?.web_slinging_costs,
  );

  if (!pending || !obj) return null;

  // CR 702.190a: When every pending action is a Sneak cast, reframe the
  // modal's subtitle — the user is choosing which attacker to return as the
  // cost-payment creature, not activating an ability.
  const onlyPreparedCopy =
    pending.actions.length === 1 && pending.actions[0]?.type === "CastPreparedCopy";
  const allSneak = pending.actions.every((a) => a.type === "CastSpellAsSneak");
  const allPlayOrCast = pending.actions.every((a) =>
    a.type === "CastSpell"
    || a.type === "CastSpellForFree"
    || a.type === "CastSpellAsMiracle"
    || a.type === "CastSpellAsMadness"
    || a.type === "CastSpellAsSneak"
    || a.type === "CastSpellAsWebSlinging"
    || a.type === "CastPreparedCopy"
    || a.type === "PlayLand"
  );
  // #506: a single pending action is a confirmation, not a choice — the modal
  // surfaces a lone card-consuming ability (cycling / Channel) so the player
  // explicitly opts in rather than auto-firing it.
  const subtitle = allSneak
    ? t("gamePage.abilityChoice.subtitleSneak")
    : onlyPreparedCopy
      ? t("gamePage.abilityChoice.subtitlePreparedCopy")
      : pending.actions.length === 1
        ? t("gamePage.abilityChoice.subtitleActivate")
        : allPlayOrCast
          ? t("gamePage.abilityChoice.subtitlePlay")
          : t("gamePage.abilityChoice.subtitleChoose");

  return (
    <ChoiceModal
      title={obj.name}
      subtitle={subtitle}
      previewCardName={obj.name}
      previewCardTypes={obj.card_types}
      options={pending.actions.map((action, i) => {
        const { label, description } = abilityChoiceLabel(
          action,
          obj,
          objects,
          webSlingingCosts,
        );
        // CR 606.1: prefix a loyalty badge for planeswalker ability costs,
        // reading the structured Loyalty cost (never parsing the label string).
        const ability =
          action.type === "ActivateAbility"
            ? obj.abilities[action.data.ability_index]
            : undefined;
        const badge = loyaltyBadge(ability?.cost);
        if (badge) {
          return {
            id: String(i),
            label: stripLoyaltyCostPrefix(label),
            description,
            // No `size`: mana-font scales the loyalty glyph off the parent's
            // font-size (font-size:1.5em), so it inherits the option row size.
            icon: (
              <ManaFontIcon
                iconClass={badge.iconClasses}
                fallbackText={badge.text}
                label={badge.text}
              />
            ),
          };
        }
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

function SpellbookDraftModal() {
  const { t } = useTranslation("game");
  const canActForWaitingState = useCanActForWaitingState();
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);
  const source = useGameStore((s) =>
    waitingFor?.type === "SpellbookDraft"
      ? s.gameState?.objects[waitingFor.data.source_id]
      : undefined,
  );

  if (waitingFor?.type !== "SpellbookDraft") return null;
  if (!canActForWaitingState) return null;

  return (
    <ChoiceModal
      title={t("cardChoice.dig.title")}
      subtitle={source?.name}
      previewCardName={source?.name}
      previewCardTypes={source?.card_types}
      previewObjectId={waitingFor.data.source_id}
      options={waitingFor.data.options.map((name) => ({
        id: name,
        label: name,
      }))}
      onChoose={(card) =>
        dispatch({ type: "SubmitSpellbookDraft", data: { card } })
      }
    />
  );
}

// ── Optional Cost Choice Modal ──────────────────────────────────────────

function OptionalCostModal() {
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);

  if (waitingFor?.type !== "OptionalCostChoice") return null;

  return <OptionalCostModalContent waitingFor={waitingFor} dispatch={dispatch} />;
}

// ── Defiler Payment Modal ────────────────────────────────────────────

function DefilerPaymentModal() {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);

  if (waitingFor?.type !== "DefilerPayment") return null;

  const { life_cost } = waitingFor.data;

  return (
    <ChoiceModal
      title={t("gamePage.defiler.title")}
      subtitle={t("gamePage.defiler.subtitle", { lifeCost: life_cost })}
      options={[
        { id: "pay", label: t("gamePage.defiler.payLife", { lifeCost: life_cost }) },
        { id: "skip", label: t("gamePage.defiler.decline") },
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

  return <OptionalEffectModalContent waitingFor={waitingFor} objects={objects} dispatch={dispatch} />;
}

// ── Top or Bottom Choice Modal (CR 401.4) ──────────────────────────────

function TopOrBottomModal() {
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);
  const objects = useGameStore((s) => s.gameState?.objects);

  if (waitingFor?.type !== "TopOrBottomChoice" && waitingFor?.type !== "ClashCardPlacement") return null;

  return <TopOrBottomChoiceModalContent waitingFor={waitingFor} objects={objects} dispatch={dispatch} />;
}

// ── Mutate Merge Choice Modal (CR 702.140c + CR 730.2a) ────────────────

function MutateMergeModal() {
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);
  const objects = useGameStore((s) => s.gameState?.objects);

  if (waitingFor?.type !== "MutateMergeChoice") return null;

  return <MutateMergeChoiceModalContent waitingFor={waitingFor} objects={objects} dispatch={dispatch} />;
}

function CipherEncodeModal() {
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);
  const objects = useGameStore((s) => s.gameState?.objects);

  if (waitingFor?.type !== "CipherEncodeChoice") return null;

  return <CipherEncodeChoiceModalContent waitingFor={waitingFor} objects={objects} dispatch={dispatch} />;
}

// ── Untap Choice Modal ─────────────────────────────────────────────────

function UntapChoiceModal() {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);
  const objects = useGameStore((s) => s.gameState?.objects);

  if (waitingFor?.type !== "UntapChoice") return null;

  const objectId = waitingFor.data.candidates[0];
  if (objectId == null) return null;

  const object = objects?.[objectId];
  const name = object?.name ?? t("gamePage.untap.permanentFallback");

  return (
    <ChoiceModal
      title={t("gamePage.untap.title", { name })}
      subtitle={t("gamePage.untap.subtitle")}
      previewCardName={object?.name}
      previewCardTypes={object?.card_types}
      options={[
        {
          id: "untap",
          label: t("gamePage.untap.untap"),
          description: t("gamePage.untap.untapDescription", { name }),
        },
        {
          id: "keep-tapped",
          label: t("gamePage.untap.keepTapped"),
          description: t("gamePage.untap.keepTappedDescription", { name }),
        },
      ]}
      onChoose={(id) =>
        dispatch({
          type: "ChooseUntap",
          data: { object_id: objectId, untap: id === "untap" },
        })
      }
    />
  );
}

// ── Exert Choice Modal (CR 701.43d: exert as it attacks) ────────────────

function ExertChoiceModal() {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);
  const objects = useGameStore((s) => s.gameState?.objects);

  if (waitingFor?.type !== "ExertChoice") return null;

  const objectId = waitingFor.data.attacker;
  const object = objects?.[objectId];
  const name = object?.name ?? t("gamePage.exert.creatureFallback");

  return (
    <ChoiceModal
      title={t("gamePage.exert.title", { name })}
      subtitle={t("gamePage.exert.subtitle")}
      previewCardName={object?.name}
      previewCardTypes={object?.card_types}
      previewObjectId={objectId}
      options={[
        {
          id: "exert",
          label: t("gamePage.exert.exert"),
          description: t("gamePage.exert.exertDescription", { name }),
        },
        {
          id: "decline",
          label: t("gamePage.exert.decline"),
          description: t("gamePage.exert.declineDescription", { name }),
        },
      ]}
      onChoose={(id) =>
        dispatch({
          type: "ChooseExert",
          data: { exert: id === "exert" },
        })
      }
    />
  );
}

// ── Enlist Choice Modal (CR 702.154a: enlist as it attacks) ─────────────

function EnlistChoiceModal() {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);
  const objects = useGameStore((s) => s.gameState?.objects);

  if (waitingFor?.type !== "EnlistChoice") return null;

  const attackerId = waitingFor.data.attacker;
  const attacker = objects?.[attackerId];
  const attackerName = attacker?.name ?? t("gamePage.enlist.attackerFallback");
  const creatureFallback = t("gamePage.enlist.creatureFallback");
  const options = [
    ...waitingFor.data.eligible.map((id) => {
      const enlisted = objects?.[id];
      const name = enlisted?.name ?? creatureFallback;
      return {
        id: String(id),
        label: name,
        description: t("gamePage.enlist.tapDescription", { name, attacker: attackerName }),
      };
    }),
    {
      id: "decline",
      label: t("gamePage.enlist.decline"),
      description: t("gamePage.enlist.declineDescription", { attacker: attackerName }),
    },
  ];

  return (
    <ChoiceModal
      title={t("gamePage.enlist.title", { name: attackerName })}
      subtitle={t("gamePage.enlist.subtitle")}
      previewCardName={attacker?.name}
      previewCardTypes={attacker?.card_types}
      previewObjectId={attackerId}
      options={options}
      onChoose={(id) =>
        dispatch({
          type: "ChooseEnlist",
          data: { target: id === "decline" ? null : Number(id) },
        })
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

function formatUnlessCost(
  cost: { type: string; cost?: { type: string; shards?: string[]; generic?: number }; amount?: number; count?: number },
  t: TFunction<"game">,
): string {
  switch (cost.type) {
    // Legacy `UnlessCost` JSON (pre-2026-05-09 fold) — preserved for
    // saved-game compat.
    case "Fixed":
      return cost.cost ? formatManaCost(cost.cost) : "0";
    case "DiscardCard":
      return t("gamePage.cost.discardCard");
    // Unified `AbilityCost` JSON (post-fold). Used by all newly produced
    // unless-payments, including the per-branch entries of `OneOf`.
    case "Mana":
      return cost.cost ? formatManaCost(cost.cost) : "0";
    case "Discard":
      return t("gamePage.cost.discardCard");
    case "PayLife": {
      const amount = typeof cost.amount === "number"
        ? cost.amount
        : (cost as { amount?: { type: string; value?: number } }).amount?.value ?? 0;
      return t("gamePage.cost.life", { amount });
    }
    case "Sacrifice": {
      const n = cost.count ?? 1;
      return t("gamePage.cost.sacrifice", { count: n });
    }
    case "ReturnToHand": {
      const n = cost.count ?? 1;
      return t("gamePage.cost.returnToHand", { count: n });
    }
    case "PayEnergy":
      return t("gamePage.cost.energy", { amount: cost.amount ?? 0 });
    default:
      return t("gamePage.cost.generic");
  }
}

function UnlessPaymentPanel() {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);

  if (waitingFor?.type !== "UnlessPayment") return null;

  const costDisplay = formatUnlessCost(waitingFor.data.cost, t);
  const description = waitingFor.data.effect_description ?? t("gamePage.unlessPayment.defaultEffect");
  const effect = description.charAt(0).toUpperCase() + description.slice(1);

  return (
    <AnimatePresence>
      <motion.div
        className="fixed inset-x-0 bottom-0 z-40 flex justify-center px-2 pb-4"
        initial={{ y: 80, opacity: 0 }}
        animate={{ y: 0, opacity: 1 }}
        exit={{ y: 80, opacity: 0 }}
        transition={{ duration: 0.25 }}
      >
        <div className="w-full max-w-md rounded-xl border border-white/10 bg-gray-900/95 p-4 shadow-2xl ring-1 ring-gray-700">
          <h3 className="mb-3 text-center text-sm font-semibold text-gray-300">
            <RichLabel
              text={t("gamePage.unlessPayment.title", { effect })}
              size="sm"
            />
          </h3>
          <div className="flex justify-center gap-3">
            <button
              onClick={() =>
                dispatch({ type: "PayUnlessCost", data: { pay: true } })
              }
              className={gameButtonClass({ tone: "emerald", size: "md" })}
            >
              <RichLabel
                text={t("gamePage.cost.pay", { cost: costDisplay })}
                size="sm"
              />
            </button>
            <button
              onClick={() =>
                dispatch({ type: "PayUnlessCost", data: { pay: false } })
              }
              className="rounded-lg bg-gray-700 px-4 py-1.5 text-sm font-semibold text-gray-200 transition hover:bg-gray-600"
            >
              {t("gamePage.unlessPayment.dontPay")}
            </button>
          </div>
        </div>
      </motion.div>
    </AnimatePresence>
  );
}

// CR 118.12a: Disjunctive unless-cost choice \u2014 the player picks **which**
// sub-cost branch to pay (or declines all branches). Mirrors
// `UnlessPaymentModal` but enumerates one option per sub-cost plus a
// decline. Drives Tergrid's Lantern's "sacrifice ... or discard ..."
// punisher pattern.
function UnlessPaymentChooseCostModal() {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);

  if (waitingFor?.type !== "UnlessPaymentChooseCost") return null;

  const costs = waitingFor.data.costs as Array<{
    type: string;
    cost?: { type: string; shards?: string[]; generic?: number };
    amount?: number;
    count?: number;
  }>;
  const description = waitingFor.data.effect_description ?? t("gamePage.unlessPayment.defaultChooseCost");
  const effect = description.charAt(0).toUpperCase() + description.slice(1);

  const branchOptions = costs.map((cost, idx) => ({
    id: String(idx),
    label: formatUnlessCost(cost, t),
  }));

  return (
    <ChoiceModal
      title={t("gamePage.unlessPayment.titleChooseOne", { effect })}
      options={[
        ...branchOptions,
        { id: "decline", label: t("gamePage.unlessPayment.takeEffect") },
      ]}
      onChoose={(id) => {
        // CR 118.12a: Typed `UnlessCostBranch` discriminant — `Decline`
        // falls through to the effect, `Pay { index }` selects the
        // sub-cost. Mirrors the Rust enum exactly.
        const choice =
          id === "decline"
            ? ({ type: "Decline" } as const)
            : ({ type: "Pay", data: { index: Number.parseInt(id, 10) } } as const);
        dispatch({
          type: "ChooseUnlessCostBranch",
          data: { choice },
        });
      }}
    />
  );
}

function ActivationCostOneOfChoiceModal() {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);

  if (waitingFor?.type !== "ActivationCostOneOfChoice") return null;

  const branchOptions = waitingFor.data.costs.map((cost: SerializedAbilityCost, idx: number) => ({
    id: String(idx),
    label: formatAbilityCost(cost),
  }));

  return (
    <ChoiceModal
      title={t("gamePage.activationCost.title")}
      options={branchOptions}
      onChoose={(id) =>
        dispatch({
          type: "ChooseActivationCostBranch",
          data: { index: Number.parseInt(id, 10) },
        })
      }
    />
  );
}

function DebugModeBanner() {
  const { t } = useTranslation("game");
  const active = useUiStore((s) => s.debugInteractionMode);
  const visible = useUiStore((s) => s.debugClickModeButtonVisible);
  const toggle = useUiStore((s) => s.toggleDebugInteractionMode);

  if (!active && !visible) return null;

  return (
    <div className="fixed left-1/2 top-2 z-50 -translate-x-1/2">
      <button
        onClick={toggle}
        className={
          "rounded-full border px-4 py-1.5 font-mono text-xs font-semibold shadow-lg backdrop-blur-sm transition-colors " +
          (active
            ? "border-amber-500/40 bg-amber-950/80 text-amber-300 hover:bg-amber-900/80"
            : "border-gray-600/50 bg-gray-950/75 text-gray-400 hover:border-amber-600/50 hover:text-amber-300")
        }
      >
        {active ? t("gamePage.debug.modeBanner") : t("gamePage.debug.modeButtonOff")}
      </button>
    </div>
  );
}
