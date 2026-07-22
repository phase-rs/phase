import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Trans, useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

import { audioManager } from "../../audio/AudioManager.ts";
import { cacheThemeManifest, clearThemeCache } from "../../audio/audioCache.ts";
import { BUILT_IN_THEMES, findManifest, validateThemeManifest } from "../../audio/themeRegistry.ts";
import { PLANESWALKER_THEME } from "../../audio/planeswalkerTheme.ts";
import {
  CARD_PREVIEW_HOVER_DELAY_MAX,
  CARD_PREVIEW_HOVER_DELAY_MIN,
  CARD_PREVIEW_HOVER_DELAY_STEP,
  usePreferencesStore,
} from "../../stores/preferencesStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import {
  ANIMATION_SPEED_DEFAULT,
  ANIMATION_SPEED_MAX,
  ANIMATION_SPEED_MIN,
  ANIMATION_SPEED_STEP,
  PACING_CATEGORIES,
  PACING_DEFAULT,
  PACING_MAX,
  PACING_MIN,
  PACING_STEP,
  type PacingCategory,
  type VfxQuality,
} from "../../animation/types.ts";
import type {
  ArtChainEntry,
  CardPreviewMode,
  CardSizePreference,
  CommandZoneDisplay,
  LogDefaultState,
  MultiplayerBoardLayout,
  ZoneCollapseMode,
} from "../../stores/preferencesStore.ts";
import type { SupportedLng } from "../../i18n/resources.ts";
import { LanguageFlag } from "../ui/LanguageFlag.tsx";
import { BATTLEFIELDS } from "../board/battlefields.ts";
import { PLAIN_BACKGROUNDS } from "../board/plainBackgrounds.ts";
import { ConfirmDialog } from "../ui/ConfirmDialog.tsx";
import { ModalPanelShell } from "../ui/ModalPanelShell";
import { MenuSelect } from "../ui/MenuSelect";
import { downloadBackup, importBackupFromFile, type ImportMode } from "../../services/backup.ts";
import { isTauri } from "../../services/platform.ts";
import { useCloudSyncStore } from "../../stores/cloudSyncStore.ts";
import { DiscordIcon, GoogleIcon } from "../ui/ProviderIcons";

export type SettingsHighlight = "board-background";

interface PreferencesModalProps {
  onClose: () => void;
  initialTab?: SettingsTabId;
  highlight?: SettingsHighlight;
}

/** Locale options for the language selector. Labels are autonyms (each language's
 *  own name) and are intentionally NOT translated. */
const LANGUAGE_OPTIONS: { value: SupportedLng; label: string }[] = [
  { value: "en", label: "English" },
  { value: "es", label: "Español" },
  { value: "fr", label: "Français" },
  { value: "de", label: "Deutsch" },
  { value: "it", label: "Italiano" },
  { value: "pt", label: "Português" },
  { value: "pl", label: "Polski" },
];

const CARD_SIZES: CardSizePreference[] = ["small", "medium", "large"];
const COMMAND_ZONE_DISPLAYS: CommandZoneDisplay[] = ["auto", "inline", "compact"];
const ZONE_COLLAPSE_MODES: ZoneCollapseMode[] = ["auto", "on", "off"];
const CARD_PREVIEW_MODES: CardPreviewMode[] = ["follow", "side", "shift"];
const LOG_DEFAULTS: LogDefaultState[] = ["open", "closed"];
const VFX_QUALITIES: VfxQuality[] = ["full", "reduced", "minimal"];
const MULTIPLAYER_BOARD_LAYOUTS: MultiplayerBoardLayout[] = ["focused", "split"];

/** Format a speed value as a user-facing label. The slider goes 0→max where
 *  max = instant (skip animations). `0` = slowest, `1` = normal. The endpoint
 *  labels are translated by the caller and passed in. */
function formatSpeed(value: number, max: number, labels: { instant: string; slowest: string }): string {
  if (value >= max) return labels.instant;
  if (value <= 0) return labels.slowest;
  return `${value.toFixed(2)}×`;
}
const SETTINGS_TABS = [
  { id: "gameplay" },
  { id: "visual" },
  { id: "combat" },
  { id: "audio" },
  { id: "multiplayer" },
  { id: "data" },
] as const;

export type SettingsTabId = (typeof SETTINGS_TABS)[number]["id"];

/** Board-background select groups. `labelKey` is a settings-namespace i18n key
 *  for the frontend-authored group/option labels; battlefield and plain
 *  background labels come from engine/asset modules and stay raw. */
type BoardBackgroundGroup = {
  labelKey: string;
  options: { value: string; labelKey?: string; label?: string }[];
};
const BOARD_BACKGROUND_GROUPS: BoardBackgroundGroup[] = [
  {
    labelKey: "gameplay.boardBackgroundGroups.automatic",
    options: [
      { value: "auto-wubrg", labelKey: "gameplay.boardBackgroundOptions.autoMatchDeck" },
      { value: "random", labelKey: "gameplay.boardBackgroundOptions.random" },
    ],
  },
  {
    labelKey: "gameplay.boardBackgroundGroups.battlefields",
    options: BATTLEFIELDS.map((bf) => ({ value: bf.id, label: `${bf.label} (${bf.color})` })),
  },
  {
    labelKey: "gameplay.boardBackgroundGroups.plain",
    options: PLAIN_BACKGROUNDS.map((bg) => ({ value: bg.id, label: bg.label })),
  },
  {
    labelKey: "gameplay.boardBackgroundGroups.custom",
    options: [{ value: "custom", labelKey: "gameplay.boardBackgroundOptions.customUrl" }],
  },
  {
    labelKey: "gameplay.boardBackgroundGroups.off",
    options: [{ value: "none", labelKey: "gameplay.boardBackgroundOptions.none" }],
  },
];

const SETTINGS_MENU_CLASS =
  "min-h-[44px] rounded-[14px] py-2 text-base sm:min-h-0 sm:text-sm";

