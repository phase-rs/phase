import { useEffect, useMemo, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";

import type { FormatConfig, FormatGroup, GameFormat, LoopDetectionMode, MatchType } from "../../adapter/types";
import { AI_DIFFICULTIES } from "../../constants/ai";
import { FORMAT_REGISTRY } from "../../data/formatRegistry";
import { FORMAT_DEFAULTS, useMultiplayerStore } from "../../stores/multiplayerStore";
import type { AiSeatConfig, HostingSettings } from "../../stores/multiplayerStore";
import { useAiDeckCatalog } from "../../services/aiDeckCatalog";
import { expandParsedDeck } from "../../services/deckParser";
import { menuButtonClass } from "../menu/buttonStyles";
import { MenuSelect, type MenuSelectGroup } from "../ui/MenuSelect";

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
// picker automatically. Surface-specific guards live at submit/render
// boundaries below.
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

const FFA_DECK_SIZE_OPTIONS = [60, 40] as const;

/** P2P uses a hub-and-spoke topology (see `p2p-adapter.ts` `P2PHostAdapter`):
 * the host holds one connection per guest and fans out filtered state, which
 * scales linearly. The ceiling here matches the engine's Free-for-All maximum
 * (`format.rs` `free_for_all`, max_players 6) rather than a transport limit. */
const P2P_MAX_PEERS = 6;

/** Uppercase field label + optional hint wrapper (mirrors the design mockup's
 *  Host-setup `Field`). Pure presentation. */
function Field({
  label,
  hint,
  htmlFor,
  children,
}: {
  label: string;
  hint?: string;
  htmlFor?: string;
  children: ReactNode;
}) {
  // A wrapping <label> would absorb the control's own text into its accessible
  // name (breaking getByLabelText and screen-reader labels). Render the label as
  // a sibling associated by htmlFor instead; fall back to a plain span for
  // control groups (segmented buttons) that have no single labelable target.
  return (
    <div className="flex flex-col gap-1.5">
      {htmlFor ? (
        <label htmlFor={htmlFor} className="text-[0.62rem] font-semibold uppercase tracking-[0.18em] text-fg-meta">
          {label}
        </label>
      ) : (
        <span className="text-[0.62rem] font-semibold uppercase tracking-[0.18em] text-fg-meta">
          {label}
        </span>
      )}
      {children}
      {hint && <span className="text-[11.5px] leading-4 text-fg-meta">{hint}</span>}
    </div>
  );
}

/** iOS-style toggle switch (mirrors the mockup's Host-setup `Toggle`). The
 *  on-state accent follows the connection mode (emerald server / cyan P2P). */
function Toggle({
  on,
  onChange,
  accent,
}: {
  on: boolean;
  onChange: (next: boolean) => void;
  accent: "emerald" | "cyan";
}) {
  const onBg = accent === "cyan" ? "bg-cyan-400/50" : "bg-emerald-400/50";
  const knob = on ? (accent === "cyan" ? "bg-cyan-200" : "bg-emerald-200") : "bg-slate-400";
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      onClick={() => onChange(!on)}
      className={`flex h-6 w-[42px] shrink-0 items-center rounded-full p-0.5 transition-colors ${on ? onBg : "bg-white/12"}`}
    >
      <span className={`h-5 w-5 rounded-full transition-transform duration-150 ${knob} ${on ? "translate-x-[18px]" : ""}`} />
    </button>
  );
}

/** A label + description row with a trailing {@link Toggle} (the mockup's
 *  privacy/timing option rows). */
function OptionRow({
  label,
  desc,
  on,
  onChange,
  accent,
}: {
  label: string;
  desc?: string;
  on: boolean;
  onChange: (next: boolean) => void;
  accent: "emerald" | "cyan";
}) {
  return (
    <div className="flex items-center justify-between gap-4">
      <div className="min-w-0">
        <div className="text-sm text-fg-card-body">{label}</div>
        {desc && <div className="mt-0.5 text-xs text-fg-meta">{desc}</div>}
      </div>
      <Toggle on={on} onChange={onChange} accent={accent} />
    </div>
  );
}

