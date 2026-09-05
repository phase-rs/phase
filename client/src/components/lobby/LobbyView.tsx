import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type { FormatGroup, GameFormat } from "../../adapter/types";
import { FORMAT_REGISTRY } from "../../data/formatRegistry";
import { flagForServer, parseJoinCode } from "../../services/serverDetection";
import { healthHint, refreshServerDirectory } from "../../services/serverDirectory";
import {
  FORMAT_DEFAULTS,
  adHocLobbySource,
  compareLobbyGameEntries,
  findLobbyGameByCode,
  hostingLobbySource,
  isLobbyEntryCompatible,
  lobbySources,
  useMultiplayerStore,
  type LobbyGameEntry,
  type LobbySource,
} from "../../stores/multiplayerStore";
import { assertNever } from "../../utils/assertNever";
import { MenuPanel } from "../menu/MenuShell";
import { menuButtonClass } from "../menu/buttonStyles";
import { GameListItem } from "./GameListItem";
import type { LobbyGame } from "./GameListItem";
import { ServerFlag } from "./ServerFlag";
import { ServerPicker } from "./ServerPicker";
import { MenuSelect } from "../ui/MenuSelect";

interface LobbyViewProps {
  onHostGame: () => void;
  onHostP2P: () => void;
  onHostDraft?: () => void;
  /**
   * Called when the user elects to join a game. `origin` is the authority
   * the join must open on — the source that listed the row, the host named
   * in a `CODE@host` code, or the hosting server for a bare typed code;
   * `null` only for a direct P2P code, which has no lobby authority at all.
   * `context` is the full `LobbyGame` row when the join originates from the
   * lobby list, so downstream views (e.g. the deck picker) can render
   * "Joining Alice's Commander game — 2/4". It is absent for typed-code joins.
   */
  onJoinGame: (
    code: string,
    origin: LobbySource | null,
    password?: string,
    format?: GameFormat,
    context?: LobbyGame,
  ) => void;
  /** Watch a live server game or draft without joining as a player. */
  onSpectate?: (code: string, origin: LobbySource | null, context?: LobbyGame) => void;
  connectionMode?: "server" | "p2p";
  onServerOffline?: () => void;
}

// <optgroup> render order for the format filter <select>. New engine
// FormatGroup variants become a TS exhaustiveness error here.
const FILTER_GROUP_ORDER: Record<FormatGroup, number> = {
  Constructed: 0,
  Commander: 1,
  Limited: 2,
  Multiplayer: 3,
};

const FORMAT_FILTER_GROUPS = (Object.keys(FILTER_GROUP_ORDER) as FormatGroup[])
  .sort((a, b) => FILTER_GROUP_ORDER[a] - FILTER_GROUP_ORDER[b])
  .map((group) => ({
    group,
    items: FORMAT_REGISTRY.filter((m) => m.group === group),
  }))
  .filter((g) => g.items.length > 0);

const FILTER_ALL_SENTINEL = "__all__";

type RoomTypeFilter = "all" | "p2p" | "server" | "draft";

const ROOM_TYPE_FILTERS: { value: RoomTypeFilter; labelKey: string }[] = [
  { value: "all", labelKey: "lobbyView.roomTypeAll" },
  { value: "draft", labelKey: "lobbyView.roomTypeDraft" },
  { value: "p2p", labelKey: "lobbyView.roomTypeP2P" },
  { value: "server", labelKey: "lobbyView.roomTypeServer" },
];