export function PreferencesModal({
  onClose,
  initialTab = "gameplay",
  highlight,
}: PreferencesModalProps) {
  const { t } = useTranslation("settings");
  const setFlexEditMode = useUiStore((s) => s.setFlexEditMode);
  const boardBackgroundRef = useRef<HTMLDivElement | null>(null);
  const [highlightFlash, setHighlightFlash] = useState(highlight === "board-background");

  useEffect(() => {
    if (highlight !== "board-background") return;
    // Scroll the highlighted section into view and flash a ring outline briefly.
    const frame = requestAnimationFrame(() => {
      boardBackgroundRef.current?.scrollIntoView({ behavior: "smooth", block: "center" });
    });
    const timer = window.setTimeout(() => setHighlightFlash(false), 1800);
    return () => {
      cancelAnimationFrame(frame);
      window.clearTimeout(timer);
    };
  }, [highlight]);

  const language = usePreferencesStore((s) => s.language);
  const setLanguage = usePreferencesStore((s) => s.setLanguage);
  const cardSize = usePreferencesStore((s) => s.cardSize);
  const commandZoneDisplay = usePreferencesStore((s) => s.commandZoneDisplay);
  const collapseLands = usePreferencesStore((s) => s.collapseLands);
  const collapseSupport = usePreferencesStore((s) => s.collapseSupport);
  const logDefaultState = usePreferencesStore((s) => s.logDefaultState);
  const multiplayerBoardLayout = usePreferencesStore((s) => s.multiplayerBoardLayout);
  const spellPaymentMode = usePreferencesStore((s) => s.spellPaymentMode);
  const priorityPassingMode = usePreferencesStore((s) => s.priorityPassingMode);
  const boardBackground = usePreferencesStore((s) => s.boardBackground);
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);
  const animationSpeedMultiplier = usePreferencesStore((s) => s.animationSpeedMultiplier);
  const pacingMultipliers = usePreferencesStore((s) => s.pacingMultipliers);
  const setCardSize = usePreferencesStore((s) => s.setCardSize);
  const setCommandZoneDisplay = usePreferencesStore((s) => s.setCommandZoneDisplay);
  const setCollapseLands = usePreferencesStore((s) => s.setCollapseLands);
  const setCollapseSupport = usePreferencesStore((s) => s.setCollapseSupport);
  const setLogDefaultState = usePreferencesStore((s) => s.setLogDefaultState);
  const setMultiplayerBoardLayout = usePreferencesStore((s) => s.setMultiplayerBoardLayout);
  const setSpellPaymentMode = usePreferencesStore((s) => s.setSpellPaymentMode);
  const setPriorityPassingMode = usePreferencesStore((s) => s.setPriorityPassingMode);
  const setBoardBackground = usePreferencesStore((s) => s.setBoardBackground);
  const customBackgroundUrl = usePreferencesStore((s) => s.customBackgroundUrl);
  const setCustomBackgroundUrl = usePreferencesStore((s) => s.setCustomBackgroundUrl);
  const setVfxQuality = usePreferencesStore((s) => s.setVfxQuality);
  const setPacingMultiplier = usePreferencesStore((s) => s.setPacingMultiplier);
  const resetPacing = usePreferencesStore((s) => s.resetPacing);
  const resetAllPreferences = usePreferencesStore((s) => s.resetAllPreferences);
  const masterVolume = usePreferencesStore((s) => s.masterVolume);
  const sfxVolume = usePreferencesStore((s) => s.sfxVolume);
  const musicVolume = usePreferencesStore((s) => s.musicVolume);
  const masterMuted = usePreferencesStore((s) => s.masterMuted);
  const setMasterMuted = usePreferencesStore((s) => s.setMasterMuted);
  const setMasterVolume = usePreferencesStore((s) => s.setMasterVolume);
  const setSfxVolume = usePreferencesStore((s) => s.setSfxVolume);
  const setMusicVolume = usePreferencesStore((s) => s.setMusicVolume);
  const setAnimationSpeedMultiplier = usePreferencesStore((s) => s.setAnimationSpeedMultiplier);
  const nativeEngineEnabled = usePreferencesStore((s) => s.nativeEngineEnabled);
  const setNativeEngineEnabled = usePreferencesStore((s) => s.setNativeEngineEnabled);
  const showKeywordStrip = usePreferencesStore((s) => s.showKeywordStrip) ?? true;
  const setShowKeywordStrip = usePreferencesStore((s) => s.setShowKeywordStrip);
  const battlefieldPeekOnHover = usePreferencesStore((s) => s.battlefieldPeekOnHover) ?? true;
  const setBattlefieldPeekOnHover = usePreferencesStore((s) => s.setBattlefieldPeekOnHover);
  const cardPreviewMode = usePreferencesStore((s) => s.cardPreviewMode) ?? "follow";
  const setCardPreviewMode = usePreferencesStore((s) => s.setCardPreviewMode);
  const cardPreviewHoverDelayMs = usePreferencesStore((s) => s.cardPreviewHoverDelayMs) ?? 0;
  const setCardPreviewHoverDelayMs = usePreferencesStore((s) => s.setCardPreviewHoverDelayMs);
  const showCardPreviewFooter = usePreferencesStore((s) => s.showCardPreviewFooter) ?? true;
  const setShowCardPreviewFooter = usePreferencesStore((s) => s.setShowCardPreviewFooter);
  const artChain = usePreferencesStore((s) => s.artChain);
  const addArtChainEntry = usePreferencesStore((s) => s.addArtChainEntry);
  const removeArtChainEntry = usePreferencesStore((s) => s.removeArtChainEntry);
  const moveArtChainEntry = usePreferencesStore((s) => s.moveArtChainEntry);
  const artOverrides = usePreferencesStore((s) => s.artOverrides);
  const clearAllArtOverrides = usePreferencesStore((s) => s.clearAllArtOverrides);
  const artOverrideCount = Object.keys(artOverrides).length;

  // Audio theme settings
  const audioThemeId = usePreferencesStore((s) => s.audioThemeId);
  const customThemeUrls = usePreferencesStore((s) => s.customThemeUrls);
  const setAudioThemeId = usePreferencesStore((s) => s.setAudioThemeId);
  const addCustomThemeUrl = usePreferencesStore((s) => s.addCustomThemeUrl);
  const removeCustomThemeUrl = usePreferencesStore((s) => s.removeCustomThemeUrl);
  const [themeImportUrl, setThemeImportUrl] = useState("");
  const [themeImportStatus, setThemeImportStatus] = useState<"idle" | "loading" | "error">("idle");
  const [themeImportError, setThemeImportError] = useState("");

  const boardBackgroundMenuGroups = useMemo(
    () =>
      BOARD_BACKGROUND_GROUPS.map((group) => ({
        label: t(group.labelKey),
        items: group.options.map((bg) => ({
          value: bg.value,
          label: bg.labelKey ? t(bg.labelKey) : (bg.label ?? bg.value),
        })),
      })),
    [t],
  );
  const selectedBoardBackgroundLabel = useMemo(() => {
    for (const group of boardBackgroundMenuGroups) {
      const match = group.items.find((item) => item.value === boardBackground);
      if (match) return match.label;
    }
    return boardBackground;
  }, [boardBackground, boardBackgroundMenuGroups]);
  const audioThemeItems = useMemo(
    () => [
      ...Object.values(BUILT_IN_THEMES).map((theme) => ({
        value: theme.id,
        label: theme.name,
      })),
      ...customThemeUrls.map((theme) => ({
        value: theme.id,
        label: theme.id,
      })),
    ],
    [customThemeUrls],
  );
  const selectedAudioThemeLabel = useMemo(
    () => audioThemeItems.find((item) => item.value === audioThemeId)?.label ?? audioThemeId,
    [audioThemeId, audioThemeItems],
  );

  const handleThemeChange = useCallback(async (id: string) => {
    setAudioThemeId(id);
    try {
      const manifest = await findManifest(id, customThemeUrls);
      await audioManager.loadTheme(manifest);
    } catch {
      // Fallback to planeswalker on failure
      setAudioThemeId("planeswalker");
      await audioManager.loadTheme(PLANESWALKER_THEME);
    }
  }, [setAudioThemeId, customThemeUrls]);

  const handleImportTheme = useCallback(async () => {
    if (!themeImportUrl.trim()) return;
    setThemeImportStatus("loading");
    setThemeImportError("");
    try {
      const response = await fetch(themeImportUrl.trim());
      const json: unknown = await response.json();
      const result = validateThemeManifest(json);
      if (result instanceof Error) throw result;
      addCustomThemeUrl(result.id, themeImportUrl.trim());
      await cacheThemeManifest(result.id, result);
      setThemeImportUrl("");
      setThemeImportStatus("idle");
    } catch (err) {
      setThemeImportError(err instanceof Error ? err.message : t("audioTheme.importFailed"));
      setThemeImportStatus("error");
    }
  }, [themeImportUrl, addCustomThemeUrl, t]);

  const handleRemoveTheme = useCallback(async (id: string) => {
    removeCustomThemeUrl(id);
    await clearThemeCache(id);
    if (audioThemeId === id) {
      await audioManager.loadTheme(PLANESWALKER_THEME);
    }
  }, [removeCustomThemeUrl, audioThemeId]);

  // Multiplayer settings — server picking lives in `ServerPicker` (opened
  // from the lobby header in either server or P2P mode), not here.
  const displayName = useMultiplayerStore((s) => s.displayName);
  const setDisplayName = useMultiplayerStore((s) => s.setDisplayName);
  const [activeTab, setActiveTab] = useState<SettingsTabId>(initialTab);

  return (
    <ModalPanelShell
      title={t("modal.title")}
      subtitle={t("modal.subtitle")}
      onClose={onClose}
      maxWidthClassName="max-w-5xl"
      bodyClassName="flex min-h-0 flex-1 flex-col overflow-hidden pl-4 pt-4 pr-1.5 pb-8 sm:pl-6 sm:pt-6 sm:pr-2 sm:pb-10"
    >
      <div className="flex min-h-0 flex-1 flex-col gap-4 md:min-h-[28rem] md:flex-row md:overflow-hidden">
            <aside className="flex shrink-0 flex-col md:w-[200px] md:justify-between">
              <nav className="flex snap-x gap-2 overflow-x-auto pb-1 md:flex-col md:overflow-visible md:pb-0">
                {SETTINGS_TABS.map((tab) => (
                  <button
                    key={tab.id}
                    onClick={() => setActiveTab(tab.id)}
                    className={`min-h-11 shrink-0 snap-start rounded-[16px] border px-3 py-2.5 text-left text-[11px] font-semibold uppercase tracking-[0.16em] transition-colors md:w-full md:px-4 md:text-xs md:tracking-[0.18em] ${
                      activeTab === tab.id
                        ? "border-sky-400/60 bg-sky-500/14 text-sky-100"
                        : "border-white/8 bg-black/20 text-slate-400 hover:border-white/14 hover:text-slate-100"
                    }`}
                  >
                    {t(`tabs.${tab.id}`)}
                  </button>
                ))}
              </nav>
              <div className="hidden shrink-0 border-t border-white/5 pt-6 pb-8 md:block">
                <ResetAllFooter resetAllPreferences={resetAllPreferences} />
              </div>
            </aside>

            <div className="thin-scrollbar flex min-h-0 min-w-0 flex-1 flex-col gap-4 overflow-y-auto pb-4 pr-3 md:pr-4">
              {activeTab === "gameplay" && (
                <SettingsSection title={t("gameplay.title")}>
                  <SettingGroup label={t("gameplay.language")}>
                    <div className="flex flex-wrap gap-2">
                      {LANGUAGE_OPTIONS.map((opt) => {
                        const selected = opt.value === language;
                        return (
                          <button
                            key={opt.value}
                            type="button"
                            onClick={() => setLanguage(opt.value)}
                            aria-pressed={selected}
                            aria-label={opt.label}
                            className={`flex min-h-11 items-center gap-2 rounded-[14px] border px-3 py-2 text-sm transition-colors ${
                              selected
                                ? "border-sky-400/60 bg-sky-400/15 text-white"
                                : "border-white/10 bg-black/18 text-slate-200 hover:border-white/25 hover:text-white"
                            }`}
                          >
                            <LanguageFlag lng={opt.value} className="h-4 w-6 shrink-0 rounded-sm" />
                            <span>{opt.label}</span>
                          </button>
                        );
                      })}
                    </div>
                  </SettingGroup>

                  <SettingGroup label={t("gameplay.cardSize")}>
                    <SegmentedControl
                      options={CARD_SIZES}
                      value={cardSize}
                      onChange={setCardSize}
                      renderLabel={(opt) => t(`gameplay.cardSizeOptions.${opt}`)}
                    />
                  </SettingGroup>

                  <SettingGroup label={t("gameplay.autoPass")}>
                    <label className="flex min-h-11 items-start gap-2">
                      <input
                        type="checkbox"
                        checked={priorityPassingMode === "SkipLowUseWindows"}
                        onChange={(event) => {
                          setPriorityPassingMode(
                            event.target.checked ? "SkipLowUseWindows" : "Standard",
                          );
                        }}
                        className="mt-1 accent-cyan-500"
                      />
                      <span className="text-sm text-slate-200">
                        {t("gameplay.skipLowUsePriorityWindows")}
                        <span className="mt-0.5 block text-xs leading-relaxed text-slate-400">
                          {t("gameplay.skipLowUsePriorityWindowsDescription")}
                        </span>
                      </span>
                    </label>
                  </SettingGroup>

                  <SettingGroup label={t("gameplay.commandZone")}>
                    <SegmentedControl
                      options={COMMAND_ZONE_DISPLAYS}
                      value={commandZoneDisplay}
                      onChange={setCommandZoneDisplay}
                      renderLabel={(opt) => t(`gameplay.commandZoneOptions.${opt}`)}
                    />
                  </SettingGroup>

                  <SettingGroup label={t("gameplay.collapseLands")}>
                    <SegmentedControl
                      options={ZONE_COLLAPSE_MODES}
                      value={collapseLands}
                      onChange={setCollapseLands}
                      renderLabel={(opt) => t(`gameplay.collapseZoneOptions.${opt}`)}
                    />
                  </SettingGroup>

                  <SettingGroup label={t("gameplay.collapseSupport")}>
                    <SegmentedControl
                      options={ZONE_COLLAPSE_MODES}
                      value={collapseSupport}
                      onChange={setCollapseSupport}
                      renderLabel={(opt) => t(`gameplay.collapseZoneOptions.${opt}`)}
                    />
                  </SettingGroup>

                  <SettingGroup label={t("gameplay.logDefault")}>
                    <SegmentedControl
                      options={LOG_DEFAULTS}
                      value={logDefaultState}
                      onChange={setLogDefaultState}
                      renderLabel={(opt) => t(`gameplay.logDefaultOptions.${opt}`)}
                    />
                  </SettingGroup>

                  <SettingGroup label={t("gameplay.spellPayment")}>
                    <label className="flex min-h-11 items-center gap-2">
                      <input
                        type="checkbox"
                        checked={spellPaymentMode === "manual"}
                        onChange={(e) => setSpellPaymentMode(e.target.checked ? "manual" : "auto")}
                        className="accent-cyan-500"
                      />
                      <span className="text-sm text-slate-200">{t("gameplay.manualManaPayment")}</span>
                    </label>
                  </SettingGroup>

                  {isTauri() && (
                    <label className="mt-1 flex min-h-11 items-start gap-2">
                      <input
                        type="checkbox"
                        checked={nativeEngineEnabled}
                        onChange={(e) => setNativeEngineEnabled(e.target.checked)}
                        className="mt-1 accent-cyan-500"
                      />
                      <span className="text-sm text-slate-200">
                        {t("gameplay.nativeEngine")}
                        <span className="mt-0.5 block text-xs text-slate-400">
                          {t("gameplay.nativeEngineDescription")}
                        </span>
                      </span>
                    </label>
                  )}

                  <div
                    ref={boardBackgroundRef}
                    className={`-m-1 rounded-[16px] p-1 transition-shadow duration-500 ${
                      highlightFlash
                        ? "shadow-[0_0_0_2px_rgba(56,189,248,0.8),0_0_24px_rgba(56,189,248,0.35)]"
                        : ""
                    }`}
                  >
                    <SettingGroup label={t("gameplay.boardBackground")}>
                      <MenuSelect
                        ariaLabel={t("gameplay.boardBackground")}
                        label={selectedBoardBackgroundLabel}
                        selectedValue={boardBackground}
                        groups={boardBackgroundMenuGroups}
                        onSelect={setBoardBackground}
                        menuLayout="dropdown"
                        wrapperClassName="w-full"
                        className={SETTINGS_MENU_CLASS}
                      />
                      {boardBackground === "custom" && (
                        <input
                          type="url"
                          value={customBackgroundUrl}
                          onChange={(e) => setCustomBackgroundUrl(e.target.value)}
                          placeholder="https://example.com/image.jpg"
                          className="mt-2 w-full rounded-[14px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-slate-100 placeholder:text-slate-500 focus:border-sky-400/40 focus:outline-none"
                        />
                      )}
                    </SettingGroup>
                  </div>
                </SettingsSection>
              )}

              {activeTab === "visual" && (
                <SettingsSection title={t("visual.title")}>
                  <SettingGroup label={t("visual.vfxQuality")}>
                    <SegmentedControl
                      options={VFX_QUALITIES}
                      value={vfxQuality}
                      onChange={setVfxQuality}
                      renderLabel={(opt) => t(`visual.vfxQualityOptions.${opt}`)}
                    />
                  </SettingGroup>

                  <SettingGroup label={t("visual.keywordStrip")}>
                    <label className="flex min-h-11 items-center gap-2">
                      <input
                        type="checkbox"
                        checked={showKeywordStrip}
                        onChange={(e) => setShowKeywordStrip(e.target.checked)}
                        className="accent-cyan-500"
                      />
                      <span className="text-sm text-slate-200">{t("visual.showKeywords")}</span>
                    </label>
                  </SettingGroup>

                  <SettingGroup label={t("visual.opponentHoverPreview")}>
                    <label className="flex min-h-11 items-center gap-2">
                      <input
                        type="checkbox"
                        checked={battlefieldPeekOnHover}
                        onChange={(e) => setBattlefieldPeekOnHover(e.target.checked)}
                        className="accent-cyan-500"
                      />
                      <span className="text-sm text-slate-200">{t("visual.showOpponentBoard")}</span>
                    </label>
                  </SettingGroup>

                  <SettingGroup label={t("visual.multiplayerBoardLayout")}>
                    <SegmentedControl
                      options={MULTIPLAYER_BOARD_LAYOUTS}
                      value={multiplayerBoardLayout}
                      onChange={setMultiplayerBoardLayout}
                      renderLabel={(opt) => t(`visual.multiplayerBoardLayoutOptions.${opt}`)}
                    />
                  </SettingGroup>

                  <SettingGroup label={t("visual.cardPreview")}>
                    <SegmentedControl
                      options={CARD_PREVIEW_MODES}
                      value={cardPreviewMode}
                      onChange={setCardPreviewMode}
                      renderLabel={(opt) => t(`visual.cardPreviewOptions.${opt}`)}
                    />
                    <p className="mt-1.5 text-xs text-slate-400">
                      {t(`visual.cardPreviewHint.${cardPreviewMode}`)}
                    </p>
                  </SettingGroup>

                  {/* Hover latency only applies to the hover-driven modes; the
                      "shift" bind-key mode is keypress-triggered, so the control
                      is mutually exclusive with it and hidden in that mode. */}
                  {cardPreviewMode !== "shift" && (
                    <SettingGroup label={t("visual.cardPreviewDelay")}>
                      <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
                        <input
                          type="range"
                          min={CARD_PREVIEW_HOVER_DELAY_MIN}
                          max={CARD_PREVIEW_HOVER_DELAY_MAX}
                          step={CARD_PREVIEW_HOVER_DELAY_STEP}
                          value={cardPreviewHoverDelayMs}
                          onChange={(e) => setCardPreviewHoverDelayMs(Number(e.target.value))}
                          aria-label={t("visual.cardPreviewDelay")}
                          className="flex-1 accent-cyan-500"
                        />
                        <span className="font-mono text-xs tabular-nums text-slate-400 sm:w-20 sm:text-right">
                          {cardPreviewHoverDelayMs === 0
                            ? t("visual.cardPreviewDelayInstant")
                            : t("visual.cardPreviewDelayValue", { ms: cardPreviewHoverDelayMs })}
                        </span>
                      </div>
                      <p className="mt-1.5 text-xs text-slate-400">
                        {t("visual.cardPreviewDelayHint")}
                      </p>
                    </SettingGroup>
                  )}

                  <SettingGroup label={t("visual.cardPreviewFooter")}>
                    <label className="flex min-h-11 items-center gap-2">
                      <input
                        type="checkbox"
                        checked={showCardPreviewFooter}
                        onChange={(e) => setShowCardPreviewFooter(e.target.checked)}
                        className="accent-cyan-500"
                      />
                      <span className="text-sm text-slate-200">
                        {t("visual.showCardPreviewFooter")}
                      </span>
                    </label>
                    <p className="mt-1.5 text-xs text-slate-400">
                      {t("visual.cardPreviewFooterHint")}
                    </p>
                  </SettingGroup>

                  <SettingGroup label={t("visual.cardArtPreferences")}>
                    <ArtChainEditor
                      chain={artChain}
                      onAdd={addArtChainEntry}
                      onRemove={removeArtChainEntry}
                      onMove={moveArtChainEntry}
                    />
                    {artOverrideCount > 0 && (
                      <ClearArtOverridesButton
                        count={artOverrideCount}
                        onClear={clearAllArtOverrides}
                      />
                    )}
                  </SettingGroup>

                  <SettingGroup label={t("flexLayout.title")}>
                    <p className="mb-2 text-xs text-slate-400">
                      {t("flexLayout.description")}
                    </p>
                    <button
                      type="button"
                      onClick={() => {
                        // Launch edit mode and close settings so the board is
                        // visible; the overlay toolbar owns presets/reset/done.
                        setFlexEditMode(true);
                        onClose();
                      }}
                      className="rounded-[14px] border border-white/10 bg-white/5 px-3 py-1.5 text-xs text-slate-200 transition hover:bg-white/10"
                    >
                      {t("flexLayout.edit")}
                    </button>
                  </SettingGroup>
                </SettingsSection>
              )}

              {activeTab === "combat" && (
                <PacingSection
                  animationSpeedMultiplier={animationSpeedMultiplier}
                  setAnimationSpeedMultiplier={setAnimationSpeedMultiplier}
                  pacingMultipliers={pacingMultipliers}
                  setPacingMultiplier={setPacingMultiplier}
                  resetPacing={resetPacing}
                />
              )}

              {activeTab === "audio" && (<>
                <SettingsSection title={t("audio.title")}>
                  <SettingGroup label={t("audio.muteAll")}>
                    <label className="flex min-h-11 items-center gap-2">
                      <input
                        type="checkbox"
                        checked={masterMuted}
                        onChange={(e) => {
                          setMasterMuted(e.target.checked);
                          if (!e.target.checked) audioManager.ensurePlayback();
                        }}
                        className="accent-cyan-500"
                      />
                      <span className="text-sm text-slate-200">{t("audio.muteAllAudio")}</span>
                    </label>
                  </SettingGroup>

                  <SettingGroup label={t("audio.globalVolume")}>
                    <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
                      <input
                        type="range"
                        min={0}
                        max={100}
                        value={masterVolume}
                        onChange={(e) => setMasterVolume(Number(e.target.value))}
                        className="flex-1 accent-cyan-500"
                      />
                      <span className="text-xs text-slate-400 sm:w-10 sm:text-right">{masterVolume}%</span>
                    </div>
                  </SettingGroup>

                  <SettingGroup label={t("audio.sfxVolume")}>
                    <div className={`flex flex-col gap-2 sm:flex-row sm:items-center ${masterMuted ? "opacity-50" : ""}`}>
                      <input
                        type="range"
                        min={0}
                        max={100}
                        value={sfxVolume}
                        onChange={(e) => setSfxVolume(Number(e.target.value))}
                        className="flex-1 accent-cyan-500"
                      />
                      <span className="text-xs text-slate-400 sm:w-10 sm:text-right">{sfxVolume}%</span>
                    </div>
                  </SettingGroup>

                  <SettingGroup label={t("audio.musicVolume")}>
                    <div className={`flex flex-col gap-2 sm:flex-row sm:items-center ${masterMuted ? "opacity-50" : ""}`}>
                      <input
                        type="range"
                        min={0}
                        max={100}
                        value={musicVolume}
                        onChange={(e) => setMusicVolume(Number(e.target.value))}
                        className="flex-1 accent-cyan-500"
                      />
                      <span className="text-xs text-slate-400 sm:w-10 sm:text-right">{musicVolume}%</span>
                    </div>
                  </SettingGroup>
                </SettingsSection>

                <SettingsSection title={t("audioTheme.title")}>
                  <SettingGroup label={t("audioTheme.theme")}>
                    <MenuSelect
                      ariaLabel={t("audioTheme.theme")}
                      label={selectedAudioThemeLabel}
                      selectedValue={audioThemeId}
                      items={audioThemeItems}
                      onSelect={handleThemeChange}
                      menuLayout="dropdown"
                      wrapperClassName="w-full"
                      className={SETTINGS_MENU_CLASS}
                    />
                  </SettingGroup>

                  <SettingGroup label={t("audioTheme.importTheme")}>
                    <div className="flex flex-col gap-2">
                      <div className="flex gap-2">
                        <input
                          type="text"
                          value={themeImportUrl}
                          onChange={(e) => setThemeImportUrl(e.target.value)}
                          placeholder="https://example.com/theme.json"
                          className="min-h-11 flex-1 rounded-[14px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-slate-100 placeholder-slate-500 focus:border-sky-400/40 focus:outline-none"
                        />
                        <button
                          type="button"
                          onClick={handleImportTheme}
                          disabled={themeImportStatus === "loading" || !themeImportUrl.trim()}
                          className="rounded-[14px] border border-white/10 bg-sky-600/30 px-4 py-2 text-sm text-slate-100 hover:bg-sky-600/50 disabled:opacity-50"
                        >
                          {themeImportStatus === "loading" ? t("audioTheme.loading") : t("audioTheme.import")}
                        </button>
                      </div>
                      {themeImportStatus === "error" && (
                        <p className="text-xs text-red-400">{themeImportError}</p>
                      )}
                    </div>
                  </SettingGroup>

                  {customThemeUrls.length > 0 && (
                    <SettingGroup label={t("audioTheme.customThemes")}>
                      <div className="flex flex-col gap-1">
                        {customThemeUrls.map((theme) => (
                          <div key={theme.id} className="flex items-center justify-between rounded-lg bg-black/20 px-3 py-2">
                            <span className="text-sm text-slate-300">{theme.id}</span>
                            <button
                              type="button"
                              onClick={() => handleRemoveTheme(theme.id)}
                              className="text-xs text-red-400 hover:text-red-300"
                            >
                              {t("audioTheme.remove")}
                            </button>
                          </div>
                        ))}
                      </div>
                    </SettingGroup>
                  )}
                </SettingsSection>
              </>)}

              {activeTab === "multiplayer" && (
                <SettingsSection title={t("multiplayer.title")}>
                  <SettingGroup label={t("multiplayer.displayName")}>
                      <input
                        type="text"
                        value={displayName}
                        onChange={(e) => setDisplayName(e.target.value)}
                        placeholder={t("multiplayer.displayNamePlaceholder")}
                        maxLength={20}
                        className="w-full rounded-[14px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-slate-100 placeholder-slate-500 focus:border-sky-400/40 focus:outline-none"
                      />
                  </SettingGroup>

                  <p className="text-xs text-slate-400">
                    {t("multiplayer.serverSelectionNote")}
                  </p>
                </SettingsSection>
              )}

              {activeTab === "data" && (
        <>
          <CloudSyncSection />
          <DataSection />
        </>
      )}
              <div className="border-t border-white/5 py-4 md:hidden">
                <ResetAllFooter resetAllPreferences={resetAllPreferences} />
              </div>
            </div>
          </div>
    </ModalPanelShell>
  );
}