/** Host, waiting-player, and AI seat glyphs for the Player Seats panel. */
function CrownGlyph({ className = "h-4 w-4" }: { className?: string }) {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className={`${className} fill-current`}>
      <path d="M3 7l4 4 5-6 5 6 4-4-1.5 11h-15L3 7Zm2.4 13h13.2v1.5H5.4V20Z" />
    </svg>
  );
}
function HumanGlyph({ className = "h-4 w-4" }: { className?: string }) {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className={`${className} fill-current`}>
      <path d="M12 12a4.5 4.5 0 1 0 0-9 4.5 4.5 0 0 0 0 9Zm0 2c-4.2 0-7.5 2.2-7.5 5v1h15v-1c0-2.8-3.3-5-7.5-5Z" />
    </svg>
  );
}
function BotGlyph({ className = "h-4 w-4" }: { className?: string }) {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className={`${className} fill-current`}>
      <path d="M12 2a1.5 1.5 0 0 1 1.5 1.5V5H17a3 3 0 0 1 3 3v8a3 3 0 0 1-3 3H7a3 3 0 0 1-3-3V8a3 3 0 0 1 3-3h3.5V3.5A1.5 1.5 0 0 1 12 2ZM9 10.5a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3Zm6 0a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3ZM2 11h1.5v4H2a1 1 0 0 1-1-1v-2a1 1 0 0 1 1-1Zm18.5 0H22a1 1 0 0 1 1 1v2a1 1 0 0 1-1 1h-1.5v-4Z" />
    </svg>
  );
}