export function LobbyView({
  onHostGame,
  onHostP2P,
  onHostDraft,
  onJoinGame,
  onSpectate,
  connectionMode,
  onServerOffline,
}: LobbyViewProps) {
  const { t } = useTranslation("multiplayer");
  const isServer = connectionMode !== "p2p";
  const isP2P = connectionMode === "p2p";
  const hostingServer = useMultiplayerStore((s) => s.hostingServer);
  const userLobbySources = useMultiplayerStore((s) => s.userLobbySources);
  const sourceStatus = useMultiplayerStore((s) => s.sourceStatus);
  const showToast = useMultiplayerStore((s) => s.showToast);
  // Flag for the connected region, or null for self-hosted/custom servers.
  const serverFlag = flagForServer(hostingServer ?? "");
  const directorySources = useMultiplayerStore((s) => s.directorySources);
  const disabledDirectorySources = useMultiplayerStore(
    (s) => s.disabledDirectorySources,
  );
  const sources = useMemo(
    () =>
      lobbySources({
        userLobbySources,
        sourceStatus,
        directorySources,
        disabledDirectorySources,
      }),
    [userLobbySources, sourceStatus, directorySources, disabledDirectorySources],
  );
  /**
   * Membership, not decoration. A directory refresh that only moves a score
   * must not tear down and re-attach every channel's listener — which sends
   * `UnsubscribeLobby`/`SubscribeLobby` on every socket and blanks each cached
   * snapshot. Same rule that keeps `sourceStatus` out of the subscription
   * effect's dependency list, and a strict improvement on depending on
   * `userLobbySources`: a no-op replacement of that array no longer churns
   * subscriptions either.
   */
  const dialedSourceKey = useMemo(
    () => sources.map((s) => s.url).join("|"),
    [sources],
  );
  /** Latest snapshot per source URL, carrying the source it was delivered
   * with. Kept per-source rather than merged so a silent or degraded
   * authority never blanks the others' rows. */
  const [listings, setListings] = useState<
    Map<string, { source: LobbySource; games: LobbyGame[] }>
  >(new Map());
  const [joinCode, setJoinCode] = useState("");
  const [passwordModal, setPasswordModal] = useState<{
    gameCode: string;
    /** The authority this game is listed on — the password retry must go to
     * the same server the row came from. */
    origin: LobbySource | null;
    format?: GameFormat;
    /** Full lobby row when click came from the list — propagates into
     * the join handler as deck-picker context. */
    context?: LobbyGame;
  } | null>(null);
  const [passwordInput, setPasswordInput] = useState("");
  const [formatFilter, setFormatFilter] = useState<GameFormat | null>(null);
  const [roomTypeFilter, setRoomTypeFilter] = useState<RoomTypeFilter>("all");
  const [serverPickerOpen, setServerPickerOpen] = useState(false);
  const subscribeLobby = useMultiplayerStore((s) => s.subscribeLobby);
  const subscribeAmbientLobby = useMultiplayerStore(
    (s) => s.subscribeAmbientLobby,
  );
  const setFormatConfig = useMultiplayerStore((s) => s.setFormatConfig);
  const hostGameCode = useMultiplayerStore((s) => s.hostGameCode);

  // If the user is browsing a specific format and clicks Host, seed the
  // host-setup form with that format — they were clearly looking for that
  // game type. Falls back to whatever format the store already remembers
  // when no filter is active. Mirrors the same store channel HostSetup
  // already reads from on mount, so no new props or prop threading.
  const handleHost = useCallback(() => {
    if (formatFilter) {
      setFormatConfig(FORMAT_DEFAULTS[formatFilter]);
    }
    onHostGame();
  }, [formatFilter, setFormatConfig, onHostGame]);

  useEffect(() => {
    // P2P mode uses a direct PeerJS code and has no lobby to subscribe to.
    if (isP2P) return;

    let cancelled = false;
    let lobbyDetach: (() => void) | null = null;

    // `PlayerCount` and reactive `PasswordRequired` are ambient on each
    // source's subscription socket, beside the `LobbyUpdate` family. The
    // store owns those sockets and re-attaches its listener to the new one
    // every reconnect produces, so subscribing to its fan-out — rather than
    // binding to a `ws` here — is what keeps this view live across a flap:
    // a listener bound to one socket goes permanently deaf the first time
    // its source drops. Registered synchronously, before the dial below, so
    // a fast source's first frame is never missed.
    const detachAmbient = subscribeAmbientLobby((frame, source) => {
      switch (frame.kind) {
        case "playerCount":
          // Recorded by the store on this source's status row; the chip
          // reads it from there, where it cannot outlive its socket.
          return;
        case "passwordRequired": {
          // Reactive fallback: the proactive path in `handleJoinFromList`
          // opens the modal before any server round-trip, so this only
          // fires for stale rows where the client thought the room was
          // open and the server said otherwise. Every field comes from the
          // authority the frame arrived on: `game_code` is unique per
          // authority, not across the merged list, so an unscoped rescan
          // could name a server that never asked for a password, and its
          // row would then route the join (a `draft_metadata` row sends
          // `MultiplayerPage` down the draft flow) on the wrong authority.
          const listed = findLobbyGameByCode(frame.gameCode, source.url);
          setPasswordModal({
            gameCode: frame.gameCode,
            origin: source,
            format: listed?.game.format,
            context: listed?.game,
          });
          setPasswordInput("");
          return;
        }
        default:
          // The site that makes `AmbientLobbyFrame`'s exhaustiveness promise
          // real. This is the union's only consumer, and a `void` callback
          // swallows an unhandled `kind` silently, so without this arm a new
          // broker frame would type-check and then be dropped by the view.
          return assertNever(frame);
      }
    });

    // Delegate lobby traffic to the shared per-source subscription sockets
    // owned by `multiplayerStore`. The store re-handshakes on drops, re-sends
    // `SubscribeLobby` on reconnect, and fans out each source's `LobbyUpdate`
    // snapshots tagged with the source that listed them — removing the
    // duplicate handshake this component previously maintained.
    (async () => {
      const detach = await subscribeLobby((games, source) => {
        if (cancelled) return;
        setListings((prev) => new Map(prev).set(source.url, { source, games }));
      });
      if (cancelled) {
        detach?.();
        return;
      }
      // `null` means every source failed; a single degraded authority leaves
      // the rest browsable and never raises the offline prompt.
      if (detach === null) {
        onServerOffline?.();
        return;
      }
      lobbyDetach = detach;
    })();

    return () => {
      cancelled = true;
      detachAmbient();
      lobbyDetach?.();
    };
    // Depends on the dialed set's MEMBERSHIP (`dialedSourceKey`), never on
    // `sourceStatus` — a status flap must not churn subscriptions.
  }, [isP2P, dialedSourceKey, subscribeLobby, subscribeAmbientLobby, onServerOffline]);

  useEffect(() => {
    // Not at app boot, for the same reason the subscription sockets are not: a
    // player who never opens multiplayer pays for nothing. Failures are silent
    // by contract — the lobby simply lists presets and hand-added sources. The
    // TTL in `serverDirectory.ts` is what makes a remount cheap.
    if (isP2P) return;
    void refreshServerDirectory();
  }, [isP2P]);

  useEffect(() => {
    // A lobby left open in a background tab goes stale: the listing and each
    // row's stored verdict otherwise refresh only on a remount. Returning to
    // the tab is the cheapest correct trigger — and it cannot storm the
    // endpoint, because `refreshServerDirectory` self-guards on both its TTL
    // and its in-flight promise. A timer would fire in a backgrounded tab and
    // buy nothing this does not.
    if (isP2P) return;
    const onVisibilityChange = () => {
      if (document.visibilityState === "visible") void refreshServerDirectory();
    };
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, [isP2P]);

  /**
   * Each browsed source's raw score components, keyed by the CLIENT url a row
   * carries — the join `GameListItem` cannot make for itself, since a
   * `LobbyGameEntry` holds the collapsed `LobbySource.score` number and never
   * the `WireScore` a hint has to read.
   *
   * Built here because this component already owns the merged list and already
   * subscribes to `directorySources`; a selector in the leaf would re-render
   * every row on any directory change and put a data join in a presentational
   * component.
   */
  const hintByUrl = useMemo(
    () =>
      new Map(
        directorySources.map((entry) => [entry.source.url, healthHint(entry.row.score)]),
      ),
    [directorySources],
  );

  const handleJoinFromList = useCallback(
    (entry: LobbyGameEntry) => {
      const { game, source } = entry;
      // Proactive password prompt: if the lobby row advertises a password,
      // open the modal before any server round-trip. The reactive
      // `PasswordRequired` handler above remains as a fallback for stale
      // rows (server says yes when the client thought no).
      if (game.has_password) {
        setPasswordModal({
          gameCode: game.game_code,
          origin: source,
          format: game.format,
          context: game,
        });
        setPasswordInput("");
        return;
      }
      onJoinGame(game.game_code, source, undefined, game.format, game);
    },
    [onJoinGame],
  );

  /**
   * The authority a typed code belongs to. `CODE@host` names its own — a
   * one-off origin that is browsed by nobody and changes no stored setting.
   * A bare code belongs to whichever source listed it, else to the hosting
   * server. `{ ok: false }` means the typed address is malformed.
   */
  const resolveTypedOrigin = useCallback(
    (
      code: string,
      address?: string,
    ): { ok: true; origin: LobbySource | null } | { ok: false } => {
      if (address !== undefined) {
        const origin = adHocLobbySource(address);
        return origin ? { ok: true, origin } : { ok: false };
      }
      return {
        ok: true,
        origin:
          findLobbyGameByCode(code)?.source
          ?? hostingLobbySource(useMultiplayerStore.getState()),
      };
    },
    [],
  );

  const handleJoinByCode = useCallback(() => {
    const raw = joinCode.trim();
    if (!raw) return;

    // Uppercase the CODE segment only: the address half carries a scheme and
    // host whose meaning is case-sensitive (`ws://`, `localhost`), and
    // uppercasing the whole string destroys both.
    const parsed = parseJoinCode(raw);
    const code = parsed.code.toUpperCase();
    const resolved = resolveTypedOrigin(code, parsed.serverAddress);
    if (!resolved.ok) {
      showToast(t("lobbyView.invalidJoinServer"));
      return;
    }
    onJoinGame(code, resolved.origin);
  }, [joinCode, onJoinGame, resolveTypedOrigin, showToast, t]);

  const handleSpectateByCode = useCallback(() => {
    const raw = joinCode.trim();
    if (!raw || !onSpectate) return;
    const parsed = parseJoinCode(raw);
    const code = parsed.code.toUpperCase();
    const resolved = resolveTypedOrigin(code, parsed.serverAddress);
    if (!resolved.ok) {
      showToast(t("lobbyView.invalidJoinServer"));
      return;
    }
    // Context scoped to the resolved authority: the row that decides the
    // draft-vs-game route must come from the server being watched, not from
    // a colliding code on another source. A `null` origin (hosting "None")
    // is refused by `onSpectate` before the context is ever read.
    onSpectate(code, resolved.origin, findLobbyGameByCode(code, resolved.origin?.url)?.game);
  }, [joinCode, onSpectate, resolveTypedOrigin, showToast, t]);

  const handlePasswordSubmit = useCallback((e: React.FormEvent) => {
    e.preventDefault();
    if (passwordModal && passwordInput) {
      onJoinGame(
        passwordModal.gameCode,
        passwordModal.origin,
        passwordInput,
        passwordModal.format,
        passwordModal.context,
      );
      setPasswordModal(null);
      setPasswordInput("");
    }
  }, [passwordModal, passwordInput, onJoinGame]);

  // Only show the room-type segmented filter when the visible list is
  // actually mixed. On a single-purpose deploy (all-P2P or all-server)
  // the control is noise, and hiding it matches the "don't add UI without
  // clear value" bar. Compared via `=== true` so absent/undefined entries
  // (older server builds pre-`is_p2p`) count as server-run, not unknown.
  // Show the room-type filter (All / Draft / P2P / Server) whenever any tables
  // are listed — matching the design's persistent filter row. Still hidden on a
  // genuinely empty lobby, where it would filter nothing.
  // One merged, ordered list across every source: official rows first, then
  // by source score, then longest-waiting table. A snapshot from a source the
  // user has since removed is dropped rather than rendered without an origin.
  const entries = useMemo(
    () =>
      [...listings.values()]
        .filter(({ source }) => sources.some((s) => s.url === source.url))
        .flatMap(({ source, games }) => games.map((game) => ({ game, source })))
        .sort(compareLobbyGameEntries),
    [listings, sources],
  );

  const showRoomTypeFilter = entries.length > 0;

  const filteredEntries = useMemo(() => {
    return entries.filter(({ game: g }) => {
      if (formatFilter && (g.format ?? "Standard") !== formatFilter) return false;
      if (roomTypeFilter === "draft" && g.draft_metadata == null) return false;
      if (roomTypeFilter === "p2p" && g.is_p2p !== true) return false;
      if (roomTypeFilter === "server" && g.is_p2p === true) return false;
      return true;
    });
  }, [entries, formatFilter, roomTypeFilter]);

  // Every enabled source reports its own online count; the chip shows the
  // total reach of the lobby the user is browsing. Two rules, both
  // structural:
  //   membership — summed over the CURRENT sources (as in `entries`), so a
  //     source the user has removed leaves the total immediately;
  //   liveness   — the count is read from that source's status row, which
  //     the store rewrites on every connection state change and refills
  //     only from a frame on the socket that is live now. A source that is
  //     reconnecting, offline, or freshly re-opened without having reported
  //     since therefore contributes nothing; there is no cached number here
  //     that could outlive the socket that sent it.
  const playerCount = useMemo(
    () =>
      sources.reduce(
        (sum, source) => sum + (sourceStatus.get(source.url)?.playerCount ?? 0),
        0,
      ),
    [sources, sourceStatus],
  );

  // Count-free by design: the picker lists each source with its own status,
  // which is where a number would be actionable.
  const anyDegraded = sources.some(
    (source) => sourceStatus.get(source.url)?.state === "offline",
  );

  const formatMenuGroups = useMemo(
    () =>
      FORMAT_FILTER_GROUPS.map(({ group, items }) => ({
        label: group,
        items: items.map((m) => ({ value: m.format, label: m.label })),
      })),
    [],
  );

  const formatMenuLabel = formatFilter
    ? (FORMAT_REGISTRY.find((m) => m.format === formatFilter)?.label ?? formatFilter)
    : t("lobbyView.allFormats");

  const serverHost = (hostingServer ?? "").replace(/^wss?:\/\//, "").split("/")[0];

  return (
    <MenuPanel className="relative z-10 flex w-full max-w-3xl flex-col gap-6 px-5 py-6">
      <div className="flex w-full flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">
          {isP2P ? t("lobbyView.directConnection") : t("lobbyView.onlineLobby")}
        </div>
        <div className="flex min-w-0 flex-wrap items-center gap-2 sm:justify-end">
          {isServer && (
            <button
              type="button"
              onClick={() => setServerPickerOpen(true)}
              title={hostingServer ?? ""}
              className="flex min-w-0 max-w-full items-center gap-1.5 rounded-[7px] border border-white/10 bg-black/25 px-2.5 py-0.5 font-mono text-[10px] text-slate-300 backdrop-blur-sm transition-colors hover:border-white/20 hover:bg-white/5"
            >
              {serverFlag && (
                <ServerFlag
                  flag={serverFlag}
                  className="h-2.5 w-auto shrink-0 rounded-[1px] ring-1 ring-black/20"
                />
              )}
              <span className="truncate whitespace-nowrap">{serverHost}</span>
            </button>
          )}
          {/* In P2P mode the user has no other path back to ServerPicker —
              the server-address chip above is hidden, and ServerOfflinePrompt
              only fires when we tried to use a server. Offer an explicit
              affordance so users who picked "P2P only" aren't trapped. */}
          {isP2P && (
            <button
              type="button"
              onClick={() => setServerPickerOpen(true)}
              title={t("lobbyView.pickServerTitle")}
              className="rounded-[7px] border border-white/10 bg-black/25 px-2.5 py-0.5 text-[10px] text-slate-300 backdrop-blur-sm transition-colors hover:border-white/20 hover:bg-white/5"
            >
              {t("lobbyView.pickServer")}
            </button>
          )}
          {isServer && playerCount > 0 && (
            <span className="rounded-[7px] border border-emerald-300/20 bg-emerald-500/15 px-2.5 py-0.5 text-xs font-medium text-emerald-200">
              {t("lobbyView.online", { count: playerCount })}
            </span>
          )}
          {isServer && anyDegraded && (
            <button
              type="button"
              onClick={() => setServerPickerOpen(true)}
              className="rounded-[7px] border border-amber-300/20 bg-amber-500/15 px-2.5 py-0.5 text-xs font-medium text-amber-200 transition-colors hover:bg-amber-500/25"
            >
              {t("lobbyView.sourcesDegraded")}
            </button>
          )}
        </div>
      </div>

      {/* Format filter — MenuSelect opens a bottom sheet below 820px (shell tab
          bar width) so the long format roster never covers the lobby form.
          Desktop keeps the anchored dropdown. min-h-44px + text-base meet the
          44/48px touch-target rule and prevent iOS focus-zoom. */}
      {isServer && (
        <div className="flex min-h-[44px] w-full items-center gap-2 self-stretch rounded-[10px] border border-white/10 bg-black/25 px-3 py-1 shadow-[0_8px_22px_rgba(0,0,0,0.18)] backdrop-blur-sm sm:w-auto sm:self-start">
          <span className="shrink-0 text-[0.62rem] font-medium uppercase tracking-[0.18em] text-gray-500">
            {t("lobbyView.format")}
          </span>
          <MenuSelect
            ariaLabel={t("lobbyView.format")}
            label={formatMenuLabel}
            selectedValue={formatFilter ?? FILTER_ALL_SENTINEL}
            items={[{ value: FILTER_ALL_SENTINEL, label: t("lobbyView.allFormats") }]}
            groups={formatMenuGroups}
            onSelect={(value) =>
              setFormatFilter(
                value === FILTER_ALL_SENTINEL ? null : (value as GameFormat),
              )
            }
            wrapperClassName="min-w-0 flex-1 sm:min-w-[10rem]"
            className="min-h-[44px] rounded-none border-0 bg-transparent px-0 py-1.5 text-base font-medium text-white shadow-none hover:bg-transparent focus-visible:ring-white/20"
          />
        </div>
      )}

      {isServer && showRoomTypeFilter && (
        <div className="flex rounded-[10px] border border-white/10 bg-black/25 p-1 shadow-[0_8px_22px_rgba(0,0,0,0.18)] backdrop-blur-sm">
          {ROOM_TYPE_FILTERS.map((opt) => (
            <button
              key={opt.value}
              onClick={() => setRoomTypeFilter(opt.value)}
              className={`rounded-[7px] px-3 py-1 text-xs font-medium transition-colors ${
                roomTypeFilter === opt.value
                  ? "bg-white/12 text-white"
                  : "text-gray-400 hover:bg-white/5 hover:text-gray-200"
              }`}
            >
              {t(opt.labelKey)}
            </button>
          ))}
        </div>
      )}

      {isServer && (
        <div className="w-full space-y-3">
          <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">{t("lobbyView.openTables")}</div>
          {filteredEntries.length === 0 ? (
            <div className="flex flex-col items-center gap-3 rounded-[10px] border border-dashed border-white/10 bg-black/12 px-4 py-6 text-center backdrop-blur-sm">
              <p className="text-sm text-gray-400">
                {formatFilter
                  ? t("lobbyView.noFormatGames", { format: formatFilter })
                  : t("lobbyView.noOpenGames")}
              </p>
              {formatFilter && (
                <button
                  type="button"
                  onClick={() => setFormatFilter(null)}
                  className={menuButtonClass({ tone: "neutral", size: "sm" })}
                >
                  {t("lobbyView.showAllFormats")}
                </button>
              )}
            </div>
          ) : (
            <div className="flex max-h-64 flex-col gap-2 overflow-y-auto">
              {filteredEntries.map((entry) => (
                <GameListItem
                  // Keyed by source too: `game_code` is unique per authority,
                  // not across the merged multi-source list.
                  key={`${entry.source.url}:${entry.game.game_code}`}
                  entry={entry}
                  onJoin={handleJoinFromList}
                  compatible={isLobbyEntryCompatible(entry.game.host_build_commit)}
                  hostGameCode={hostGameCode}
                  healthHint={hintByUrl.get(entry.source.url) ?? null}
                />
              ))}
            </div>
          )}
        </div>
      )}

      {isP2P && (
        <div className="w-full rounded-[10px] border border-cyan-400/20 bg-cyan-500/[0.07] px-4 py-3 text-sm leading-6 text-cyan-100 shadow-[0_8px_22px_rgba(0,0,0,0.18)] backdrop-blur-sm">
          {t("lobbyView.p2pNotice")}
        </div>
      )}

      <div className="w-full space-y-3">
        <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">
          {isP2P ? t("lobbyView.joinByCode") : t("lobbyView.joinATable")}
        </div>
        <div className="flex w-full flex-col gap-2 sm:flex-row sm:items-center">
          <input
            type="text"
            value={joinCode}
            onChange={(e) => setJoinCode(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleJoinByCode()}
            placeholder={isP2P ? t("lobbyView.p2pCodePlaceholder") : t("lobbyView.serverCodePlaceholder")}
            maxLength={isP2P ? 5 : 50}
            className="min-w-0 flex-1 rounded-[8px] border border-white/10 bg-black/25 px-4 py-2 font-mono text-sm tracking-wider text-white placeholder-gray-500 outline-none backdrop-blur-sm focus:border-white/20"
          />
          <div className="flex w-full shrink-0 items-center gap-2 sm:w-auto">
            <button
              onClick={handleJoinByCode}
              disabled={!joinCode.trim()}
              className={menuButtonClass({
                tone: "cyan",
                size: "sm",
                disabled: !joinCode.trim(),
                className: "flex-1 sm:flex-none",
              })}
            >
              {t("lobbyView.join")}
            </button>
            {isServer && onSpectate && (
              <button
                type="button"
                onClick={handleSpectateByCode}
                disabled={!joinCode.trim()}
                className={menuButtonClass({
                  tone: "neutral",
                  size: "sm",
                  disabled: !joinCode.trim(),
                  className: "flex-1 sm:flex-none",
                })}
              >
                {t("lobbyView.watch")}
              </button>
            )}
          </div>
        </div>
      </div>

      <div className="flex w-full flex-col gap-3 border-t border-white/8 pt-4 sm:flex-row sm:items-center sm:justify-between">
        <div className="min-w-0">
          <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">{t("lobbyView.host")}</div>
          <div className="mt-1 text-sm text-slate-400">
            {isP2P ? t("lobbyView.hostP2PDescription") : t("lobbyView.hostServerDescription")}
          </div>
        </div>
        <div className="flex items-center gap-2">
          {onHostDraft && (
            <button
              onClick={onHostDraft}
              className={menuButtonClass({ tone: "purple", size: "md" })}
            >
              {t("lobbyView.hostDraft")}
            </button>
          )}
          {isServer && (
            <button
              onClick={handleHost}
              className={menuButtonClass({ tone: "emerald", size: "md" })}
            >
              {t("lobbyView.hostGame")}
            </button>
          )}
          {isP2P && (
            <button
              onClick={onHostP2P}
              className={menuButtonClass({ tone: "cyan", size: "md" })}
            >
              {t("lobbyView.hostP2PGame")}
            </button>
          )}
        </div>
      </div>

      {serverPickerOpen && (
        <ServerPicker onClose={() => setServerPickerOpen(false)} />
      )}

      {/* Password modal */}
      {passwordModal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div
            className="absolute inset-0 bg-black/60"
            onClick={() => setPasswordModal(null)}
          />
          <div className="relative z-10 w-full max-w-xs rounded-[10px] border border-white/10 bg-[#0b1020]/96 p-6 shadow-2xl backdrop-blur-md">
            <h3 className="mb-3 text-sm font-semibold text-white">
              {t("lobbyView.passwordRequired")}
            </h3>
            <form onSubmit={handlePasswordSubmit}>
              <input
                type="password"
                value={passwordInput}
                onChange={(e) => setPasswordInput(e.target.value)}
                placeholder={t("lobbyView.passwordPlaceholder")}
                className="mb-4 w-full rounded-lg bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 outline-none ring-1 ring-gray-700 focus:ring-cyan-500"
                autoFocus
              />
              <div className="flex justify-end gap-2">
                <button
                  type="button"
                  onClick={() => setPasswordModal(null)}
                  className={menuButtonClass({ tone: "neutral", size: "sm" })}
                >
                  {t("common:actions.cancel")}
                </button>
                <button
                  type="submit"
                  disabled={!passwordInput}
                  className={menuButtonClass({
                    tone: "cyan",
                    size: "sm",
                    disabled: !passwordInput,
                  })}
                >
                  {t("lobbyView.join")}
                </button>
              </div>
            </form>
          </div>
        </div>
      )}
    </MenuPanel>
  );
}