function ClearArtOverridesButton({
  count,
  onClear,
}: {
  count: number;
  onClear: () => void;
}) {
  const { t } = useTranslation("settings");
  const [confirmOpen, setConfirmOpen] = useState(false);

  const onConfirm = useCallback(() => {
    onClear();
    setConfirmOpen(false);
  }, [onClear]);

  return (
    <>
      <button
        type="button"
        onClick={() => setConfirmOpen(true)}
        className="mt-2 rounded-[14px] border border-white/10 bg-white/5 px-3 py-1.5 text-xs text-slate-200 transition hover:bg-white/10"
      >
        {t("visual.clearArtOverrides", { count })}
      </button>
      <ConfirmDialog
        open={confirmOpen}
        title={t("visual.clearArtOverrides", { count })}
        message={t("visual.clearArtOverridesConfirm", { count })}
        confirmLabel={t("visual.clearArtOverridesAction")}
        onConfirm={onConfirm}
        onCancel={() => setConfirmOpen(false)}
        tone="danger"
      />
    </>
  );
}

/** Discreet trailing footer with a single "Reset to defaults" action that
 *  wipes the entire preferences store back to factory defaults. Confirms
 *  before firing — this clears AI seats, board background, audio levels,
 *  and every pacing slider, which is rarely what someone means accidentally. */