export function HostSetup({
  onHost,
  onBack,
  connectionMode,
  hostDisabled = false,
  hostDisabledReason,
}: HostSetupProps) {
  const { t } = useTranslation(["multiplayer", "menu"]);
  // Player name is edited in `PlayerIdentityBanner` above this form (see
  // MultiplayerPage). We read it here only to submit it and to seed the
  // room-name placeholder — this form itself intentionally has no
  // player-name field to avoid the two-inputs-for-one-value confusion.
  const displayName = useMultiplayerStore((s) => s.displayName);
  const setFormatConfig = useMultiplayerStore((s) => s.setFormatConfig);
  const setCompatibilityPlayerCount = useMultiplayerStore(
    (s) => s.setCompatibilityPlayerCount,
  );
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
  // CR 732.2a: combo (infinite-loop) detector opt-in, chosen at match creation and
  // immutable during play. Available at every player count (Commander infinites).
  const [loopDetection, setLoopDetection] = useState<LoopDetectionMode>(
    remembered?.loopDetection ?? { type: "Off" },
  );
  const [aiSeats, setAiSeats] = useState<AiSeatConfig[]>(remembered?.aiSeats ?? []);
  const [startWhenFull, setStartWhenFull] = useState(remembered?.startWhenFull ?? true);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const effectiveMatchType = playerCount === 2 ? matchType : "Bo1";
  const aiDeckCatalog = useAiDeckCatalog({
    selectedFormat: formatConfig.format,
    selectedMatchType: effectiveMatchType,
  });
  const defaultAiDeck = aiDeckCatalog.candidates[0]
    ? { type: "DeckList" as const, data: expandParsedDeck(aiDeckCatalog.candidates[0].deck) }
    : null;
  const aiSeatsSupported = !formatConfig.team_based && formatConfig.format !== "Planechase";
  const effectiveAiSeats = aiSeatsSupported ? aiSeats : [];

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
    setCompatibilityPlayerCount(playerCount);
  }, [formatConfig, playerCount, setCompatibilityPlayerCount, setFormatConfig]);

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
    setCompatibilityPlayerCount(newCount);
    if (newCount !== 2) {
      setMatchType("Bo1");
    }
    setAiSeats([]);
  };

  const handlePlayerCountChange = (count: number) => {
    setPlayerCount(count);
    setCompatibilityPlayerCount(count);
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
      loopDetection,
      isPublic,
      startWhenFull,
      // Ranked rating updates aren't implemented in the engine — the room is
      // always casual. The transport field is retained for protocol parity.
      ranked: false,
      aiSeats: effectiveAiSeats,
    });
    try {
      const ok = await onHost({
        displayName,
        public: isPublic,
        password: showPassword ? password : "",
        timerSeconds: null,
        formatConfig: finalConfig,
        matchType: effectiveMatchType,
        loopDetection,
        aiSeats: effectiveAiSeats.map((seat) => ({
          ...seat,
          ...(defaultAiDeck ? { deck: defaultAiDeck } : {}),
        })),
        startWhenFull,
        ranked: false,
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

  // Filter formats: P2P supports 2-6 players (hub-and-spoke, see P2P_MAX_PEERS),
  // so any format whose minimum is reachable from that ceiling is listable.
  // Formats requiring more seats than the ceiling are hidden here to avoid
  // advertising a configuration we can't actually host.
  const availableFormats = useMemo(
    () =>
      isP2P
        ? FORMAT_OPTIONS.filter(
            (f) => FORMAT_DEFAULTS[f.format].min_players <= P2P_MAX_PEERS,
          )
        : FORMAT_OPTIONS,
    [isP2P],
  );

  // Shared field-input grammar (mockup Host-setup inputs).
  const inp =
    "w-full rounded-[12px] border border-hairline bg-black/28 px-3.5 py-2.5 text-sm text-white placeholder-gray-500 outline-none transition-colors focus:border-hairline-hover";
  const segWrap = "flex rounded-[12px] bg-black/28 p-1 ring-1 ring-white/10";
  const seg = (on: boolean, extra = "") =>
    `flex-1 rounded-[9px] px-3 py-1.5 text-xs font-medium transition-colors ${
      on ? "bg-white/10 text-white" : "text-fg-meta hover:text-slate-200"
    } ${extra}`;
  const formatMeta = availableFormats.find((f) => f.format === selectedFormat);
  const formatMenuGroups = useMemo((): MenuSelectGroup[] => {
    const groups: MenuSelectGroup[] = [];
    for (const group of (Object.keys(GROUP_ORDER) as FormatGroup[]).sort(
      (a, b) => GROUP_ORDER[a] - GROUP_ORDER[b],
    )) {
      const groupFormats = availableFormats.filter((f) => f.group === group);
      if (groupFormats.length === 0) continue;
      groups.push({
        label: group,
        items: groupFormats.map((opt) => ({
          value: opt.format,
          label: opt.label,
        })),
      });
    }
    return groups;
  }, [availableFormats]);
  const difficultyMenuItems = useMemo(
    () =>
      AI_DIFFICULTIES.map(({ id }) => ({
        value: id,
        label: t(`menu:aiDifficulty.levels.${id}`),
      })),
    [t],
  );
  const submitDisabled =
    hostDisabled || isSubmitting || hostingStatus !== "idle" || (effectiveAiSeats.length > 0 && !defaultAiDeck);

  return (
    <form
      onSubmit={(e) => { e.preventDefault(); void handleHost(); }}
      className="relative z-10 flex w-full flex-col gap-5"
    >
      {isP2P && (
        <p className="max-w-2xl text-sm leading-6 text-slate-400">
          {t("hostSetup.p2pNotice")}
        </p>
      )}

      {/* Two-column table-setup grammar (design mockup HostScreen): form panel
          beside a sticky seat panel + primary CTA. Stacks to one column below lg. */}
      <div className="grid w-full grid-cols-1 gap-5 lg:grid-cols-[minmax(0,1fr)_260px] lg:items-start">
        {/* ----- left: configuration form ----- */}
        <div className="surface-card flex flex-col gap-4 rounded-panel border border-hairline p-5">
          {/* Room name — per-match label, distinct from the player's name
              (edited in the `PlayerIdentityBanner` above this form). Blank falls
              back to the player's name on the server side. */}
          <Field
            label={`${t("hostSetup.roomName")} (${t("hostSetup.optional")})`}
            htmlFor="host-setup-room"
            hint={`${t("hostSetup.roomNameHelp")}${displayName ? t("hostSetup.roomNameHelpDefault", { name: displayName }) : ""}`}
          >
            <input
              id="host-setup-room"
              type="text"
              value={roomName}
              onChange={(e) => setRoomName(e.target.value)}
              placeholder={
                displayName
                  ? t("hostSetup.roomNameDefaultPlaceholder", { name: displayName })
                  : t("hostSetup.roomNamePlaceholder")
              }
              maxLength={40}
              className={inp}
            />
          </Field>

          <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
            {/* Format — grouped MenuSelect mirrors the engine's FormatGroup
                taxonomy. fitContainer keeps the trigger inside the grid column;
                menuLayout="dropdown" anchors below the trigger on all widths. */}
            <Field label={t("hostSetup.format")} hint={formatMeta?.description}>
              <MenuSelect
                ariaLabel={t("hostSetup.format")}
                label={formatMeta?.label ?? selectedFormat}
                selectedValue={selectedFormat}
                groups={formatMenuGroups}
                onSelect={(value) => handleFormatSelect(value as GameFormat)}
                menuLayout="dropdown"
                fitContainer
                wrapperClassName="w-full min-w-0"
                className={`${inp} min-h-[44px] w-full cursor-pointer font-medium`}
              />
            </Field>

            <Field label={t("hostSetup.startingLife")} htmlFor="host-setup-life">
              <input
                id="host-setup-life"
                type="number"
                value={formatConfig.starting_life}
                onChange={(e) =>
                  setLocalFormatConfig((prev) => ({
                    ...prev,
                    starting_life: Math.max(1, parseInt(e.target.value) || 1),
                  }))
                }
                min={1}
                className={inp}
              />
            </Field>
          </div>

          <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
            {/* Player count — hidden for fixed-seat formats like Standard
                (min==max==2). `maxPlayers` already clamps to the P2P mesh
                ceiling so the picker never offers an unhostable seat. */}
            {formatConfig.min_players !== maxPlayers && (
              <Field label={t("hostSetup.players")}>
                <div className={segWrap}>
                  {Array.from(
                    { length: maxPlayers - formatConfig.min_players + 1 },
                    (_, i) => formatConfig.min_players + i,
                  ).map((count) => (
                    <button type="button" key={count} onClick={() => handlePlayerCountChange(count)} className={seg(playerCount === count)}>
                      {count}
                    </button>
                  ))}
                </div>
              </Field>
            )}

            <Field label={t("hostSetup.matchType")}>
              <div className={segWrap}>
                <button type="button" onClick={() => setMatchType("Bo1")} className={seg(matchType === "Bo1")}>
                  {t("hostSetup.bo1")}
                </button>
                <button
                  type="button"
                  onClick={() => setMatchType("Bo3")}
                  disabled={playerCount !== 2}
                  className={seg(matchType === "Bo3", playerCount !== 2 ? "cursor-not-allowed opacity-40" : "")}
                >
                  {t("hostSetup.bo3")}
                </button>
              </div>
            </Field>

            {/* CR 732.2a: combo (infinite-loop) detector opt-in, immutable once the
                match starts. Offered at every player count (Commander infinites). */}
            <Field label={t("common:comboDetector.label")}>
              <div className={segWrap} title={t("common:comboDetector.title")}>
                <button
                  type="button"
                  onClick={() => setLoopDetection({ type: "Off" })}
                  className={seg(loopDetection.type === "Off")}
                >
                  {t("common:comboDetector.off")}
                </button>
                <button
                  type="button"
                  onClick={() => setLoopDetection({ type: "On" })}
                  className={seg(loopDetection.type === "On")}
                >
                  {t("common:comboDetector.on")}
                </button>
                <button
                  type="button"
                  onClick={() => setLoopDetection({ type: "Interactive" })}
                  className={seg(loopDetection.type === "Interactive")}
                >
                  {t("common:comboDetector.interactive")}
                </button>
              </div>
            </Field>
          </div>
          {playerCount !== 2 && <p className="-mt-1 text-xs text-fg-meta">{t("hostSetup.bo3Note")}</p>}

          {/* Free-for-all deck size (FFA only) */}
          {selectedFormat === "FreeForAll" && (
            <Field label={t("hostSetup.deckSize")}>
              <div className={segWrap}>
                {FFA_DECK_SIZE_OPTIONS.map((deckSize) => (
                  <button type="button" key={deckSize} onClick={() => handleDeckSizeChange(deckSize)} className={seg(formatConfig.deck_size === deckSize)}>
                    {deckSize}
                  </button>
                ))}
              </div>
            </Field>
          )}

          {/* Commander damage threshold (Commander only) */}
          {formatConfig.commander_damage_threshold != null && (
            <Field label={t("hostSetup.commanderDamage")} htmlFor="host-setup-cmd-dmg">
              <input
                id="host-setup-cmd-dmg"
                type="number"
                value={formatConfig.commander_damage_threshold ?? 21}
                onChange={(e) =>
                  setLocalFormatConfig((prev) => ({
                    ...prev,
                    commander_damage_threshold: Math.max(1, parseInt(e.target.value) || 21),
                  }))
                }
                min={1}
                className={inp}
              />
            </Field>
          )}

          <div className="border-t border-hairline-strong" />

          {/* Privacy / timing options — iOS-toggle rows (design mockup). */}
          {!isP2P && (
            <OptionRow
              label={t("hostSetup.listInLobby")}
              on={isPublic}
              onChange={setIsPublic}
              accent={accentTone}
            />
          )}
          <OptionRow label={t("hostSetup.startWhenFull")} on={startWhenFull} onChange={setStartWhenFull} accent={accentTone} />
          {/* Sandbox mode — capability flag, orthogonal to format; lets the host
              submit debug actions. Off by default; immutable for the session. */}
          <OptionRow
            label={t("hostSetup.sandboxMode")}
            desc={t("hostSetup.sandboxModeHelp")}
            on={formatConfig.allow_debug_actions}
            onChange={(v) => setLocalFormatConfig((prev) => ({ ...prev, allow_debug_actions: v }))}
            accent={accentTone}
          />
          <div className="flex flex-col gap-2.5">
            <OptionRow
              label={t("hostSetup.setPassword")}
              on={showPassword}
              onChange={(v) => {
                setShowPassword(v);
                if (!v) setPassword("");
              }}
              accent={accentTone}
            />
            {showPassword && (
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                placeholder={t("hostSetup.passwordPlaceholder")}
                maxLength={32}
                className={inp}
              />
            )}
          </div>
        </div>

        {/* ----- right: seat panel + primary CTA (sticky on lg) ----- */}
        <div className="flex flex-col gap-4 lg:sticky lg:top-4">
          {playerCount > 1 && (
            <div className="surface-card rounded-panel border border-hairline p-4">
              <div className="mb-3 text-[0.62rem] font-semibold uppercase tracking-[0.18em] text-fg-meta">
                {t("hostSetup.playerSeats")}
              </div>
              <div className="flex flex-col gap-2">
                {/* Seat 0 is always the host */}
                <div className="flex items-center gap-2.5 rounded-[12px] border border-hairline bg-black/20 px-3 py-2">
                  <span className="w-3.5 shrink-0 text-center font-mono text-[11px] text-fg-meta">1</span>
                  <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-[9px] border border-ember/50 bg-ember/15 text-ember-soft">
                    <CrownGlyph />
                  </span>
                  <span className="text-[13px] font-medium text-amber-200">{t("hostSetup.youHost")}</span>
                </div>
                {/* Seats 1..playerCount-1 */}
                {Array.from({ length: playerCount - 1 }, (_, i) => i + 1).map((seatIndex) => {
                  const aiSeat = effectiveAiSeats.find((s) => s.seatIndex === seatIndex);
                  return (
                    <div key={seatIndex} className="flex items-center gap-2.5 rounded-[12px] border border-hairline bg-black/20 px-3 py-2">
                      <span className="w-3.5 shrink-0 text-center font-mono text-[11px] text-fg-meta">{seatIndex + 1}</span>
                      <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-[9px] border border-hairline bg-white/5 text-fg-meta">
                        {aiSeat ? <BotGlyph /> : <HumanGlyph />}
                      </span>
                      {aiSeatsSupported && (
                        <button
                          type="button"
                          onClick={() => toggleAiSeat(seatIndex)}
                          className={`rounded-badge px-2 py-0.5 text-[11px] font-semibold transition-colors ${
                            aiSeat ? "bg-amber-500/20 text-amber-300" : "bg-cyan-500/20 text-cyan-300"
                          }`}
                        >
                          {aiSeat ? t("hostSetup.ai") : t("hostSetup.human")}
                        </button>
                      )}
                      {aiSeat ? (
                        <MenuSelect
                          ariaLabel={t("menu:aiDifficulty.label")}
                          label={
                            difficultyMenuItems.find((item) => item.value === aiSeat.difficulty)?.label ??
                            t(`menu:aiDifficulty.levels.${aiSeat.difficulty}`)
                          }
                          selectedValue={aiSeat.difficulty}
                          items={difficultyMenuItems}
                          onSelect={(value) => setAiDifficulty(seatIndex, value)}
                          menuLayout="dropdown"
                          wrapperClassName="ml-auto min-w-0"
                          className="rounded-[8px] border border-hairline bg-black/30 px-1.5 py-1 text-[11px] font-medium text-white"
                        />
                      ) : (
                        <span className="ml-auto text-[11px] text-fg-meta">{t("hostSetup.waitingForPlayer")}</span>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          <button
            type="submit"
            disabled={submitDisabled}
            title={hostDisabled ? hostDisabledReason : undefined}
            aria-disabled={submitDisabled || undefined}
            className={`${menuButtonClass({ tone: accentTone, size: "md" })} w-full disabled:cursor-not-allowed disabled:opacity-50`}
          >
            {isSubmitting || hostingStatus !== "idle"
              ? t("hostSetup.opening")
              : isP2P
                ? t("hostSetup.hostP2PGame")
                : t("hostSetup.hostGame")}
          </button>
          <button
            type="button"
            onClick={onBack}
            className={`${menuButtonClass({ tone: "neutral", size: "sm" })} w-full`}
          >
            {t("hostSetup.back")}
          </button>
        </div>
      </div>
    </form>
  );
}
