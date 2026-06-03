import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useLocation, useNavigate } from "react-router";

import type { GameFormat } from "../adapter/types";
import { useAudioContext } from "../audio/useAudioContext";
import { DiscordBadge } from "../components/chrome/DiscordBadge";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { useInShell } from "../components/chrome/ShellContext";
import { BrokerOfflinePrompt } from "../components/lobby/BrokerOfflinePrompt";
import { HostSetup } from "../components/lobby/HostSetup";
import type { LobbyGame } from "../components/lobby/GameListItem";
import { JoinErrorDialog } from "../components/lobby/JoinErrorDialog";
import { LobbyView } from "../components/lobby/LobbyView";
import { PlayerIdentityBanner } from "../components/lobby/PlayerIdentityBanner";
import { ServerOfflinePrompt } from "../components/lobby/ServerOfflinePrompt";
import { ConnectionToast } from "../components/multiplayer/ConnectionToast";
import { MenuParticles } from "../components/menu/MenuParticles";
import { MenuPanel, MenuShell } from "../components/menu/MenuShell";
import { menuButtonClass } from "../components/menu/buttonStyles";
import { MyDecks } from "../components/menu/MyDecks";
import { ACTIVE_DECK_KEY, loadActiveDeck, touchDeckPlayed } from "../constants/storage";
import { parseRoomCode, stripPeerIdPrefix } from "../network/connection";
import { evaluateDeckCompatibility } from "../services/deckCompatibility";
import { expandParsedDeck } from "../services/deckParser";
import type { LiveCheck, MultiplayerView } from "./multiplayerPageState";
import { classifyCompatResult } from "./multiplayerPageState";
import { clearWsSession } from "../services/multiplayerSession";
import { findLobbyGameByCode, useMultiplayerStore } from "../stores/multiplayerStore";
import {
  useMultiplayerDraftStore,
  type MultiplayerDraftPhase,
} from "../stores/multiplayerDraftStore";
import { useGameStore, saveActiveGame } from "../stores/gameStore";
import { useCardDataStore } from "../stores/cardDataStore";
import type { HostSettings } from "../components/lobby/HostSetup";

type ConnectionMode = "server" | "p2p";

function parseViewParam(value: string | null): MultiplayerView {
  if (value === "host-setup" || value === "deck-select" || value === "draft-lobby") return value;
  return "lobby";
}

type PendingAction =
  | { type: "host"; settings: HostSettings; connectionMode: ConnectionMode }
  | {
      type: "join";
      code: string;
      password?: string;
      format?: GameFormat;
      isP2P?: boolean;
      reservationToken?: string | null;
      /**
       * Full lobby row, populated when the join originated from a lobby list
       * click (not from a typed code). Lets the deck-select view render
       * "Joining Alice's Commander game — 2/4" so the user doesn't lose the
       * thread between clicking a game and picking a deck.
       */
      context?: LobbyGame;
    };