function ResetAllFooter({
  resetAllPreferences,
}: {
  resetAllPreferences: () => void;
}) {
  const { t } = useTranslation("settings");
  const [confirmOpen, setConfirmOpen] = useState(false);

  const onConfirm = useCallback(() => {
    resetAllPreferences();
    setConfirmOpen(false);
  }, [resetAllPreferences]);

  return (
    <>
      <button
        type="button"
        onClick={() => setConfirmOpen(true)}
        className="text-xs font-medium uppercase tracking-[0.18em] text-slate-500 transition-colors hover:text-rose-300"
      >
        {t("resetAll.button")}
      </button>
      <ConfirmDialog
        open={confirmOpen}
        title={t("resetAll.button")}
        message={t("resetAll.confirm")}
        confirmLabel={t("resetAll.confirmAction")}
        onConfirm={onConfirm}
        onCancel={() => setConfirmOpen(false)}
        tone="danger"
      />
    </>
  );
}

function SyncSpinner() {
  return (
    <svg
      className="h-3.5 w-3.5 animate-spin"
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
    >
      <circle
        className="opacity-25"
        cx="12"
        cy="12"
        r="10"
        stroke="currentColor"
        strokeWidth="4"
      />
      <path
        className="opacity-75"
        fill="currentColor"
        d="M4 12a8 8 0 0 1 8-8v4a4 4 0 0 0-4 4H4z"
      />
    </svg>
  );
}

