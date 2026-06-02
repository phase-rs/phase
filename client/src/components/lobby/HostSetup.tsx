import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import type { FormatConfig, FormatGroup, GameFormat, MatchType } from "../../adapter/types";
import { FORMAT_REGISTRY } from "../../data/formatRegistry";
import { FORMAT_DEFAULTS, useMultiplayerStore } from "../../stores/multiplayerStore";
import type { AiSeatConfig, HostingSettings } from "../../stores/multiplayerStore";
import { useAiDeckCatalog } from "../../services/aiDeckCatalog";
import { expandParsedDeck } from "../../services/deckParser";
import { MenuPanel } from "../menu/MenuShell";
import { menuButtonClass } from "../menu/buttonStyles";

export type { AiSeatConfig };
export type HostSettings = HostingSettings;

interface HostSetupProps {
  onHost: (settings: HostSettings) => void | Promise<boolean>;
  onBack: () => void;
  connectionMode: "server" | "p2p";
  /** When true, the host-submit button is disabled (e.g. live deck check
   * says the active deck is illegal for the chosen format, or a check is
   * still in flight). The parent surfaces the *reason* via the legality
   * chip above the form, so this only needs to gate the submit itself. */
  hostDisabled?: boolean;
  hostDisabledReason?: string;
}

// Format options derive from the engine-authored FORMAT_REGISTRY so new
// formats added in `crates/engine/src/types/format.rs` flow through to this
// picker automatically. Two-Headed Giant is intentionally absent from the
// registry (team-based play unsupported), so it never appears here either.
const FORMAT_OPTIONS: { format: GameFormat; label: string; description: string; group: FormatGroup }[] = FORMAT_REGISTRY.map((m) => ({
  format: m.format,
  label: m.label,
  description: m.description,
  group: m.group,
}));

// <optgroup> render order for the format <select>. New engine FormatGroup
// variants become a TS exhaustiveness error here.
const GROUP_ORDER: Record<FormatGroup, number> = {
  Constructed: 0,
  Commander: 1,
  Limited: 2,
  Multiplayer: 3,
};

const DIFFICULTY_OPTIONS = ["VeryEasy", "Easy", "Medium", "Hard", "VeryHard"];
const FFA_DECK_SIZE_OPTIONS = [60, 40] as const;

/** P2P's WebRTC mesh supports 2-4 peers (see `p2p-adapter.ts:165`). The
 * HostSetup UI clamps format player counts to this ceiling so multi-seat
 * formats like Commander can still be hosted while 6-player FreeForAll
 * can't advertise an unreachable configuration. */
const P2P_MAX_PEERS = 4;

