import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";

import type { GameFormat } from "../adapter/types";
import { useAudioContext } from "../audio/useAudioContext";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { BrokerOfflinePrompt } from "../components/lobby/BrokerOfflinePrompt";
import { HostSetup } from "../components/lobby/HostSetup";
import type { LobbyGame } from "../components/lobby/GameListItem";
import { JoinErrorDialog } from "../components/lobby/JoinErrorDialog";
import { LobbyView } from "../components/lobby/LobbyView";
import { PlayerIdentityBanner } from "../components/lobby/PlayerIdentityBanner";
import { ServerOfflinePrompt } from "../components/lobby/ServerOfflinePrompt";
import { ConnectionToast } from "../components/multiplayer/ConnectionToast";
import { MenuParticles } from "../components/menu/MenuParticles";
import { MenuShell } from "../components/menu/MenuShell";
import { MyDecks } from "../components/menu/MyDecks";
import { ACTIVE_DECK_KEY, loadActiveDeck, touchDeckPlayed } from "../constants/storage";
import { parseRoomCode, stripPeerIdPrefix } from "../network/connection";
import { evaluateDeckCompatibility } from "../services/deckCompatibility";
import type { LiveCheck } from "./multiplayerPageState";
import { classifyCompatResult } from "./multiplayerPageState";
import { clearWsSession } from "../services/multiplayerSession";
import { useMultiplayerStore } from "../stores/multiplayerStore";
import { useGameStore, saveActiveGame } from "../stores/gameStore";
import type { HostSettings } from "../components/lobby/HostSetup";

type ConnectionMode = "server" | "p2p";
type MultiplayerView = "lobby" | "host-setup" | "deck-select";

type PendingAction =
  | { type: "host"; settings: HostSettings; connectionMode: ConnectionMode }
  | {
      type: "join";
      code: string;
      password?: string;
      format?: GameFormat;
      /**
       * Full lobby row, populated when the join originated from a lobby list
       * click (not from a typed code). Lets the deck-select view render
       * "Joining Alice's Commander game — 2/4" so the user doesn't lose the
       * thread between clicking a game and picking a deck.
       */
      context?: LobbyGame;
    };