const SYNC_BUTTON_CLASS =
  "rounded-[14px] border border-white/10 bg-white/5 px-4 py-2 text-sm font-medium text-slate-100 transition hover:bg-white/10 disabled:cursor-not-allowed disabled:opacity-50";

function CloudSyncSection() {
  const { t } = useTranslation("settings");
  const available = useCloudSyncStore((s) => s.available);
  const identity = useCloudSyncStore((s) => s.identity);
  const sessionResolved = useCloudSyncStore((s) => s.sessionResolved);
  const status = useCloudSyncStore((s) => s.status);
  const error = useCloudSyncStore((s) => s.error);
  const lastSyncedAt = useCloudSyncStore((s) => s.lastSyncedAt);
  const conflict = useCloudSyncStore((s) => s.conflict);
  const conflictDiff = useCloudSyncStore((s) => s.conflictDiff);
  const signIn = useCloudSyncStore((s) => s.signIn);
  const signOut = useCloudSyncStore((s) => s.signOut);
  const syncNow = useCloudSyncStore((s) => s.syncNow);
  const resolveConflict = useCloudSyncStore((s) => s.resolveConflict);

  // Hidden entirely on deployments without a configured provider (self-hosters),
  // who keep file backup as their data-portability path.
  if (!available) return null;

  const syncing = status === "syncing";

  const statusLine =
    status === "error" ? (
      <span className="text-rose-400">
        {t("sync.statusError")}
        {error ? `: ${error}` : ""}
      </span>
    ) : syncing ? (
      t("sync.statusSyncing")
    ) : (
      t("sync.lastSynced", {
        time: lastSyncedAt
          ? new Date(lastSyncedAt).toLocaleString()
          : t("sync.never"),
      })
    );

  return (
    <SettingsSection title={t("sync.title")}>
      <p className="text-xs text-slate-400">{t("sync.description")}</p>
      <p className="text-xs text-slate-500">{t("sync.savesNote")}</p>

      {!sessionResolved ? (
        // Session restore in flight — withhold the sign-in CTA so a signed-in
        // user doesn't see the prompt flash before identity adopts.
        <p className="text-xs text-slate-500">{t("sync.statusSyncing")}</p>
      ) : !identity ? (
        <div className="flex flex-wrap gap-2">
          <button
            className={SYNC_BUTTON_CLASS}
            onClick={() => void signIn("discord")}
          >
            <span className="flex items-center gap-2">
              <DiscordIcon className="h-4 w-4" />
              {t("sync.signInWith", { provider: t("sync.providerDiscord") })}
            </span>
          </button>
          <button
            className={SYNC_BUTTON_CLASS}
            onClick={() => void signIn("google")}
          >
            <span className="flex items-center gap-2">
              <GoogleIcon className="h-4 w-4" />
              {t("sync.signInWith", { provider: t("sync.providerGoogle") })}
            </span>
          </button>
        </div>
      ) : (
        <div className="flex flex-col gap-3">
          <div className="flex items-center gap-2">
            {identity.avatarUrl && (
              <img
                src={identity.avatarUrl}
                alt=""
                className="h-6 w-6 rounded-full"
                referrerPolicy="no-referrer"
              />
            )}
            <span className="text-sm text-slate-200">
              {t("sync.signedInAs", { name: identity.label })}
            </span>
          </div>

          {conflict ? (
            <div className="flex flex-col gap-2 rounded-[14px] border border-amber-400/30 bg-amber-400/10 p-3">
              <p className="text-sm font-medium text-amber-200">
                {t("sync.conflictTitle")}
              </p>
              <p className="text-xs text-amber-100/80">
                {t("sync.conflictBody")}
              </p>
              {conflictDiff && (
                <ul className="space-y-0.5 text-xs text-amber-100/70">
                  {(conflictDiff.decksAdded > 0 ||
                    conflictDiff.decksModified > 0 ||
                    conflictDiff.decksRemoved > 0) && (
                    <li>
                      {t("sync.diffDecks", {
                        added: conflictDiff.decksAdded,
                        modified: conflictDiff.decksModified,
                        removed: conflictDiff.decksRemoved,
                      })}
                    </li>
                  )}
                  {conflictDiff.prefsChanged && <li>{t("sync.diffPrefs")}</li>}
                  {conflictDiff.feedsChanged && <li>{t("sync.diffFeeds")}</li>}
                  {conflictDiff.otherChanged && <li>{t("sync.diffOther")}</li>}
                </ul>
              )}
              <div className="flex flex-wrap gap-2">
                <button
                  className={SYNC_BUTTON_CLASS}
                  onClick={() => void resolveConflict("cloud")}
                >
                  {t("sync.keepCloud")}
                </button>
                <button
                  className={SYNC_BUTTON_CLASS}
                  onClick={() => void resolveConflict("local")}
                >
                  {t("sync.keepLocal")}
                </button>
              </div>
            </div>
          ) : (
            <div className="flex flex-wrap items-center gap-2">
              <button
                className={SYNC_BUTTON_CLASS}
                disabled={syncing}
                onClick={() => void syncNow()}
              >
                <span className="flex items-center gap-2">
                  {syncing && <SyncSpinner />}
                  {t("sync.syncNow")}
                </span>
              </button>
              <button
                className={SYNC_BUTTON_CLASS}
                onClick={() => void signOut()}
              >
                {t("sync.signOut")}
              </button>
            </div>
          )}

          <p className="text-xs text-slate-500">{statusLine}</p>
        </div>
      )}
    </SettingsSection>
  );
}