export function MultiplayerPage() {
  const { t } = useTranslation("multiplayer");
  useAudioContext("lobby");
  const navigate = useNavigate();
  const location = useLocation();
  // In the shell the rail's footer owns the Discord link; the page's fixed
  // top-left badge would collide with the left-aligned eyebrow, so drop it.
  const embedded = useInShell();

  // Warm the shared card DB so host-setup deck legality checks are instant.
  // Idempotent; closes the deep-link hole when opening /multiplayer directly.
  useEffect(() => {
    void useCardDataStore.getState().warm();
  }, []);

  const startHosting = useMultiplayerStore((s) => s.startHosting);
  const startP2PHostingSession = useMultiplayerStore((s) => s.startP2PHostingSession);
  const showToast = useMultiplayerStore((s) => s.showToast);

  const draftPhase = useMultiplayerDraftStore((s) => s.phase);
  const draftRoomCode = useMultiplayerDraftStore((s) => s.roomCode);
  const joinDraft = useMultiplayerDraftStore((s) => s.joinDraft);
  const leaveDraft = useMultiplayerDraftStore((s) => s.leave);

  const [view, setView] = useState<MultiplayerView>(() => (
    parseViewParam(new URLSearchParams(location.search).get("view"))
  ));
  const [activeDeckName, setActiveDeckName] = useState<string | null>(null);
  // Initial mode tracks `serverAddress`: if the user has picked "None" in
  // `ServerPicker` (empty string sentinel), skip straight to P2P so the
  // lobby doesn't attempt a doomed subscription.
  const initialServerAddress = useMultiplayerStore.getState().serverAddress;
  const [connectionMode, setConnectionMode] = useState<ConnectionMode>(
    initialServerAddress ? "server" : "p2p",
  );
  const [showSettings, setShowSettings] = useState(false);
  const [pendingAction, setPendingAction] = useState<PendingAction | null>(null);
  // Shown when `LobbyView` detects the server is unreachable. The user picks
  // between staying in server mode (LobbyView remounts via `lobbyRetryKey` and
  // retries) or flipping to P2P for direct-code play. Tracked on this page,
  // not in the store, because it's scoped to the Multiplayer flow.
  const [serverOfflinePrompt, setServerOfflinePrompt] = useState(false);
  const [lobbyRetryKey, setLobbyRetryKey] = useState(0);
  // Set when the user clicks "Host online game" on a `LobbyOnly` server but
  // the broker isn't reachable. Stashes the pending action so the modal's
  // "Continue without lobby" button can dispatch it with `useBroker: false`.
  const [brokerOfflinePrompt, setBrokerOfflinePrompt] = useState<
    { action: PendingAction } | null
  >(null);
  // Fatal guest-side errors (build mismatch especially) need more weight
  // than a transient toast — the user may need to act (refresh the page
  // to pick up a new build). State is null when no error is displayed.
  const [joinErrorDialog, setJoinErrorDialog] = useState<
    {
      title: string;
      message: string;
      primaryAction?: { label: string; onClick: () => void };
    } | null
  >(null);
  // Where to return when the user enters deck-select *without* a pending
  // host/join action (i.e. clicked the "Change" affordance on the active-
  // deck banner). Before this, back/confirm both assumed pendingAction
  // was set, so leaving deck-select dumped the user into the lobby even
  // when they came from host-setup — and from lobby, another back escaped
  // multiplayer entirely.
  const [deckSelectReturn, setDeckSelectReturn] =
    useState<MultiplayerView>("lobby");
  const serverAddress = useMultiplayerStore((s) => s.serverAddress);
  // HostSetup mirrors its in-flight format into the store on every change,
  // so reading it here lets both the deck-picker filter and the live
  // compatibility check react to the user's format choice without any
  // cross-component plumbing.
  const storeFormatConfig = useMultiplayerStore((s) => s.formatConfig);
  // Live deck-vs-format compatibility state, rendered as a chip under the
  // Active Deck banner on host-setup. `idle` suppresses the chip entirely
  // (no deck, no format, or not on host-setup). Evaluation runs through
  // the engine — the frontend never decides legality itself.
  const [liveCheck, setLiveCheck] = useState<LiveCheck>({ status: "idle" });

  useEffect(() => {
    setActiveDeckName(localStorage.getItem(ACTIVE_DECK_KEY));
  }, []);

  useEffect(() => {
    const state = location.state as {
      deckRejected?: boolean;
      reason?: string;
      format?: string;
      joinCode?: string;
    } | null;
    if (!state?.deckRejected) return;
    showToast(state.reason ?? t("page.deckRejected"));
    setPendingAction({
      type: "join",
      code: state.joinCode ?? "",
      format: (state.format as GameFormat) ?? undefined,
    });
    setView("deck-select");
    navigate(location.pathname, { replace: true, state: null });
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // Sync connectionMode when the user changes their server address via
  // ServerPicker. Empty address → P2P (no server to talk to). Restored
  // address → server (selecting a server IS the explicit intent). Only
  // reacts to serverAddress changes — not connectionMode — so an explicit
  // "Use Direct Code" selection isn't immediately reversed.
  useEffect(() => {
    if (!serverAddress) {
      setConnectionMode("p2p");
    } else {
      setConnectionMode("server");
    }
  }, [serverAddress]);

  // Live legality check: whenever the user is on host-setup with an active
  // deck and a chosen format, re-run the engine's compatibility check after
  // a short debounce. The debounce absorbs rapid format clicks so we don't
  // fire a WASM call per keypress-equivalent.
  useEffect(() => {
    if (view !== "host-setup" || !activeDeckName) {
      setLiveCheck({ status: "idle" });
      return;
    }
    const format = storeFormatConfig?.format;
    if (!format) {
      setLiveCheck({ status: "idle" });
      return;
    }
    const deck = loadActiveDeck();
    if (!deck) {
      setLiveCheck({ status: "idle" });
      return;
    }

    setLiveCheck({ status: "checking", format });
    let cancelled = false;
    const handle = window.setTimeout(() => {
      evaluateDeckCompatibility(deck, { selectedFormat: format })
        .then((result) => {
          if (cancelled) return;
          setLiveCheck(classifyCompatResult(format, result));
        })
        .catch(() => {
          if (!cancelled) setLiveCheck({ status: "idle" });
        });
    }, 250);

    return () => {
      cancelled = true;
      window.clearTimeout(handle);
    };
  }, [view, activeDeckName, storeFormatConfig?.format]);

  // In deck-select, tapping a deck tile IS the confirmation — there's no
  // other use for the screen since we don't show deck contents. We persist
  // the choice, then either execute the pending host/join action or return
  // to wherever the user triggered the "Change" affordance from.
  const handleSelectDeck = (name: string) => {
    setActiveDeckName(name);
    localStorage.setItem(ACTIVE_DECK_KEY, name);

    // Only auto-advance out of deck-select. When this handler fires from
    // other views (e.g. adopting an imported deck), we don't want to
    // navigate; we're just recording the active-deck choice.
    if (view !== "deck-select") return;

    if (pendingAction) {
      const action = pendingAction;
      void executeAction(action).then((ok) => {
        if (ok) setPendingAction(null);
      });
      return;
    }
    setView(deckSelectReturn);
  };

  const handleEditDeck = useCallback((name: string) => {
    const returnParams = new URLSearchParams(location.search);
    if (view === "lobby") {
      returnParams.delete("view");
    } else {
      returnParams.set("view", view);
    }
    const returnSearch = returnParams.toString();
    const returnTo = `${location.pathname}${returnSearch ? `?${returnSearch}` : ""}`;
    const fmt = pendingAction?.type === "host"
      ? pendingAction.settings.formatConfig.format
      : pendingAction?.type === "join"
        ? pendingAction.format
        : storeFormatConfig?.format;
    const formatParam = fmt ? `&format=${fmt.toLowerCase()}` : "";
    navigate(
      `/deck-builder?deck=${encodeURIComponent(name)}${formatParam}&returnTo=${encodeURIComponent(returnTo)}`,
    );
  }, [location.pathname, location.search, navigate, pendingAction, storeFormatConfig, view]);

  const expandDeck = useCallback(() => {
    const deck = loadActiveDeck();
    if (!deck) return null;
    return expandParsedDeck(deck);
  }, []);

  const resolveGuestFromStore = useMultiplayerStore((s) => s.resolveGuest);
  const lookupJoinTargetFromStore = useMultiplayerStore((s) => s.lookupJoinTarget);

  /**
   * Guest-path P2P resolve loop. Tries `resolveGuest` over the shared
   * subscription socket, prompts for a password on `password_required`
   * and retries on the same socket, surfaces explicit UI for
   * `build_mismatch` / `connection_lost` / etc., and navigates on
   * success. No `throw`-based control flow: failures come back as a
   * discriminated `ResolveResult`.
   *
   * Declared above `executeAction` so the deck-select → re-dispatch
   * path can route LobbyOnly joins through the broker too. `setJoinErrorDialog`
   * is referenced as an identifier (stable across renders via React).
   */
  const joinP2PRoom = useCallback(
    async (
      code: string,
      initialPassword?: string,
      reservationToken?: string | null,
    ): Promise<boolean> => {
      let password = initialPassword;
      while (true) {
        const result = await resolveGuestFromStore(code, password, { reservationToken });
        if (result.ok) {
          const gameId = crypto.randomUUID();
          useGameStore.setState({ gameId });
          const roomCode = stripPeerIdPrefix(result.peerInfo.host_peer_id);
          if (result.peerInfo.reservation_token) {
            window.sessionStorage.setItem(
              `phase-p2p-reservation:${roomCode}`,
              result.peerInfo.reservation_token,
            );
          }
          navigate(`/game/${gameId}?mode=p2p-join&code=${roomCode}`);
          return true;
        }
        if (result.reason === "password_required") {
          const entered = window.prompt(t("page.passwordPrompt"));
          if (!entered) return false;
          password = entered;
          continue;
        }
        if (result.reason === "build_mismatch") {
          setJoinErrorDialog({
            title: t("page.joinErrorOutOfDateTitle"),
            message: result.message,
            primaryAction: {
              label: t("page.joinErrorRefresh"),
              onClick: () => window.location.reload(),
            },
          });
          return false;
        }
        if (
          result.reason === "not_found" ||
          result.reason === "room_full"
        ) {
          setJoinErrorDialog({
            title: t("page.joinErrorCantJoinTitle"),
            message: result.message,
          });
          return false;
        }
        showToast(result.message);
        return false;
      }
    },
    [navigate, resolveGuestFromStore, showToast, t],
  );

  // Execute a pending action (host or join) with the currently active deck.
  //
  // Before routing, we validate the active deck against the chosen format
  // via the engine's `evaluateDeckCompatibility` (the only authority on
  // legality). If the deck fails, we surface the first engine-provided
  // reason as a toast and push the user to deck-select so they can pick a
  // compatible deck — rather than letting them host/join and fail server-
  // side after the room is already open.
  const executeAction = useCallback(
    async (action: PendingAction): Promise<boolean> => {
      const deckName = localStorage.getItem(ACTIVE_DECK_KEY);
      if (!deckName) {
        showToast(t("page.selectDeckFirst"));
        return false;
      }

      const parsedDeck = loadActiveDeck();
      if (!parsedDeck) {
        showToast(t("page.couldNotLoadDeck"));
        return false;
      }

      const validationFormat: GameFormat | undefined =
        action.type === "host"
          ? action.settings.formatConfig.format
          : action.format;

      if (validationFormat) {
        try {
          const compat = await evaluateDeckCompatibility(parsedDeck, {
            selectedFormat: validationFormat,
          });
          if (compat.selected_format_compatible === false) {
            const reason =
              compat.selected_format_reasons[0]
              ?? t("page.deckNotLegal", { format: validationFormat });
            showToast(reason);
            setPendingAction(action);
            setView("deck-select");
            return false;
          }
        } catch (err) {
          showToast(
            err instanceof Error
              ? t("page.deckCheckFailed", { error: err.message })
              : t("page.deckCheckFailedGeneric"),
          );
          return false;
        }
      }

      touchDeckPlayed(deckName);

      if (action.type === "host") {
        const deck = expandDeck();
        if (!deck) {
          showToast(t("page.couldNotLoadDeck"));
          return false;
        }

        const opponentCount = Math.max(0, action.settings.formatConfig.max_players - 1);
        const aiSeatIndexes = new Set(action.settings.aiSeats.map((seat) => seat.seatIndex));
        const allOpponentsAreAi =
          opponentCount > 0
          && Array.from({ length: opponentCount }, (_, i) => i + 1)
            .every((seatIndex) => aiSeatIndexes.has(seatIndex));
        if (allOpponentsAreAi) {
          const sortedAiSeats = [...action.settings.aiSeats]
            .sort((a, b) => a.seatIndex - b.seatIndex);
          const aiSeats = sortedAiSeats.map((seat) => ({
            difficulty: seat.difficulty,
            deckName: seat.deckName,
          }));
          const headDifficulty = aiSeats[0]?.difficulty ?? "Medium";
          const gameId = crypto.randomUUID();
          clearWsSession();
          saveActiveGame({
            id: gameId,
            mode: "ai",
            difficulty: headDifficulty,
            aiSeats,
            formatConfig: action.settings.formatConfig,
          });
          useGameStore.setState({ gameId });
          navigate(
            `/game/${gameId}?mode=ai&difficulty=${headDifficulty}&format=${action.settings.formatConfig.format}&players=${action.settings.formatConfig.max_players}&match=${action.settings.matchType.toLowerCase()}`,
          );
          return true;
        }

        // Reachability + mode check for the hosting flow. We lean on the
        // store's long-lived subscription socket (opened when the user
        // entered this page) rather than paying a fresh broker handshake:
        // `ensureSubscriptionSocket` is idempotent and returns `null` when
        // the server is unreachable, which is exactly the signal the
        // `BrokerOfflinePrompt` needs. This also populates `serverInfo` on
        // the store so the mode check has authoritative data even on a
        // fresh page load. A `LobbyOnly` server doesn't run games — it
        // only brokers P2P peer IDs — so a user who clicked "Host Game"
        // (server mode) against such a server is implicitly asking for a
        // broker-advertised P2P game.
        const store = useMultiplayerStore.getState();
        const socket = await store.ensureSubscriptionSocket();
        const mode = socket?.serverInfo.mode ?? store.serverInfo?.mode;

        if (action.connectionMode === "p2p" || mode === "LobbyOnly") {
          if (mode === "LobbyOnly" && !socket) {
            setBrokerOfflinePrompt({ action });
            return false;
          }
          const ok = await startP2PHostingSession(action.settings, deck, {
            useBroker: mode === "LobbyOnly",
            roomName: action.settings.roomName,
          });
          if (!ok) {
            return false;
          }
          navigate("/");
        } else {
          // Server-mode host: if the server is unreachable, surface the
          // offline prompt and offer a P2P fallback rather than handing
          // the action off to `startHosting`, which would hang on the WS
          // handshake and leave the user staring at the host-setup screen.
          if (!socket) {
            setBrokerOfflinePrompt({ action });
            return false;
          }
          startHosting(action.settings, deck);
          navigate("/");
        }
      } else {
        const { code, password, context } = action;

        if (context?.is_p2p === true || action.isP2P === true) {
          return joinP2PRoom(code, password, action.reservationToken);
        }

        const p2pCode = parseRoomCode(code);
        if (p2pCode && code.trim().length === 5) {
          const gameId = crypto.randomUUID();
          useGameStore.setState({ gameId });
          navigate(`/game/${gameId}?mode=p2p-join&code=${p2pCode}`);
          return true;
        }

        clearWsSession();
        const gameId = crypto.randomUUID();
        saveActiveGame({ id: gameId, mode: "online", difficulty: "" });
        useGameStore.setState({ gameId });
        const params = new URLSearchParams({ mode: "join", code });
        if (action.reservationToken) {
          window.sessionStorage.setItem(
            `phase-join-reservation:${code}`,
            action.reservationToken,
          );
        }
        if (password) {
          params.set("password", password);
        }
        navigate(`/game/${gameId}?${params.toString()}`);
      }

      return true;
    },
    [expandDeck, startHosting, startP2PHostingSession, navigate, showToast, joinP2PRoom, t],
  );

  // Host setup complete → execute immediately if deck exists, otherwise prompt
  const handleHostSetupComplete = useCallback(
    async (settings: HostSettings): Promise<boolean> => {
      const action: PendingAction = { type: "host", settings, connectionMode };
      if (activeDeckName) {
        return executeAction(action);
      }
      setPendingAction(action);
      setView("deck-select");
      return true;
    },
    [connectionMode, activeDeckName, executeAction],
  );

  // Navigate to draft setup page. The multiplayer draft page handles its
  // own set selection and pod configuration — we just route the user there.
  const handleHostDraft = useCallback(() => {
    navigate("/draft?mode=multiplayer");
  }, [navigate]);

  // Join a draft pod from the lobby. Draft entries carry `draft_metadata`
  // and are always P2P — the guest joins via PeerJS room code.
  const handleJoinDraftFromLobby = useCallback(
    async (code: string, _context?: LobbyGame) => {
      const playerName = useMultiplayerStore.getState().displayName ?? "Player";
      try {
        await joinDraft({ roomCode: code, displayName: playerName });
        setView("draft-lobby");
      } catch {
        showToast(t("page.failedToJoinDraft"));
      }
    },
    [joinDraft, showToast, t],
  );

  const handleSpectate = useCallback(
    async (code: string, context?: LobbyGame) => {
      const resolved = context ?? findLobbyGameByCode(code);
      if (resolved?.draft_metadata) {
        navigate(`/draft-spectator?code=${encodeURIComponent(code)}`);
        return;
      }
      // Typed codes skip lobby-row context; drafts not in the public lobby
      // still resolve via SpectateDraft when lookup reports not_found.
      if (!resolved?.draft_metadata && connectionMode === "server") {
        const lookup = await lookupJoinTargetFromStore(code);
        if (!lookup.ok && lookup.reason === "not_found") {
          navigate(`/draft-spectator?code=${encodeURIComponent(code)}`);
          return;
        }
        if (!lookup.ok) {
          showToast(lookup.message);
          return;
        }
      }
      const gameId = crypto.randomUUID();
      useGameStore.setState({ gameId });
      navigate(`/game/${gameId}?mode=spectate&code=${encodeURIComponent(code)}`);
    },
    [navigate, connectionMode, lookupJoinTargetFromStore, showToast],
  );

  // Join from lobby → execute immediately if deck exists, otherwise prompt
  const handleJoinGame = useCallback(
    async (
      code: string,
      password?: string,
      format?: GameFormat,
      context?: LobbyGame,
    ) => {
      // Draft entries bypass the normal join-with-deck flow entirely — draft
      // pods handle their own deck building after the draft completes.
      if (context?.draft_metadata) {
        void handleJoinDraftFromLobby(code, context);
        return;
      }

      const trimmedCode = code.trim();
      const directP2PCode = parseRoomCode(trimmedCode);

      // Raw 5-character room codes are direct PeerJS joins with no server
      // metadata to query. Preserve the old flow and skip lookup entirely.
      if (!format && !context && directP2PCode && trimmedCode.length === 5) {
        setPendingAction({
          type: "join",
          code,
          password,
          format,
        });
        setView("deck-select");
        return;
      }

      // Typed-code path (no lobby-row context) uses the read-only
      // `LookupJoinTarget` RPC so the deck picker can filter by format
      // without accidentally consuming a seat on Full servers.
      let resolvedFormat = format;
      let resolvedPassword = password;
      let resolvedIsP2P = context?.is_p2p === true;
      let reservationToken: string | null = null;
      const reserveOptions = {
        reserve: true,
        displayName: useMultiplayerStore.getState().displayName || "Player",
      };
      const result = await lookupJoinTargetFromStore(code, resolvedPassword, reserveOptions);
      if (result.ok) {
        resolvedFormat = result.info.format_config?.format ?? resolvedFormat;
        resolvedIsP2P = result.info.is_p2p;
        reservationToken = result.info.reservation_token ?? null;
      } else if (result.reason === "password_required") {
        const entered = window.prompt(t("page.passwordPrompt"));
        if (!entered) return;
        resolvedPassword = entered;
        const retry = await lookupJoinTargetFromStore(code, resolvedPassword, reserveOptions);
        if (retry.ok) {
          resolvedFormat = retry.info.format_config?.format ?? resolvedFormat;
          resolvedIsP2P = retry.info.is_p2p;
          reservationToken = retry.info.reservation_token ?? null;
        } else {
          showToast(retry.message);
          return;
        }
      } else {
        showToast(result.message);
        return;
      }
      const action: PendingAction = {
        type: "join",
        code,
        password: resolvedPassword,
        format: resolvedFormat,
        isP2P: resolvedIsP2P,
        reservationToken,
        context,
      };
      setPendingAction(action);
      setView("deck-select");
    },
    [lookupJoinTargetFromStore, handleJoinDraftFromLobby, showToast, t],
  );

  const handleBack = () => {
    if (view === "deck-select") {
      if (pendingAction?.type === "join" && pendingAction.reservationToken) {
        void lookupJoinTargetFromStore(
          pendingAction.code,
          pendingAction.password,
          { releaseReservationToken: pendingAction.reservationToken },
        );
      }
      // With a pending action the user clearly came from a host/join
      // attempt; without one they came from the "Change Deck" affordance,
      // and `deckSelectReturn` remembers which view rendered that button.
      setView(
        pendingAction?.type === "host"
          ? "host-setup"
          : pendingAction?.type === "join"
            ? "lobby"
            : deckSelectReturn,
      );
      return;
    }
    if (view === "host-setup") {
      setView("lobby");
      return;
    }
    if (view === "draft-lobby") {
      void leaveDraft();
      setView("lobby");
      return;
    }
    navigate("/");
  };

  // Derive the format the deck picker filters by.
  //
  // The happy paths (host-submit-without-deck, join-from-lobby-row) carry
  // the format on `pendingAction`. When the user clicks "Change Deck" out
  // of host-setup, `pendingAction` is null — we fall back to the same
  // `storeFormatConfig` the live-check effect uses above.
  const selectedFormat: GameFormat | undefined =
    pendingAction?.type === "host"
      ? pendingAction.settings.formatConfig.format
      : pendingAction?.type === "join"
        ? pendingAction.format
        : storeFormatConfig?.format;

  const title =
    view === "lobby"
      ? t("page.titleLobby")
      : view === "host-setup"
        ? t("page.titleHostSetup")
        : view === "draft-lobby"
          ? t("page.titleDraftLobby")
          : t("page.titleDeckSelect");

  const description =
    view === "lobby"
      ? t("page.descriptionLobby")
      : view === "host-setup"
        ? t("page.descriptionHostSetup")
        : view === "draft-lobby"
          ? t("page.descriptionDraftLobby")
          : selectedFormat
            ? t("page.descriptionDeckSelectFormat", { format: selectedFormat })
            : t("page.descriptionDeckSelect");

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      {/* Inside the shell the scene + particles are rendered once by AppShell;
          only paint our own when standalone, matching the other embedded panes. */}
      {!embedded && <MenuParticles />}
      <ScreenChrome
        onBack={handleBack}
        settingsOpen={showSettings}
        onSettingsOpenChange={setShowSettings}
      />
      {!embedded && (
        <div className="fixed left-20 top-[calc(env(safe-area-inset-top)+1rem)] z-20 flex h-11 items-center">
          <DiscordBadge />
        </div>
      )}
      <div className="menu-scene__vignette" />
      <div className="menu-scene__sigil menu-scene__sigil--left" />
      <div className="menu-scene__sigil menu-scene__sigil--right" />
      <div className="menu-scene__haze" />

      <MenuShell
        eyebrow={t("page.eyebrow")}
        title={title}
        description={description}
        layout="stacked"
        contentWidthClass={view === "host-setup" ? "max-w-4xl" : "max-w-3xl"}
      >
        <div className="flex w-full flex-col items-start">
        {/* Player identity — always available on lobby/host-setup so users
            can edit their name without hunting in Preferences. */}
        {(view === "lobby" || view === "host-setup") && <PlayerIdentityBanner />}

        {/* Active deck indicator — host-setup only. Deck commitment is
            meaningless at the lobby level because no format is chosen yet;
            joining a table picks the deck against the host's format via
            the deck-select view. */}
        {view === "host-setup" && activeDeckName && (
          <div className="mb-4 flex w-full max-w-3xl items-center justify-between gap-3 rounded-[16px] border border-white/8 bg-black/16 px-4 py-2.5">
            <div className="min-w-0">
              <div className="text-[0.6rem] uppercase tracking-[0.22em] text-slate-500">
                {t("page.activeDeck")}
              </div>
              <div className="truncate text-sm font-medium text-white">{activeDeckName}</div>
            </div>
            <div className="flex shrink-0 items-center gap-3">
              <button
                onClick={() => handleEditDeck(activeDeckName)}
                className="text-xs text-slate-400 transition-colors hover:text-white"
              >
                {t("page.edit")}
              </button>
              <button
                onClick={() => {
                  setDeckSelectReturn(view as MultiplayerView);
                  setPendingAction(null);
                  setView("deck-select");
                }}
                className="text-xs text-slate-400 transition-colors hover:text-white"
              >
                {t("page.change")}
              </button>
            </div>
          </div>
        )}

        {view === "host-setup" && activeDeckName && liveCheck.status !== "idle" && (
          <DeckLegalityChip check={liveCheck} />
        )}

        {/* No deck warning — host-setup only, for the same reason as above. */}
        {view === "host-setup" && !activeDeckName && (
          <div className="mb-4 flex w-full max-w-3xl items-center justify-between gap-3 rounded-[16px] border border-amber-500/20 bg-amber-500/8 px-4 py-2.5">
            <span className="text-xs text-amber-200">
              {t("page.noDeckWarning")}
            </span>
            <button
              onClick={() => {
                setDeckSelectReturn(view as MultiplayerView);
                setView("deck-select");
              }}
              className="shrink-0 rounded-lg border border-amber-400/20 bg-amber-400/10 px-3 py-1 text-xs font-medium text-amber-200 transition-colors hover:bg-amber-400/18"
            >
              {t("page.pickDeck")}
            </button>
          </div>
        )}

        {view === "lobby" && (
          <LobbyView
            // Remount on server change so local lobby state (playerCount,
            // game list) resets and the subscription effect re-runs against
            // the freshly-dialed socket — without this, switching servers
            // left the previous region's PlayerCount on screen. lobbyRetryKey
            // still drives the "Keep waiting" offline retry.
            key={`${serverAddress}:${lobbyRetryKey}`}
            onHostGame={() => { setConnectionMode("server"); setView("host-setup"); }}
            onHostP2P={() => { setConnectionMode("p2p"); setView("host-setup"); }}
            onHostDraft={handleHostDraft}
            onJoinGame={handleJoinGame}
            onSpectate={connectionMode === "server" ? handleSpectate : undefined}
            connectionMode={connectionMode}
            onServerOffline={() => {
              // Only prompt when we're actually trying to use the server; if
              // the user already flipped to P2P the "unreachable" state is
              // expected and not worth interrupting.
              if (connectionMode === "server") {
                setServerOfflinePrompt(true);
              }
            }}
          />
        )}

        {view === "host-setup" && (
          <HostSetup
            onHost={handleHostSetupComplete}
            onBack={() => setView("lobby")}
            connectionMode={connectionMode}
            hostDisabled={liveCheck.status === "illegal" || liveCheck.status === "checking"}
            hostDisabledReason={
              liveCheck.status === "illegal"
                ? t("page.deckNotLegal", { format: liveCheck.format })
                : liveCheck.status === "checking"
                  ? t("deckLegalityChip.checkingLegality")
                  : undefined
            }
          />
        )}

        {view === "draft-lobby" && (
          <DraftLobbyPanel
            phase={draftPhase}
            roomCode={draftRoomCode}
            onLeave={() => {
              void leaveDraft();
              setView("lobby");
            }}
          />
        )}

        {view === "deck-select" && (
          <>
            {pendingAction?.type === "join" && pendingAction.context && (
              <div className="mb-4 w-full max-w-3xl rounded-[16px] border border-cyan-400/20 bg-cyan-500/[0.07] px-4 py-2.5">
                <div className="text-[0.6rem] uppercase tracking-[0.22em] text-cyan-300/70">
                  {t("page.joining")}
                </div>
                <div className="mt-1 text-sm text-cyan-100">
                  <span className="font-medium">
                    {pendingAction.context.host_name || t("page.anonymous")}
                  </span>
                  {pendingAction.context.format && (
                    <span className="text-cyan-200/70">
                      {" "}· {pendingAction.context.format}
                    </span>
                  )}
                  {pendingAction.context.max_players != null && (
                    <span className="text-cyan-200/70">
                      {" "}· {pendingAction.context.current_players ?? 1}/
                      {pendingAction.context.max_players}
                    </span>
                  )}
                </div>
              </div>
            )}
            {/* No `onConfirmSelection` / `confirmLabel` — clicking a deck
                tile IS the confirmation. `handleSelectDeck` saves the
                choice and either executes the pending action or returns
                to the caller view in one step. */}
            <MyDecks
              mode="select"
              selectedFormat={selectedFormat}
              onSelectDeck={handleSelectDeck}
              onEditDeck={handleEditDeck}
              activeDeckName={activeDeckName}
            />
          </>
        )}

        </div>
      </MenuShell>
      <ConnectionToast />
      {serverOfflinePrompt && view === "lobby" && (
        <ServerOfflinePrompt
          serverAddress={serverAddress}
          onUseDirect={() => {
            setConnectionMode("p2p");
            setServerOfflinePrompt(false);
          }}
          onKeepWaiting={() => {
            setServerOfflinePrompt(false);
            // Force LobbyView to unmount + remount with a fresh WebSocket
            // connection attempt. All local state in LobbyView resets, which
            // is intentional — we want a clean retry.
            setLobbyRetryKey((k) => k + 1);
          }}
        />
      )}
      {brokerOfflinePrompt && (
        <BrokerOfflinePrompt
          serverAddress={serverAddress}
          onCancel={() => setBrokerOfflinePrompt(null)}
          onContinueWithoutLobby={() => {
            const { action } = brokerOfflinePrompt;
            setBrokerOfflinePrompt(null);
            if (action.type === "host") {
              const deck = expandDeck();
              if (!deck) {
                showToast(t("page.couldNotLoadDeck"));
                return;
              }
              void startP2PHostingSession(action.settings, deck, {
                useBroker: false,
                roomName: action.settings.roomName,
              }).then((ok) => {
                if (ok) navigate("/");
              });
            }
          }}
        />
      )}
      {joinErrorDialog && (
        <JoinErrorDialog
          title={joinErrorDialog.title}
          message={joinErrorDialog.message}
          primaryAction={joinErrorDialog.primaryAction}
          onDismiss={() => setJoinErrorDialog(null)}
        />
      )}
    </div>
  );
}

// ── Draft Lobby Panel ─────────────────────────────────────────────────
//
// Minimal inline panel shown when the user has joined (as guest) a
// multiplayer draft pod. Displays connection status, room code, and a
// leave button. The full draft UI lives on the DraftPage; this panel is
// a holding area while waiting in the pod lobby.

function DraftLobbyPanel({
  phase,
  roomCode,
  onLeave,
}: {
  phase: MultiplayerDraftPhase;
  roomCode: string | null;
  onLeave: () => void;
}) {
  const { t } = useTranslation("multiplayer");
  const seats = useMultiplayerDraftStore((s) => s.seats);
  const joined = useMultiplayerDraftStore((s) => s.joined);
  const total = useMultiplayerDraftStore((s) => s.total);
  const error = useMultiplayerDraftStore((s) => s.error);

  return (
    <MenuPanel className="relative z-10 flex w-full max-w-3xl flex-col gap-5 px-5 py-6">
      <div className="flex items-center justify-between">
        <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">
          {t("draftLobbyPanel.draftPod")}
        </div>
        {roomCode && (
          <span className="rounded-full border border-white/10 bg-black/18 px-2.5 py-0.5 font-mono text-xs tracking-wider text-purple-400">
            {roomCode}
          </span>
        )}
      </div>

      {phase === "connecting" && (
        <div className="text-sm text-slate-400">{t("draftLobbyPanel.connecting")}</div>
      )}

      {phase === "error" && (
        <div className="rounded-[16px] border border-rose-400/20 bg-rose-500/[0.07] px-4 py-3 text-sm text-rose-200">
          {error ?? t("draftLobbyPanel.connectionFailed")}
        </div>
      )}

      {(phase === "lobby" || phase === "connecting") && total > 0 && (
        <div className="flex flex-col gap-3">
          <div className="text-sm text-slate-300">
            {t("draftLobbyPanel.playersJoined", { joined, total })}
          </div>
          <div className="flex flex-wrap gap-2">
            {seats.map((seat, i) => (
              <div
                key={i}
                className={`rounded-lg border px-3 py-1.5 text-xs ${
                  seat.display_name
                    ? "border-purple-400/20 bg-purple-500/[0.07] text-purple-200"
                    : "border-white/8 bg-black/16 text-slate-500"
                }`}
              >
                {seat.display_name || t("draftLobbyPanel.seat", { number: i + 1 })}
              </div>
            ))}
          </div>
        </div>
      )}

      {phase === "drafting" && (
        <div className="text-sm text-emerald-300">
          {t("draftLobbyPanel.draftInProgress")}
        </div>
      )}

      <button
        onClick={onLeave}
        className={menuButtonClass({ tone: "neutral", size: "sm" })}
      >
        {t("draftLobbyPanel.leaveDraft")}
      </button>
    </MenuPanel>
  );
}

function DeckLegalityChip({ check }: { check: LiveCheck }) {
  const { t } = useTranslation("multiplayer");
  if (check.status === "idle") return null;

  const base =
    "mb-4 flex w-full max-w-3xl items-start gap-3 rounded-[16px] border px-4 py-2.5";

  if (check.status === "checking") {
    return (
      <div className={`${base} border-white/8 bg-black/16`}>
        <span className="text-xs text-slate-400">
          {t("deckLegalityChip.checking", { format: check.format })}
        </span>
      </div>
    );
  }

  if (check.status === "legal") {
    return (
      <div
        className={`${base} border-emerald-400/20 bg-emerald-500/[0.07]`}
        role="status"
      >
        <span className="text-xs font-medium text-emerald-200">
          {t("deckLegalityChip.legal", { format: check.format })}
        </span>
      </div>
    );
  }

  // illegal — surface up to the first two reasons from the engine so the
  // user knows why before they try to host.
  const reasons = check.reasons.slice(0, 2);
  return (
    <div
      className={`${base} flex-col border-rose-400/20 bg-rose-500/[0.07]`}
      role="alert"
    >
      <div className="text-xs font-medium text-rose-200">
        {t("deckLegalityChip.notLegal", { format: check.format })}
      </div>
      {reasons.length > 0 && (
        <ul className="mt-1 list-inside list-disc text-[11px] leading-5 text-rose-200/80">
          {reasons.map((reason, i) => (
            <li key={i}>{reason}</li>
          ))}
        </ul>
      )}
    </div>
  );
}