export function HostSetup({
  onHost,
  onBack,
  connectionMode,
  hostDisabled = false,
  hostDisabledReason,
}: HostSetupProps) {
  const { t } = useTranslation("multiplayer");
  // Player name is edited in `PlayerIdentityBanner` above this form (see
  // MultiplayerPage). We read it here only to submit it and to seed the
  // room-name placeholder — this form itself intentionally has no
  // player-name field to avoid the two-inputs-for-one-value confusion.
  const displayName = useMultiplayerStore((s) => s.displayName);
  const setFormatConfig = useMultiplayerStore((s) => s.setFormatConfig);
  const hostingStatus = useMultiplayerStore((s) => s.hostingStatus);

  // Seed the format picker from whatever the user last selected (persisted
  // in the store). This means navigating away and back to host-setup keeps
  // the chosen format, and downstream views (the deck picker reached via
  // "Change Deck") can read the format from the store to filter decks.
  const storeFormatConfig = useMultiplayerStore((s) => s.formatConfig);
  const lastHostConfig = useMultiplayerStore((s) => s.lastHostConfig);
  const rememberHostConfig = useMultiplayerStore((s) => s.rememberHostConfig);

  const isP2P = connectionMode === "p2p";

  // Restore the player's last host-setup choices across sessions — but only
  // when they're still hostable in this connection mode. A remembered format
  // whose minimum exceeds the P2P mesh ceiling can't run over P2P, so we drop
  // back to defaults rather than seed an unhostable configuration.
  const remembered =
    lastHostConfig != null &&
    (!isP2P || FORMAT_DEFAULTS[lastHostConfig.format].min_players <= P2P_MAX_PEERS)
      ? lastHostConfig
      : null;
  const initialFormatConfig =
    remembered?.formatConfig ?? storeFormatConfig ?? FORMAT_DEFAULTS.Commander;
  // Clamp a remembered player count to what this mode/format can actually seat.
  const seatCeiling = isP2P
    ? Math.min(initialFormatConfig.max_players, P2P_MAX_PEERS)
    : initialFormatConfig.max_players;

  const [roomName, setRoomName] = useState("");
  const [isPublic, setIsPublic] = useState(remembered?.isPublic ?? true);
  const [showPassword, setShowPassword] = useState(false);
  const [password, setPassword] = useState("");
  const [selectedFormat, setSelectedFormat] = useState<GameFormat>(
    initialFormatConfig.format,
  );
  const [formatConfig, setLocalFormatConfig] =
    useState<FormatConfig>(initialFormatConfig);
  const [playerCount, setPlayerCount] = useState(
    Math.min(remembered?.playerCount ?? initialFormatConfig.min_players, seatCeiling),
  );
  const [matchType, setMatchType] = useState<MatchType>(remembered?.matchType ?? "Bo1");
  const [aiSeats, setAiSeats] = useState<AiSeatConfig[]>(remembered?.aiSeats ?? []);
  const [startWhenFull, setStartWhenFull] = useState(remembered?.startWhenFull ?? true);
  const [ranked, setRanked] = useState(remembered?.ranked ?? false);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const effectiveMatchType = playerCount === 2 ? matchType : "Bo1";
  const aiDeckCatalog = useAiDeckCatalog({
    selectedFormat: formatConfig.format,
    selectedMatchType: effectiveMatchType,
  });
  const defaultAiDeck = aiDeckCatalog.candidates[0]
    ? { type: "DeckList" as const, data: expandParsedDeck(aiDeckCatalog.candidates[0].deck) }
    : null;

  // Mirror the in-flight format to the store on every change so sibling
  // views (the deck picker shown when the user clicks "Change Deck" out
  // of this form) can filter by the format the user is actively
  // configuring — not just the one they submitted last time. Mirror the
  // format-level invariants only; `max_players` is the format's ceiling
  // here (not the user's chosen count), so overwriting it with the local
  // `playerCount` would collapse the picker on re-entry. The submission
  // payload injects `playerCount` via `finalConfig` below.
  useEffect(() => {
    setFormatConfig(formatConfig);
  }, [formatConfig, setFormatConfig]);

  const maxPlayers = isP2P
    ? Math.min(formatConfig.max_players, P2P_MAX_PEERS)
    : formatConfig.max_players;
  const accentTone = isP2P ? "cyan" : "emerald";

  const handleFormatSelect = (format: GameFormat) => {
    const defaults = FORMAT_DEFAULTS[format];
    setSelectedFormat(format);
    setLocalFormatConfig(defaults);
    // Let multi-seat formats start at their own minimum (e.g. Commander's
    // min is 2, so it still defaults to a duel but users can bump up to 4).
    const newCount = defaults.min_players;
    setPlayerCount(newCount);
    if (newCount !== 2) {
      setMatchType("Bo1");
    }
    setAiSeats([]);
  };

  const handlePlayerCountChange = (count: number) => {
    setPlayerCount(count);
    if (count !== 2) {
      setMatchType("Bo1");
    }
    // Remove AI seats that exceed the new count (seat 0 is always the host)
    setAiSeats((prev) => prev.filter((s) => s.seatIndex < count));
  };

  const handleDeckSizeChange = (deckSize: number) => {
    setLocalFormatConfig((prev) => ({ ...prev, deck_size: deckSize }));
  };

  const toggleAiSeat = (seatIndex: number) => {
    setAiSeats((prev) => {
      const existing = prev.find((s) => s.seatIndex === seatIndex);
      if (existing) {
        return prev.filter((s) => s.seatIndex !== seatIndex);
      }
      return [...prev, { seatIndex, difficulty: "Medium", deckName: null }];
    });
  };

  const setAiDifficulty = (seatIndex: number, difficulty: string) => {
    setAiSeats((prev) =>
      prev.map((s) => (s.seatIndex === seatIndex ? { ...s, difficulty } : s)),
    );
  };

  const handleHost = async () => {
    if (isSubmitting || hostingStatus !== "idle") return;
    setIsSubmitting(true);
    // `finalConfig` is the submission payload — `max_players` here is the
    // user's chosen count, not the format ceiling. Do NOT mirror this
    // into the store: the store tracks the format's invariants (so the
    // deck picker can filter), and overwriting `max_players` there would
    // collapse the picker on re-entry. The live mirror effect above
    // keeps the store in sync with the format itself.
    const finalConfig = { ...formatConfig, max_players: playerCount };
    const trimmedRoomName = roomName.trim();
    // Default to the placeholder value when the field is blank so the
    // lobby title matches what the user was shown. Falls back to null
    // (server uses host name) only if the user has no display name set.
    const resolvedRoomName =
      trimmedRoomName.length > 0
        ? trimmedRoomName
        : displayName
          ? `${displayName}'s table`
          : null;
    // Remember the chosen settings so the next host session restores them
    // instead of resetting to defaults. Persist the format's own config (with
    // its true `max_players` ceiling), not `finalConfig` — the latter's
    // `max_players` is the chosen player count and would cap the slider on
    // restore. Room name and password are intentionally not persisted.
    rememberHostConfig({
      format: selectedFormat,
      formatConfig,
      playerCount,
      matchType: effectiveMatchType,
      isPublic,
      startWhenFull,
      ranked,
      aiSeats,
    });
    try {
      const ok = await onHost({
        displayName,
        public: isPublic,
        password: showPassword ? password : "",
        timerSeconds: null,
        formatConfig: finalConfig,
        matchType: effectiveMatchType,
        aiSeats: aiSeats.map((seat) => ({
          ...seat,
          ...(defaultAiDeck ? { deck: defaultAiDeck } : {}),
        })),
        startWhenFull,
        ranked,
        roomName: resolvedRoomName,
      });
      if (ok !== false) return;
    } catch {
      // The parent surfaces the specific failure as a toast/dialog.
    }
    if (hostingStatus === "idle") {
      setIsSubmitting(false);
    }
  };

  // Filter formats: P2P supports 2-4 peers via WebRTC mesh, so any format
  // whose minimum is reachable from that ceiling is listable. Multi-seat
  // formats that need more than 4 players (e.g. 6-player FreeForAll) are
  // hidden here to avoid advertising a configuration we can't actually host.
  const availableFormats = isP2P
    ? FORMAT_OPTIONS.filter(
        (f) => FORMAT_DEFAULTS[f.format].min_players <= P2P_MAX_PEERS,
      )
    : FORMAT_OPTIONS;

  return (
    <form
      onSubmit={(e) => { e.preventDefault(); void handleHost(); }}
      className="relative z-10 flex w-full max-w-xl flex-col items-center gap-6"
    >
      <MenuPanel className="flex w-full flex-col gap-6">
        <div className="space-y-2">
          <h2 className="menu-display text-[1.9rem] leading-tight text-white">
            {isP2P ? t("hostSetup.hostDirectMatch") : t("hostSetup.hostMatch")}
          </h2>
          {isP2P && (
            <p className="text-sm leading-6 text-slate-400">
              {t("hostSetup.p2pNotice")}
            </p>
          )}
        </div>

        <div className="flex w-full flex-col gap-4">
        {/* Room name — per-match label. Distinct from the player's name
            (edited in the `PlayerIdentityBanner` above this form). Blank
            falls back to the player's name on the server side. */}
        <div>
          <label className="mb-1.5 block text-xs font-medium uppercase tracking-wider text-gray-400">
            {t("hostSetup.roomName")} <span className="text-slate-600">{t("hostSetup.optional")}</span>
          </label>
          <input
            type="text"
            value={roomName}
            onChange={(e) => setRoomName(e.target.value)}
            placeholder={
              displayName
                ? t("hostSetup.roomNameDefaultPlaceholder", { name: displayName })
                : t("hostSetup.roomNamePlaceholder")
            }
            maxLength={40}
            className="w-full rounded-[16px] bg-black/18 px-3 py-2 text-sm text-white placeholder-gray-500 outline-none ring-1 ring-white/10 focus:ring-white/20"
          />
          <p className="mt-1 text-[11px] text-slate-500">
            {t("hostSetup.roomNameHelp")}
            {displayName ? t("hostSetup.roomNameHelpDefault", { name: displayName }) : ""}
          </p>
        </div>

        {/* Format selection -- grouped native <select>. Native is the
            mobile/tablet UX win here: iOS/Android render full-screen
            touch-optimized pickers from <select>, while a custom listbox
            would have to reimplement keyboard avoidance, momentum scroll,
            and 44/48px hit targets. <optgroup>s mirror the engine's
            FormatGroup taxonomy. */}
        <div>
          <label
            htmlFor="host-setup-format"
            className="mb-1.5 block text-xs font-medium uppercase tracking-wider text-gray-400"
          >
            {t("hostSetup.format")}
          </label>
          <select
            id="host-setup-format"
            value={selectedFormat}
            onChange={(e) => handleFormatSelect(e.target.value as GameFormat)}
            className="w-full min-h-[48px] rounded-[16px] border border-white/10 bg-black/18 px-3 py-3 text-base font-medium text-white outline-none ring-1 ring-white/10 focus:border-white/18 focus:ring-white/20"
          >
            {(Object.keys(GROUP_ORDER) as FormatGroup[])
              .sort((a, b) => GROUP_ORDER[a] - GROUP_ORDER[b])
              .map((group) => {
                const items = availableFormats.filter((f) => f.group === group);
                if (items.length === 0) return null;
                return (
                  <optgroup key={group} label={group} className="bg-[#0a0f1b] text-slate-100">
                    {items.map((opt) => (
                      <option
                        key={opt.format}
                        value={opt.format}
                        title={opt.description}
                        className="bg-[#0a0f1b] text-slate-100"
                      >
                        {opt.label}
                      </option>
                    ))}
                  </optgroup>
                );
              })}
          </select>
          {(() => {
            const meta = availableFormats.find((f) => f.format === selectedFormat);
            return meta ? (
              <p className="mt-1.5 text-[11px] leading-4 text-slate-500">{meta.description}</p>
            ) : null;
          })()}
        </div>

        {/* Format-specific settings */}
        <div className="rounded-[18px] border border-white/10 bg-black/18 p-3">
          <div className="flex flex-col gap-3">
            {/* Starting life */}
            <div className="flex items-center justify-between">
              <span className="text-xs text-gray-400">{t("hostSetup.startingLife")}</span>
              <input
                type="number"
                value={formatConfig.starting_life}
                onChange={(e) =>
                  setLocalFormatConfig((prev) => ({
                    ...prev,
                    starting_life: Math.max(1, parseInt(e.target.value) || 1),
                  }))
                }
                min={1}
                className="w-16 rounded-xl bg-black/18 px-2 py-1 text-center text-sm text-white outline-none ring-1 ring-white/10 focus:ring-white/20"
              />
            </div>

            {selectedFormat === "FreeForAll" && (
              <div className="flex items-center justify-between">
                <span className="text-xs text-gray-400">{t("hostSetup.deckSize")}</span>
                <div className="flex rounded-[14px] bg-black/18 p-0.5 ring-1 ring-white/10">
                  {FFA_DECK_SIZE_OPTIONS.map((deckSize) => (
                    <button
                      type="button"
                      key={deckSize}
                      onClick={() => handleDeckSizeChange(deckSize)}
                      className={`rounded px-3 py-1 text-xs font-medium transition-colors ${
                        formatConfig.deck_size === deckSize
                          ? "bg-white/10 text-white"
                          : "text-gray-400 hover:text-gray-200"
                      }`}
                    >
                      {deckSize}
                    </button>
                  ))}
                </div>
              </div>
            )}

            {/* Player count — hidden for fixed-seat formats like Standard
                (min==max==2). Shown in both server and P2P modes when the
                format supports a range; `maxPlayers` already clamps to the
                P2P mesh ceiling so the P2P picker never offers a seat the
                transport can't host. */}
            {formatConfig.min_players !== maxPlayers && (
              <div className="flex items-center justify-between">
                <span className="text-xs text-gray-400">{t("hostSetup.players")}</span>
                <div className="flex rounded-[14px] bg-black/18 p-0.5 ring-1 ring-white/10">
                  {Array.from(
                    { length: maxPlayers - formatConfig.min_players + 1 },
                    (_, i) => formatConfig.min_players + i,
                  ).map((count) => (
                    <button
                      type="button"
                      key={count}
                      onClick={() => handlePlayerCountChange(count)}
                      className={`rounded px-3 py-1 text-xs font-medium transition-colors ${
                        playerCount === count
                          ? "bg-white/10 text-white"
                          : "text-gray-400 hover:text-gray-200"
                      }`}
                    >
                      {count}
                    </button>
                  ))}
                </div>
              </div>
            )}

            <div className="flex items-center justify-between">
              <span className="text-xs text-gray-400">{t("hostSetup.matchType")}</span>
              <div className="flex rounded-[14px] bg-black/18 p-0.5 ring-1 ring-white/10">
                <button
                  type="button"
                  onClick={() => setMatchType("Bo1")}
                  className={`rounded px-3 py-1 text-xs font-medium transition-colors ${
                    matchType === "Bo1"
                      ? "bg-white/10 text-white"
                      : "text-gray-400 hover:text-gray-200"
                  }`}
                >
                  {t("hostSetup.bo1")}
                </button>
                <button
                  type="button"
                  onClick={() => setMatchType("Bo3")}
                  disabled={playerCount !== 2}
                  className={`rounded px-3 py-1 text-xs font-medium transition-colors ${
                    matchType === "Bo3"
                      ? "bg-white/10 text-white"
                      : "text-gray-400 hover:text-gray-200"
                  } ${playerCount !== 2 ? "cursor-not-allowed opacity-40" : ""}`}
                >
                  {t("hostSetup.bo3")}
                </button>
              </div>
            </div>
            {playerCount !== 2 && (
              <p className="text-xs text-gray-500">{t("hostSetup.bo3Note")}</p>
            )}

            {/* Commander damage threshold (Commander only) */}
            {formatConfig.commander_damage_threshold != null && (
              <div className="flex items-center justify-between">
                <span className="text-xs text-gray-400">{t("hostSetup.commanderDamage")}</span>
                <input
                  type="number"
                  value={formatConfig.commander_damage_threshold ?? 21}
                  onChange={(e) =>
                    setLocalFormatConfig((prev) => ({
                      ...prev,
                      commander_damage_threshold: Math.max(1, parseInt(e.target.value) || 21),
                    }))
                  }
                  min={1}
                  className="w-16 rounded-xl bg-black/18 px-2 py-1 text-center text-sm text-white outline-none ring-1 ring-white/10 focus:ring-white/20"
                />
              </div>
            )}
          </div>
        </div>

        {/* AI seat configuration */}
        {playerCount > 1 && (
          <div>
            <label className="mb-1.5 block text-xs font-medium uppercase tracking-wider text-gray-400">
              {t("hostSetup.playerSeats")}
            </label>
            <div className="flex flex-col gap-1.5">
              {/* Seat 0 is always the host */}
              <div className="flex items-center gap-2 rounded-[16px] border border-white/10 bg-black/18 px-3 py-2">
                <span className="text-xs font-medium text-emerald-400">{t("hostSetup.seat", { number: 1 })}</span>
                <span className="flex-1 text-xs text-gray-300">{t("hostSetup.youHost")}</span>
              </div>
              {/* Seats 1..playerCount-1 */}
              {Array.from({ length: playerCount - 1 }, (_, i) => i + 1).map((seatIndex) => {
                const aiSeat = aiSeats.find((s) => s.seatIndex === seatIndex);
                return (
                  <div
                    key={seatIndex}
                    className="flex items-center gap-2 rounded-[16px] border border-white/10 bg-black/18 px-3 py-2"
                  >
                    <span className="text-xs font-medium text-gray-400">{t("hostSetup.seat", { number: seatIndex + 1 })}</span>
                    <button
                      type="button"
                      onClick={() => toggleAiSeat(seatIndex)}
                      className={`rounded px-2 py-0.5 text-xs font-medium transition-colors ${
                        aiSeat
                          ? "bg-amber-500/20 text-amber-300"
                          : "bg-cyan-500/20 text-cyan-300"
                      }`}
                    >
                      {aiSeat ? t("hostSetup.ai") : t("hostSetup.human")}
                    </button>
                    {aiSeat && (
                      <select
                        value={aiSeat.difficulty}
                        onChange={(e) => setAiDifficulty(seatIndex, e.target.value)}
                        className="rounded bg-gray-700 px-1.5 py-0.5 text-xs text-white outline-none"
                      >
                        {DIFFICULTY_OPTIONS.map((d) => (
                          <option
                            key={d}
                            value={d}
                            className="bg-[#0a0f1b] text-slate-100"
                          >
                            {d}
                          </option>
                        ))}
                      </select>
                    )}
                    {!aiSeat && (
                      <span className="flex-1 text-right text-xs text-gray-500">
                        {t("hostSetup.waitingForPlayer")}
                      </span>
                    )}
                  </div>
                );
              })}
            </div>
          </div>
        )}

        <label className="flex items-center gap-2">
          <input
            type="checkbox"
            checked={startWhenFull}
            onChange={(e) => setStartWhenFull(e.target.checked)}
            className={isP2P ? "accent-cyan-500" : "accent-emerald-500"}
          />
          <span className="text-sm text-gray-300">{t("hostSetup.startWhenFull")}</span>
        </label>

        <label className="flex items-center gap-2">
          <input
            type="checkbox"
            checked={ranked}
            onChange={(e) => setRanked(e.target.checked)}
            className={isP2P ? "accent-cyan-500" : "accent-emerald-500"}
          />
          <span className="text-sm text-gray-300">{t("hostSetup.ranked")}</span>
        </label>

        {/* List in lobby (server mode only) */}
        {!isP2P && (
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={isPublic}
              onChange={(e) => setIsPublic(e.target.checked)}
              className="accent-emerald-500"
            />
            <span className="text-sm text-gray-300">{t("hostSetup.listInLobby")}</span>
          </label>
        )}

        {/* Sandbox mode — capability flag, orthogonal to format. When enabled
            at game creation, the host can submit debug actions and may grant
            permission to other players. Off by default. Immutable for the
            life of the session. */}
        <div>
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={formatConfig.allow_debug_actions}
              onChange={(e) =>
                setLocalFormatConfig((prev) => ({
                  ...prev,
                  allow_debug_actions: e.target.checked,
                }))
              }
              className="accent-amber-500"
            />
            <span className="text-sm text-gray-300">
              {t("hostSetup.sandboxMode")}
            </span>
          </label>
          <p className="mt-1 pl-6 text-[11px] leading-4 text-slate-500">
            {t("hostSetup.sandboxModeHelp")}
          </p>
        </div>

        {/* Password toggle and input */}
        <div>
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={showPassword}
              onChange={(e) => {
                setShowPassword(e.target.checked);
                if (!e.target.checked) setPassword("");
              }}
              className={isP2P ? "accent-cyan-500" : "accent-emerald-500"}
            />
            <span className="text-sm text-gray-300">{t("hostSetup.setPassword")}</span>
          </label>
          {showPassword && (
            <input
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              placeholder={t("hostSetup.passwordPlaceholder")}
              maxLength={32}
              className="mt-2 w-full rounded-[16px] bg-black/18 px-3 py-2 text-sm text-white placeholder-gray-500 outline-none ring-1 ring-white/10 focus:ring-white/20"
            />
          )}
        </div>

      </div>

        <div className="flex justify-end gap-3">
          <button
            type="button"
            onClick={onBack}
            className={menuButtonClass({ tone: "neutral", size: "sm" })}
          >
            {t("hostSetup.back")}
          </button>
          <button
            type="submit"
            disabled={hostDisabled || isSubmitting || hostingStatus !== "idle" || (aiSeats.length > 0 && !defaultAiDeck)}
            title={hostDisabled ? hostDisabledReason : undefined}
            aria-disabled={hostDisabled || isSubmitting || hostingStatus !== "idle" || (aiSeats.length > 0 && !defaultAiDeck) || undefined}
            className={`${menuButtonClass({ tone: accentTone, size: "md" })} disabled:cursor-not-allowed disabled:opacity-50`}
          >
            {isSubmitting || hostingStatus !== "idle"
              ? t("hostSetup.opening")
              : isP2P
                ? t("hostSetup.hostP2PGame")
                : t("hostSetup.hostGame")}
          </button>
        </div>
      </MenuPanel>
    </form>
  );
}