function DataSection() {
  const { t } = useTranslation("settings");
  const telemetryEnabled = usePreferencesStore((s) => s.telemetryEnabled);
  const setTelemetryEnabled = usePreferencesStore((s) => s.setTelemetryEnabled);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pendingImportFile, setPendingImportFile] = useState<File | null>(null);

  const onExport = useCallback(() => {
    setError(null);
    try {
      downloadBackup();
      setStatus(t("data.backupDownloaded"));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [t]);

  const onImport = useCallback(
    async (file: File, mode: ImportMode) => {
      setError(null);
      setStatus(null);
      try {
        const result = await importBackupFromFile(file, mode);
        const base = result.preferencesReplaced
          ? t("data.importedWithPreferences", { count: result.decksImported })
          : t("data.imported", { count: result.decksImported });
        const malformedSuffix =
          result.malformedKeys.length > 0
            ? " " + t("data.skippedMalformed", { count: result.malformedKeys.length })
            : "";
        setStatus(base + malformedSuffix);
        // Zustand stores read from localStorage at boot — reload so every
        // subscriber picks up the restored data instead of holding stale state.
        setTimeout(() => {
          window.location.reload();
        }, 600);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [t],
  );

  const dismissImportDialog = useCallback(() => {
    setPendingImportFile(null);
  }, []);

  const confirmImport = useCallback(
    (mode: ImportMode) => {
      if (!pendingImportFile) return;
      void onImport(pendingImportFile, mode);
      setPendingImportFile(null);
    },
    [onImport, pendingImportFile],
  );

  return (
    <SettingsSection title={t("data.title")}>
      <p className="text-xs text-slate-400">
        {t("data.description")}
      </p>
      <div className="flex flex-wrap gap-2">
        <button
          onClick={onExport}
          className="rounded-[14px] border border-white/10 bg-white/5 px-4 py-2 text-sm font-medium text-slate-100 transition hover:bg-white/10"
        >
          {t("data.exportBackup")}
        </button>
        <button
          onClick={() => {
            fileInputRef.current?.click();
          }}
          className="rounded-[14px] border border-white/10 bg-white/5 px-4 py-2 text-sm font-medium text-slate-100 transition hover:bg-white/10"
        >
          {t("data.importBackup")}
        </button>
      </div>
      <input
        ref={fileInputRef}
        type="file"
        accept="application/json,.json"
        className="hidden"
        onChange={(e) => {
          const file = e.target.files?.[0];
          e.target.value = "";
          if (!file) return;
          setPendingImportFile(file);
        }}
      />
      <ConfirmDialog
        open={pendingImportFile != null}
        title={t("data.importModeTitle")}
        message={t("data.importModeMessage")}
        confirmLabel={t("data.importOverwrite")}
        secondaryConfirmLabel={t("data.importMerge")}
        onConfirm={() => confirmImport("overwrite")}
        onSecondaryConfirm={() => confirmImport("merge")}
        onCancel={dismissImportDialog}
        tone="danger"
        secondaryTone="primary"
      />
      {status && <p className="text-xs text-emerald-400">{status}</p>}
      {error && <p className="text-xs text-rose-400">{error}</p>}
      <label className="mt-1 flex min-h-11 items-start gap-2">
        <input
          type="checkbox"
          checked={telemetryEnabled}
          onChange={(e) => setTelemetryEnabled(e.target.checked)}
          className="mt-1 accent-cyan-500"
        />
        <span className="text-sm text-slate-200">
          {t("data.telemetry")}
          <span className="mt-0.5 block text-xs text-slate-400">{t("data.telemetryDescription")}</span>
        </span>
      </label>
    </SettingsSection>
  );
}

function SettingsSection({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section className="rounded-[20px] border border-white/10 bg-black/18 p-4 shadow-[0_18px_54px_rgba(0,0,0,0.18)] backdrop-blur-md sm:p-5">
      <h3 className="mb-4 text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-slate-500">{title}</h3>
      <div className="flex flex-col gap-4">{children}</div>
    </section>
  );
}

function SettingGroup({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <label className="mb-2 block text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
        {label}
      </label>
      {children}
    </div>
  );
}

/** Single labelled multiplier slider with an inline reset affordance. The
 *  reset button is rendered always (so the layout doesn't shift), but is
 *  visually faded and disabled when the value already equals `defaultValue`.
 *  Aria + tooltip wired so screen readers and hover-help both work. */
function MultiplierSlider({
  label,
  description,
  value,
  defaultValue,
  min,
  max,
  step,
  onChange,
}: {
  label: string;
  description?: string;
  value: number;
  defaultValue: number;
  min: number;
  max: number;
  step: number;
  onChange: (next: number) => void;
}) {
  const { t } = useTranslation("settings");
  const atDefault = Math.abs(value - defaultValue) < 1e-9;
  return (
    <div>
      <div className="mb-2 flex items-baseline justify-between gap-3">
        <label className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
          {label}
        </label>
        <span className="font-mono text-xs tabular-nums text-slate-300">
          {formatSpeed(value, max, { instant: t("pacing.instant"), slowest: t("pacing.slowest") })}
        </span>
      </div>
      <div className="flex items-center gap-2">
        <input
          type="range"
          min={min}
          max={max}
          step={step}
          value={value}
          onChange={(e) => onChange(Number(e.target.value))}
          onDoubleClick={() => onChange(defaultValue)}
          aria-label={label}
          className="flex-1 accent-cyan-500"
        />
        <button
          type="button"
          onClick={() => onChange(defaultValue)}
          disabled={atDefault}
          aria-label={t("pacing.resetSliderLabel", { label })}
          title={atDefault ? t("pacing.atDefault") : t("pacing.resetToDefault")}
          className={`flex h-7 w-7 items-center justify-center rounded-full border border-white/10 bg-black/18 text-slate-300 transition-all ${
            atDefault
              ? "cursor-not-allowed opacity-30"
              : "hover:border-cyan-400/40 hover:text-cyan-200 hover:bg-cyan-400/10"
          }`}
        >
          <svg
            xmlns="http://www.w3.org/2000/svg"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth={2}
            strokeLinecap="round"
            strokeLinejoin="round"
            className="h-3.5 w-3.5"
            aria-hidden="true"
          >
            <path d="M3 12a9 9 0 1 0 3-6.7" />
            <path d="M3 4v5h5" />
          </svg>
        </button>
      </div>
      {description && <p className="mt-2 text-xs text-slate-500">{description}</p>}
    </div>
  );
}

/** Unified pacing panel — global animation speed plus every per-category
 *  multiplier in one place. Each slider has its own reset; the section also
 *  offers a "Reset section" link that resets everything here without
 *  touching unrelated preferences. */
function PacingSection({
  animationSpeedMultiplier,
  setAnimationSpeedMultiplier,
  pacingMultipliers,
  setPacingMultiplier,
  resetPacing,
}: {
  animationSpeedMultiplier: number;
  setAnimationSpeedMultiplier: (n: number) => void;
  pacingMultipliers: Record<PacingCategory, number>;
  setPacingMultiplier: (category: PacingCategory, n: number) => void;
  resetPacing: () => void;
}) {
  const { t } = useTranslation("settings");
  const allAtDefault =
    Math.abs(animationSpeedMultiplier - ANIMATION_SPEED_DEFAULT) < 1e-9 &&
    PACING_CATEGORIES.every(
      (c) => Math.abs(pacingMultipliers[c] - PACING_DEFAULT) < 1e-9,
    );

  return (
    <section className="rounded-[20px] border border-white/10 bg-black/18 p-4 shadow-[0_18px_54px_rgba(0,0,0,0.18)] backdrop-blur-md sm:p-5">
      <header className="mb-4 flex items-center justify-between">
        <h3 className="text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-slate-500">
          {t("pacing.title")}
        </h3>
        <button
          type="button"
          onClick={resetPacing}
          disabled={allAtDefault}
          className={`text-[0.62rem] font-semibold uppercase tracking-[0.18em] transition-colors ${
            allAtDefault
              ? "cursor-not-allowed text-slate-700"
              : "text-slate-500 hover:text-cyan-200"
          }`}
        >
          {t("pacing.resetSection")}
        </button>
      </header>

      <div className="flex flex-col gap-5">
        <MultiplierSlider
          label={t("pacing.animationSpeed")}
          description={t("pacing.animationSpeedDescription")}
          value={ANIMATION_SPEED_MAX - animationSpeedMultiplier}
          defaultValue={ANIMATION_SPEED_MAX - ANIMATION_SPEED_DEFAULT}
          min={ANIMATION_SPEED_MIN}
          max={ANIMATION_SPEED_MAX}
          step={ANIMATION_SPEED_STEP}
          onChange={(speed) => setAnimationSpeedMultiplier(ANIMATION_SPEED_MAX - speed)}
        />

        {PACING_CATEGORIES.map((category) => (
          <MultiplierSlider
            key={category}
            label={t(`pacing.labels.${category}`)}
            description={t(`pacing.descriptions.${category}`)}
            value={PACING_MAX - pacingMultipliers[category]}
            defaultValue={PACING_MAX - PACING_DEFAULT}
            min={PACING_MIN}
            max={PACING_MAX}
            step={PACING_STEP}
            onChange={(speed) => setPacingMultiplier(category, PACING_MAX - speed)}
          />
        ))}
      </div>

      <p className="mt-4 text-[0.68rem] leading-relaxed text-slate-500">
        <Trans
          t={t}
          i18nKey="pacing.hint"
          components={{ glyph: <span className="text-slate-300" /> }}
        />
      </p>
    </section>
  );
}

const ART_CHAIN_RULE_OPTIONS: { type: ArtChainEntry["type"]; labelKey: string }[] = [
  { type: "source_printing", labelKey: "artChain.rules.sourcePrinting" },
  { type: "newest", labelKey: "artChain.rules.newest" },
  { type: "oldest", labelKey: "artChain.rules.oldest" },
  { type: "prefer_borderless", labelKey: "artChain.rules.preferBorderless" },
  { type: "prefer_extended", labelKey: "artChain.rules.preferExtended" },
];

interface ScryfallSetInfo {
  name: string;
  icon_svg_uri: string;
  released_at: string;
}

function artChainEntryLabel(entry: ArtChainEntry, t: TFunction<"settings">): string {
  switch (entry.type) {
    // `entry.label` is the Scryfall set name (engine/asset data) — left raw.
    case "set": return t("artChain.setEntry", { name: entry.label, code: entry.setCode.toUpperCase() });
    case "newest": return t("artChain.rules.newest");
    case "oldest": return t("artChain.rules.oldest");
    case "prefer_borderless": return t("artChain.rules.preferBorderless");
    case "prefer_extended": return t("artChain.rules.preferExtended");
    case "source_printing": return t("artChain.rules.sourcePrinting");
  }
}

function isTerminal(entry: ArtChainEntry): boolean {
  return entry.type === "newest" || entry.type === "oldest";
}

function ArtChainEditor({
  chain,
  onAdd,
  onRemove,
  onMove,
}: {
  chain: ArtChainEntry[];
  onAdd: (entry: ArtChainEntry) => void;
  onRemove: (index: number) => void;
  onMove: (from: number, to: number) => void;
}) {
  const { t } = useTranslation("settings");
  const [setInput, setSetInput] = useState("");
  const [scryfallSets, setScryfallSets] = useState<Record<string, ScryfallSetInfo> | null>(null);

  useEffect(() => {
    fetch(__SCRYFALL_SETS_URL__)
      .then((r) => r.json() as Promise<Record<string, ScryfallSetInfo>>)
      .then(setScryfallSets)
      .catch(() => {});
  }, []);

  const resolveSetCode = useCallback(
    (input: string): { code: string; label: string } | null => {
      if (!scryfallSets) return null;
      const trimmed = input.trim().toLowerCase();
      if (!trimmed) return null;
      if (scryfallSets[trimmed]) {
        return { code: trimmed, label: scryfallSets[trimmed].name };
      }
      const byName = Object.entries(scryfallSets).find(
        ([, info]) => info.name.toLowerCase() === trimmed,
      );
      if (byName) return { code: byName[0], label: byName[1].name };
      return null;
    },
    [scryfallSets],
  );

  const handleAddSet = useCallback(() => {
    const resolved = resolveSetCode(setInput);
    if (resolved) {
      onAdd({ type: "set", setCode: resolved.code, label: resolved.label });
      setSetInput("");
    }
  }, [setInput, resolveSetCode, onAdd]);

  const sortedSets = scryfallSets
    ? Object.entries(scryfallSets)
        .sort(([, a], [, b]) => b.released_at.localeCompare(a.released_at))
    : [];

  const terminalIndex = chain.findIndex(isTerminal);

  return (
    <div className="flex flex-col gap-3">
      {chain.length === 0 && (
        <p className="text-xs text-slate-500">{t("artChain.emptyState")}</p>
      )}

      {chain.length > 0 && (
        <div className="flex flex-col gap-1">
          {chain.map((entry, i) => (
            <div
              key={`${entry.type}-${i}`}
              className={`flex items-center gap-2 rounded-lg px-3 py-2 ${
                terminalIndex >= 0 && i > terminalIndex
                  ? "bg-amber-500/5 opacity-50"
                  : "bg-black/20"
              }`}
            >
              <span className="mr-1 font-mono text-[10px] text-slate-600">{i + 1}</span>
              <span className="flex-1 text-sm text-slate-200">{artChainEntryLabel(entry, t)}</span>
              <button
                type="button"
                onClick={() => onMove(i, i - 1)}
                disabled={i === 0}
                className="text-slate-500 transition hover:text-slate-200 disabled:opacity-30"
                aria-label={t("artChain.moveUp")}
              >
                <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-4 w-4">
                  <path fillRule="evenodd" d="M14.77 12.79a.75.75 0 01-1.06-.02L10 8.832 6.29 12.77a.75.75 0 11-1.08-1.04l4.25-4.5a.75.75 0 011.08 0l4.25 4.5a.75.75 0 01-.02 1.06z" clipRule="evenodd" />
                </svg>
              </button>
              <button
                type="button"
                onClick={() => onMove(i, i + 1)}
                disabled={i === chain.length - 1}
                className="text-slate-500 transition hover:text-slate-200 disabled:opacity-30"
                aria-label={t("artChain.moveDown")}
              >
                <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-4 w-4">
                  <path fillRule="evenodd" d="M5.23 7.21a.75.75 0 011.06.02L10 11.168l3.71-3.938a.75.75 0 111.08 1.04l-4.25 4.5a.75.75 0 01-1.08 0l-4.25-4.5a.75.75 0 01.02-1.06z" clipRule="evenodd" />
                </svg>
              </button>
              <button
                type="button"
                onClick={() => onRemove(i)}
                className="text-slate-500 transition hover:text-red-400"
                aria-label={t("artChain.remove")}
              >
                <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-4 w-4">
                  <path fillRule="evenodd" d="M4.293 4.293a1 1 0 011.414 0L10 8.586l4.293-4.293a1 1 0 111.414 1.414L11.414 10l4.293 4.293a1 1 0 01-1.414 1.414L10 11.414l-4.293 4.293a1 1 0 01-1.414-1.414L8.586 10 4.293 5.707a1 1 0 010-1.414z" clipRule="evenodd" />
                </svg>
              </button>
            </div>
          ))}
          {terminalIndex >= 0 && terminalIndex < chain.length - 1 && (
            <p className="text-[10px] text-amber-400/70">
              {t("artChain.unreachable", { rule: artChainEntryLabel(chain[terminalIndex], t) })}
            </p>
          )}
        </div>
      )}

      <div className="flex flex-col gap-2 rounded-lg border border-white/5 bg-black/10 p-3">
        <span className="text-[10px] font-semibold uppercase tracking-widest text-slate-500">{t("artChain.addRule")}</span>
        <div className="flex flex-wrap gap-2">
          {ART_CHAIN_RULE_OPTIONS.map((opt) => (
            <button
              key={opt.type}
              type="button"
              onClick={() => onAdd({ type: opt.type } as ArtChainEntry)}
              disabled={chain.some((e) => e.type === opt.type)}
              className="rounded-lg border border-white/10 bg-white/5 px-3 py-1.5 text-xs text-slate-200 transition hover:bg-white/10 disabled:opacity-30"
            >
              {t(opt.labelKey)}
            </button>
          ))}
        </div>
        <div className="flex gap-2">
          <div className="relative flex-1">
            <input
              type="text"
              value={setInput}
              onChange={(e) => setSetInput(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleAddSet()}
              placeholder={t("artChain.setInputPlaceholder")}
              list="art-chain-set-list"
              className="w-full rounded-[14px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-slate-100 placeholder:text-slate-500 focus:border-sky-400/40 focus:outline-none"
            />
            {sortedSets.length > 0 && (
              <datalist id="art-chain-set-list">
                {sortedSets.map(([code, info]) => (
                  <option key={code} value={info.name} />
                ))}
              </datalist>
            )}
          </div>
          <button
            type="button"
            onClick={handleAddSet}
            disabled={!resolveSetCode(setInput)}
            className="rounded-[14px] border border-white/10 bg-sky-600/30 px-4 py-2 text-sm text-slate-100 hover:bg-sky-600/50 disabled:opacity-50"
          >
            {t("artChain.addSet")}
          </button>
        </div>
      </div>

      <p className="text-xs text-slate-500">
        {t("artChain.rulesPriorityNote")}
      </p>
    </div>
  );
}

function SegmentedControl<T extends string>({
  options,
  value,
  onChange,
  renderLabel,
}: {
  options: T[];
  value: T;
  onChange: (v: T) => void;
  /** Maps a raw option value to its translated, display-ready label. */
  renderLabel: (opt: T) => string;
}) {
  return (
    <div className="flex min-h-11 flex-wrap rounded-[16px] border border-white/10 bg-black/18 p-1">
      {options.map((opt) => (
        <button
          key={opt}
          onClick={() => onChange(opt)}
          className={`min-h-9 flex-1 rounded-[12px] px-3 py-2 text-xs font-semibold transition-colors ${
            value === opt
              ? "bg-sky-500/80 text-white"
              : "text-slate-400 hover:text-slate-200"
          }`}
        >
          {renderLabel(opt)}
        </button>
      ))}
    </div>
  );
}