export function MultiplayerPage() {
  useAudioContext("lobby");
  const navigate = useNavigate();

  const startHosting = useMultiplayerStore((s) => s.startHosting);
  const startP2PHostingSession = useMultiplayerStore((s) => s.startP2PHostingSession);
  const showToast = useMultiplayerStore((s) => s.showToast);

  const [view, setView] = useState<MultiplayerView>("lobby");
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

  const expandDeck = useCallback(() => {
    const deck = loadActiveDeck();
    if (!deck) return null;
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
    return { main_deck: mainDeck, sideboard };
  }, []);

  const resolveGuestFromStore = useMultiplayerStore((s) => s.resolveGuest);

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
    async (code: string, initialPassword?: string) => {
      let password = initialPassword;
      while (true) {
        const result = await resolveGuestFromStore(code, password);
        if (result.ok) {
          const gameId = crypto.randomUUID();
          useGameStore.setState({ gameId });
          // Broker stores whatever peer id the host registered with
          // (`PEER_ID_PREFIX + roomCode`). `joinRoom(code)` re-prepends
          // the prefix itself, so strip to the bare 5-char code here.
          // `stripPeerIdPrefix` is a no-op on already-bare values, which
          // keeps us compatible with any legacy broker entry that
          // happened to be registered without a prefix.
          const roomCode = stripPeerIdPrefix(result.peerInfo.host_peer_id);
          navigate(`/game/${gameId}?mode=p2p-join&code=${roomCode}`);
          return;
        }
        if (result.reason === "password_required") {
          const entered = window.prompt("This room requires a password:");
          if (!entered) return;
          password = entered;
          continue;
        }
        if (result.reason === "build_mismatch") {
          setJoinErrorDialog({
            title: "Client out of date",
            message: result.message,
            primaryAction: {
              label: "Refresh",
              onClick: () => window.location.reload(),
            },
          });
          return;
        }
        if (
          result.reason === "not_found" ||
          result.reason === "room_full"
        ) {
          setJoinErrorDialog({
            title: "Can't join this room",
            message: result.message,
          });
          return;
        }
        showToast(result.message);
        return;
      }
    },
    [navigate, resolveGuestFromStore, showToast],
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
        showToast("Select a deck before continuing.");
        return false;
      }

      const parsedDeck = loadActiveDeck();
      if (!parsedDeck) {
        showToast("Could not load deck. Try re-importing it.");
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
              ?? `Deck is not legal in ${validationFormat}.`;
            showToast(reason);
            setPendingAction(action);
            setView("deck-select");
            return false;
          }
        } catch (err) {
          showToast(
            err instanceof Error
              ? `Deck check failed: ${err.message}`
              : "Deck check failed.",
          );
          return false;
        }
      }

      touchDeckPlayed(deckName);

      if (action.type === "host") {
        const deck = expandDeck();
        if (!deck) {
          showToast("Could not load deck. Try re-importing it.");
          return false;
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
        // Join flow. Typed codes use `parseRoomCode` for direct dial
        // (peer-code path); lobby-row clicks with `context.is_p2p` are
        // already intercepted upstream in `handleJoinGame` and never
        // reach here.
        const { code, password } = action;

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
        if (password) {
          params.set("password", password);
        }
        navigate(`/game/${gameId}?${params.toString()}`);
      }

      return true;
    },
    [expandDeck, startHosting, startP2PHostingSession, navigate, showToast],
  );

  // Host setup complete → execute immediately if deck exists, otherwise prompt
  const handleHostSetupComplete = useCallback(
    (settings: HostSettings) => {
      const action: PendingAction = { type: "host", settings, connectionMode };
      if (activeDeckName) {
        void executeAction(action);
      } else {
        setPendingAction(action);
        setView("deck-select");
      }
    },
    [connectionMode, activeDeckName, executeAction],
  );

  // Join from lobby → execute immediately if deck exists, otherwise prompt
  const handleJoinGame = useCallback(
    (
      code: string,
      password?: string,
      format?: GameFormat,
      context?: LobbyGame,
    ) => {
      // Explicit `=== true` so legacy lobby rows (older server builds
      // without `is_p2p`) fall through to the server-run flow rather
      // than being treated as P2P. Typed-code joins intentionally do
      // NOT route through the broker — the code a user types is the
      // 5-char peer `roomCode` (shared out of band), not the broker's
      // 6-char `gameCode`. Lobby-row clicks carry `context.is_p2p`
      // and use `joinP2PRoom` for the broker lookup.
      if (context?.is_p2p === true) {
        void joinP2PRoom(code, password);
        return;
      }
      const action: PendingAction = {
        type: "join",
        code,
        password,
        format,
        context,
      };
      // Always route through deck-select on join so the user picks against
      // the host's format-filtered list. Even with an active deck, that
      // deck may be illegal in the host's format — better to surface the
      // mismatch immediately in the filtered picker than to fail server-
      // side after we've already navigated into the game.
      setPendingAction(action);
      setView("deck-select");
    },
    [joinP2PRoom],
  );

  const handleBack = () => {
    if (view === "deck-select") {
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
      ? "Join or host a table."
      : view === "host-setup"
        ? "Set up your table."
        : "Choose a deck.";

  const description =
    view === "lobby"
      ? "Browse available tables, join by code, or host a new match."
      : view === "host-setup"
        ? "Adjust format, privacy, and timing before opening the room."
        : selectedFormat
          ? `Pick a deck for ${selectedFormat}.`
          : "Pick the deck you want to bring online.";

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <MenuParticles />
      <ScreenChrome
        onBack={handleBack}
        settingsOpen={showSettings}
        onSettingsOpenChange={setShowSettings}
      />
      <div className="menu-scene__vignette" />
      <div className="menu-scene__sigil menu-scene__sigil--left" />
      <div className="menu-scene__sigil menu-scene__sigil--right" />
      <div className="menu-scene__haze" />

      <MenuShell eyebrow="Multiplayer" title={title} description={description} layout="stacked">
        <div className="flex w-full flex-col items-center">
        {/* Player identity — always available on lobby/host-setup so users
            can edit their name without hunting in Preferences. */}
        {(view === "lobby" || view === "host-setup") && <PlayerIdentityBanner />}

        {/* Active deck indicator — host-setup only. Deck commitment is
            meaningless at the lobby level because no format is chosen yet;
            joining a table picks the deck against the host's format via
            the deck-select view. */}
        {view === "host-setup" && activeDeckName && (
          <div className="mx-auto mb-4 flex w-full max-w-xl items-center justify-between gap-3 rounded-[16px] border border-white/8 bg-black/16 px-4 py-2.5">
            <div className="min-w-0">
              <div className="text-[0.6rem] uppercase tracking-[0.22em] text-slate-500">
                Active Deck
              </div>
              <div className="truncate text-sm font-medium text-white">{activeDeckName}</div>
            </div>
            <button
              onClick={() => {
                setDeckSelectReturn(view as MultiplayerView);
                setPendingAction(null);
                setView("deck-select");
              }}
              className="shrink-0 text-xs text-slate-400 transition-colors hover:text-white"
            >
              Change
            </button>
          </div>
        )}

        {view === "host-setup" && activeDeckName && liveCheck.status !== "idle" && (
          <DeckLegalityChip check={liveCheck} />
        )}

        {/* No deck warning — host-setup only, for the same reason as above. */}
        {view === "host-setup" && !activeDeckName && (
          <div className="mx-auto mb-4 flex w-full max-w-xl items-center justify-between gap-3 rounded-[16px] border border-amber-500/20 bg-amber-500/8 px-4 py-2.5">
            <span className="text-xs text-amber-200">
              No deck selected — you'll need to pick one before hosting.
            </span>
            <button
              onClick={() => {
                setDeckSelectReturn(view as MultiplayerView);
                setView("deck-select");
              }}
              className="shrink-0 rounded-lg border border-amber-400/20 bg-amber-400/10 px-3 py-1 text-xs font-medium text-amber-200 transition-colors hover:bg-amber-400/18"
            >
              Pick Deck
            </button>
          </div>
        )}

        {view === "lobby" && (
          <LobbyView
            key={lobbyRetryKey}
            onHostGame={() => { setConnectionMode("server"); setView("host-setup"); }}
            onHostP2P={() => { setConnectionMode("p2p"); setView("host-setup"); }}
            onJoinGame={handleJoinGame}
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
                ? `Deck is not legal in ${liveCheck.format}.`
                : liveCheck.status === "checking"
                  ? "Checking deck legality…"
                  : undefined
            }
          />
        )}

        {view === "deck-select" && (
          <>
            {pendingAction?.type === "join" && pendingAction.context && (
              <div className="mx-auto mb-4 w-full max-w-xl rounded-[16px] border border-cyan-400/20 bg-cyan-500/[0.07] px-4 py-2.5">
                <div className="text-[0.6rem] uppercase tracking-[0.22em] text-cyan-300/70">
                  Joining
                </div>
                <div className="mt-1 text-sm text-cyan-100">
                  <span className="font-medium">
                    {pendingAction.context.host_name || "Anonymous"}
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
                showToast("Could not load deck. Try re-importing it.");
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

function DeckLegalityChip({ check }: { check: LiveCheck }) {
  if (check.status === "idle") return null;

  const base =
    "mx-auto mb-4 flex w-full max-w-xl items-start gap-3 rounded-[16px] border px-4 py-2.5";

  if (check.status === "checking") {
    return (
      <div className={`${base} border-white/8 bg-black/16`}>
        <span className="text-xs text-slate-400">
          Checking deck against {check.format}…
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
          ✓ Legal in {check.format}
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
        Not legal in {check.format}
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
